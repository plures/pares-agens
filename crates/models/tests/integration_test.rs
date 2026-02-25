//! Integration tests for `pares-models` using a `wiremock` mock HTTP server.

use std::collections::HashMap;

use serde_json::json;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use pares_models::{
    config::{ProviderConfig, RouterConfig, RoutingRule},
    router::ModelRouter,
    types::{ChatCompletionRequest, ChatMessage, Role, Tool},
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn openai_response(content: &str) -> serde_json::Value {
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    })
}

fn tool_call_response() -> serde_json::Value {
    json!({
        "id": "chatcmpl-tools",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": "gpt-4o",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_abc123",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"location\":\"London\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
}

/// Build SSE lines for a minimal streaming response.
fn streaming_body(chunks: &[&str]) -> String {
    let mut body = String::new();
    for (i, text) in chunks.iter().enumerate() {
        let finish = if i == chunks.len() - 1 { "stop" } else { "null" };
        let _ = finish; // used in finish_reason override below
        let chunk = json!({
            "id": "chatcmpl-stream",
            "object": "chat.completion.chunk",
            "created": 1700000000u64,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": { "content": text },
                "finish_reason": serde_json::Value::Null
            }]
        });
        // Override finish_reason for last chunk
        let mut chunk_obj = chunk.as_object().unwrap().clone();
        if i == chunks.len() - 1 {
            let choices = chunk_obj.get_mut("choices").unwrap().as_array_mut().unwrap();
            choices[0].as_object_mut().unwrap().insert(
                "finish_reason".to_string(),
                json!("stop"),
            );
        }
        body.push_str(&format!("data: {}\n\n", serde_json::to_string(&chunk_obj).unwrap()));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn single_provider_router(server: &MockServer) -> ModelRouter {
    let config = RouterConfig::single(
        "mock",
        ProviderConfig::new(server.uri(), None),
    );
    ModelRouter::new(config)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Basic non-streaming chat completion round-trip.
#[tokio::test]
async fn test_chat_completion_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response("Hello!")))
        .mount(&server)
        .await;

    let router = single_provider_router(&server);
    let req = ChatCompletionRequest::new(
        "gpt-4o",
        vec![ChatMessage::text(Role::User, "Hi")],
    );

    let resp = router.chat(&req).await.expect("chat completion should succeed");
    assert_eq!(resp.choices[0].message.content.as_deref(), Some("Hello!"));
    assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
}

/// API key is forwarded as a `Bearer` token.
#[tokio::test]
async fn test_api_key_forwarded() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response("Authenticated!")))
        .mount(&server)
        .await;

    let config = RouterConfig::single(
        "openai",
        ProviderConfig::new(server.uri(), Some("sk-test".into())),
    );
    let router = ModelRouter::new(config);
    let req = ChatCompletionRequest::new(
        "gpt-4o",
        vec![ChatMessage::text(Role::User, "ping")],
    );

    let resp = router.chat(&req).await.expect("should accept authenticated request");
    assert_eq!(resp.choices[0].message.content.as_deref(), Some("Authenticated!"));
}

/// The client returns an error on a 4xx API response.
#[tokio::test]
async fn test_api_error_4xx() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let router = single_provider_router(&server);
    let req = ChatCompletionRequest::new(
        "gpt-4o",
        vec![ChatMessage::text(Role::User, "hello")],
    );

    let err = router.chat(&req).await.unwrap_err();
    match err {
        pares_models::Error::ApiError { status, .. } => assert_eq!(status, 401),
        other => panic!("unexpected error: {other}"),
    }
}

/// The client returns an error on a 5xx API response.
#[tokio::test]
async fn test_api_error_5xx() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&server)
        .await;

    let router = single_provider_router(&server);
    let req = ChatCompletionRequest::new(
        "gpt-4o",
        vec![ChatMessage::text(Role::User, "hello")],
    );

    let err = router.chat(&req).await.unwrap_err();
    match err {
        pares_models::Error::ApiError { status, .. } => assert_eq!(status, 500),
        other => panic!("unexpected error: {other}"),
    }
}

