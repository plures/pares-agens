//! SSE (Server-Sent Events) stream parser for OpenAI-compatible streaming responses.

use futures_util::{Stream, StreamExt};
use reqwest::Response;

use crate::{error::Error, types::ChatCompletionChunk};

/// Parse an HTTP response body as a stream of [`ChatCompletionChunk`]s.
///
/// The OpenAI streaming protocol sends newline-delimited `data: <json>` lines,
/// terminated by `data: [DONE]`.
pub fn parse_sse_stream(
    response: Response,
) -> impl Stream<Item = Result<ChatCompletionChunk, Error>> {
    let byte_stream = response.bytes_stream();

    // Buffer incomplete lines across chunk boundaries.
    let buffer = String::new();

    futures_util::stream::unfold((byte_stream, buffer), |(mut stream, mut buf)| async move {
        loop {
            // Try to yield from whatever is already in the buffer.
            if let Some(result) = consume_line(&mut buf) {
                return Some((result, (stream, buf)));
            }

            // Need more bytes.
            match stream.next().await {
                None => {
                    // Stream ended; flush any remaining buffered content.
                    if buf.is_empty() {
                        return None;
                    }
                    // Treat the remaining buffer as a final (incomplete) line.
                    let result = parse_data_line(buf.trim());
                    buf.clear();
                    return result.map(|r| (r, (stream, buf)));
                }
                Some(Err(e)) => return Some((Err(Error::Http(e)), (stream, buf))),
                Some(Ok(bytes)) => {
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    continue;
                }
            }
        }
    })
}

/// Try to consume a complete SSE `data:` line from `buf`.
///
/// Returns `None` if there is no newline yet (caller should read more bytes).
/// Returns `None` if the line was `[DONE]` (stream finished).
fn consume_line(buf: &mut String) -> Option<Result<ChatCompletionChunk, Error>> {
    loop {
        let newline = buf.find('\n')?;
        let line = buf[..newline].trim_end_matches('\r').to_owned();
        buf.drain(..=newline);

        // Skip empty lines (SSE event separator).
        if line.is_empty() {
            continue;
        }

        return parse_data_line(&line);
    }
}

/// Parse a single SSE `data:` line.
///
/// * `data: [DONE]`  → `None`  (signals end of stream; the caller stops)
/// * `data: {...}`   → `Some(Ok(chunk))`
/// * anything else   → `Some(Err(...))`
fn parse_data_line(line: &str) -> Option<Result<ChatCompletionChunk, Error>> {
    if let Some(payload) = line.strip_prefix("data:") {
        let payload = payload.trim();
        if payload == "[DONE]" {
            return None;
        }
        Some(serde_json::from_str::<ChatCompletionChunk>(payload).map_err(Error::Json))
    } else {
        // Non-data lines (e.g. `event:`, `id:`, comments) are silently skipped.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_done_sentinel() {
        assert!(parse_data_line("data: [DONE]").is_none());
    }

    #[test]
    fn skips_non_data_lines() {
        assert!(parse_data_line("event: delta").is_none());
        assert!(parse_data_line(": keep-alive").is_none());
    }

    #[test]
    fn parses_valid_chunk() {
        let json = r#"{"id":"c1","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let result = parse_data_line(&format!("data: {json}")).unwrap().unwrap();
        assert_eq!(result.id, "c1");
        assert_eq!(result.choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn returns_error_on_invalid_json() {
        let result = parse_data_line("data: {not json}").unwrap();
        assert!(result.is_err());
    }
}
