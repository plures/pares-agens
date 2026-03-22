//! P2P inference protocol.
//!
//! Defines the wire types and transport abstraction for sending prompts to
//! remote devices and streaming tokens back.  Actual transport (Hyperswarm,
//! loopback for tests, WebRTC, etc.) is injected via the [`PeerTransport`]
//! trait so this module stays transport-agnostic.

use crate::MeshError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Wire types ────────────────────────────────────────────────────────────────

/// A prompt payload forwarded to a remote device for inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceRequest {
    /// Unique request identifier (UUID-v4 string).
    pub request_id: String,
    /// Target expert identifier (must be loaded on the destination device).
    pub expert_id: String,
    /// The prompt text to run through the model.
    pub prompt: String,
    /// Maximum number of tokens to generate.
    pub max_tokens: u32,
    /// Sampling temperature (0.0 = deterministic, 1.0 = creative).
    pub temperature: f32,
}

/// A single streamed token returned from a remote device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenChunk {
    /// The request this chunk belongs to.
    pub request_id: String,
    /// The token text fragment.
    pub token: String,
    /// `true` when this is the last chunk for the request.
    pub is_final: bool,
}

// ── PeerTransport ─────────────────────────────────────────────────────────────

/// Abstract transport used by [`InferenceProtocol`] to exchange messages with
/// peers.
///
/// Implementors wrap the actual network layer (Hyperswarm DHT, loopback for
/// tests, etc.).  All inference data is end-to-end encrypted at the transport
/// level — no token ever reaches a relay server.
#[async_trait]
pub trait PeerTransport: Send + Sync {
    /// Send an [`InferenceRequest`] to the given device and receive the full
    /// ordered sequence of [`TokenChunk`]s in return.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError`] on transport failure, timeout, or remote error.
    async fn send_request(
        &self,
        device_id: &str,
        request: &InferenceRequest,
    ) -> Result<Vec<TokenChunk>, MeshError>;
}

// ── InferenceProtocol ─────────────────────────────────────────────────────────

/// High-level P2P inference client.
///
/// Serialises [`InferenceRequest`]s and assembles [`TokenChunk`] streams via
/// the injected [`PeerTransport`].  The caller never touches raw bytes.
pub struct InferenceProtocol {
    transport: Box<dyn PeerTransport>,
}

impl InferenceProtocol {
    /// Create a new protocol client backed by the given transport.
    pub fn new(transport: Box<dyn PeerTransport>) -> Self {
        Self { transport }
    }

    /// Run inference on a remote device, collecting all generated tokens into
    /// a single `String`.
    ///
    /// # Errors
    ///
    /// Propagates any transport error as [`MeshError`].
    pub async fn infer(
        &self,
        device_id: &str,
        request: InferenceRequest,
    ) -> Result<String, MeshError> {
        let chunks = self
            .transport
            .send_request(device_id, &request)
            .await?;
        let text = chunks.into_iter().map(|c| c.token).collect();
        Ok(text)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub transport that echoes the prompt back as individual characters.
    struct EchoTransport;

    #[async_trait]
    impl PeerTransport for EchoTransport {
        async fn send_request(
            &self,
            _device_id: &str,
            request: &InferenceRequest,
        ) -> Result<Vec<TokenChunk>, MeshError> {
            let chars: Vec<TokenChunk> = request
                .prompt
                .chars()
                .enumerate()
                .map(|(i, ch)| {
                    let is_final = i == request.prompt.len() - 1;
                    TokenChunk {
                        request_id: request.request_id.clone(),
                        token: ch.to_string(),
                        is_final,
                    }
                })
                .collect();
            Ok(chars)
        }
    }

    /// Stub transport that always returns a transport error.
    struct FailingTransport;

    #[async_trait]
    impl PeerTransport for FailingTransport {
        async fn send_request(
            &self,
            device_id: &str,
            _request: &InferenceRequest,
        ) -> Result<Vec<TokenChunk>, MeshError> {
            Err(MeshError::DeviceUnreachable(device_id.to_string()))
        }
    }

    fn req(prompt: &str) -> InferenceRequest {
        InferenceRequest {
            request_id: "req-1".into(),
            expert_id: "chat-8b".into(),
            prompt: prompt.into(),
            max_tokens: 64,
            temperature: 0.7,
        }
    }

    #[tokio::test]
    async fn infer_assembles_tokens_from_chunks() {
        let protocol = InferenceProtocol::new(Box::new(EchoTransport));
        let result = protocol.infer("dev-1", req("hello")).await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn infer_propagates_transport_error() {
        let protocol = InferenceProtocol::new(Box::new(FailingTransport));
        let err = protocol.infer("dev-1", req("hello")).await.unwrap_err();
        assert!(matches!(err, MeshError::DeviceUnreachable(_)));
    }

    #[test]
    fn inference_request_roundtrips_serde() {
        let r = req("test prompt");
        let json = serde_json::to_string(&r).unwrap();
        let back: InferenceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.prompt, r.prompt);
    }

    #[test]
    fn token_chunk_roundtrips_serde() {
        let chunk = TokenChunk {
            request_id: "r1".into(),
            token: "foo".into(),
            is_final: true,
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let back: TokenChunk = serde_json::from_str(&json).unwrap();
        assert!(back.is_final);
    }
}