/// Tool/function-calling: the response carries `tool_calls` instead of text.
#[tokio::test]
async fn test_tool_calling() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call_response()))
        .mount(&server)
        .await;

    let router = single_provider_router(&server);
    let mut req = ChatCompletionRequest::new(
        "gpt-4o",
        vec![ChatMessage::text(Role::User, "What is the weather in London?")],
    );
    req.tools = Some(vec![Tool::function(
        "get_weather",
        "Get the current weather for a city",
        json!({"type":"object","properties":{"location":{"type":"string"}},"required":["location"]}),
    )]);

    let resp = router.chat(&req).await.expect("tool call should succeed");
    let choice = &resp.choices[0];
    assert_eq!(choice.finish_reason.as_deref(), Some("tool_calls"));
    let tool_calls = choice.message.tool_calls.as_ref().expect("tool_calls present");
    assert_eq!(tool_calls[0].function.name, "get_weather");
    assert_eq!(tool_calls[0].id, "call_abc123");
}

/// Routing rules select the correct provider based on model prefix.
#[tokio::test]
async fn test_routing_by_model_prefix() {
    // Two mock servers — one for OpenAI models, one for local models.
    let openai_server = MockServer::start().await;
    let local_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response("from openai")))
        .mount(&openai_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response("from local")))
        .mount(&local_server)
        .await;

    let config = RouterConfig {
        providers: HashMap::from([
            ("openai".to_string(), ProviderConfig::new(openai_server.uri(), Some("sk-key".into()))),
            ("local".to_string(), ProviderConfig::new(local_server.uri(), None)),
        ]),
        rules: vec![
            RoutingRule { model_prefix: Some("gpt-".into()), provider: "openai".into() },
        ],
        default_provider: "local".into(),
    };
    let router = ModelRouter::new(config);

    // "gpt-4o" matches the prefix rule → openai server
    let gpt_req = ChatCompletionRequest::new(
        "gpt-4o",
        vec![ChatMessage::text(Role::User, "hello")],
    );
    let gpt_resp = router.chat(&gpt_req).await.unwrap();
    assert_eq!(gpt_resp.choices[0].message.content.as_deref(), Some("from openai"));

    // "ai/mistral-nemo" does not match → falls back to local server
    let local_req = ChatCompletionRequest::new(
        "ai/mistral-nemo",
        vec![ChatMessage::text(Role::User, "hello")],
    );
    let local_resp = router.chat(&local_req).await.unwrap();
    assert_eq!(local_resp.choices[0].message.content.as_deref(), Some("from local"));
}

/// SSE streaming returns chunks that can be collected into a full response.
#[tokio::test]
async fn test_streaming_chat_completion() {
    use futures_util::StreamExt;

    let server = MockServer::start().await;
    let body = streaming_body(&["Hello", ", ", "world", "!"]);

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let router = single_provider_router(&server);
    let req = ChatCompletionRequest::new(
        "gpt-4o",
        vec![ChatMessage::text(Role::User, "Say hello")],
    );

    let stream = router.chat_stream(&req).await.expect("stream should open");
    let chunks: Vec<_> = stream.collect().await;

    let assembled: String = chunks
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .filter_map(|c| c.choices.first())
        .filter_map(|ch| ch.delta.content.as_deref())
        .collect();

    assert_eq!(assembled, "Hello, world!");
}

/// Unknown provider name produces `Error::ProviderNotFound`.
#[tokio::test]
async fn test_provider_not_found() {
    let config = RouterConfig {
        providers: HashMap::new(),
        rules: vec![],
        default_provider: "ghost".into(),
    };
    let router = ModelRouter::new(config);
    let req = ChatCompletionRequest::new("gpt-4o", vec![ChatMessage::text(Role::User, "hi")]);

    let err = router.chat(&req).await.unwrap_err();
    match err {
        pares_models::Error::ProviderNotFound(name) => assert_eq!(name, "ghost"),
        other => panic!("unexpected: {other}"),
    }
}
