use serde::{Deserialize, Serialize};

use super::error::DeepSeekError;
use super::request::ToolCall;
use super::response::FinishReason;
use super::response::Usage;

// ── Streaming ───────────────────────────────────────────────────────────────

/// Server-Sent Events (SSE) response stream from DeepSeek.
///
/// # What is SSE?
///
/// Server-Sent Events is a simple HTTP-based protocol for receiving real-time
/// data. Instead of sending one complete response then closing the connection,
/// the server keeps the connection open and sends multiple **events** over time.
///
/// Each event is plain text separated by a blank line (`\n\n`):
///
/// ```text
/// data: {"id":"...","choices":[{"delta":{"content":"Hello"}}]}
///
/// data: {"id":"...","choices":[{"delta":{"content":" world"}}]}
///
/// data: [DONE]
/// ```
///
/// DeepSeek uses SSE to stream chat completions. Each event carries one JSON
/// chunk — a tiny piece of the model's reply, typically a few characters.
/// The stream ends with the `data: [DONE]` sentinel.
///
/// # Usage
///
/// ```no_run
/// let mut stream: DeepSeekStream = client.stream(request).await?;
/// while let Some(chunk) = stream.next().await {
///     match chunk {
///         Ok(c) => print!("{}", c.choices[0].delta.content.as_deref().unwrap_or("")),
///         Err(e) => eprintln!("stream error: {e}"),
///     }
/// }
/// ```
pub struct DeepSeekStream {
    /// The HTTP response body. We read from it in small chunks as data arrives.
    pub(crate) response: reqwest::Response,
    /// Accumulates bytes read from the network that haven't yet formed a complete
    /// SSE event. SSE events are separated by `\n\n`, but network chunks can split
    /// an event mid-line, so we buffer until we see the separator.
    pub(crate) buffer: Vec<u8>,
    /// Set to `true` once the `data: [DONE]` sentinel is received.
    pub(crate) finished: bool,
}

/// A single chunk in a streaming DeepSeek response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeepSeekChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

// ── SSE Parsing Helpers ─────────────────────────────────────────────
//
// These functions understand the SSE wire format but are otherwise
// protocol-agnostic. They transform: raw bytes → event text → JSON string.

/// Find the end of the first complete SSE event in `buf`.
///
/// SSE separates events with a blank line — two consecutive `\n` characters.
/// This function returns the byte offset just past the first `\n\n`, so that
/// `buf[..pos]` is one complete event (including the trailing newlines).
///
/// Returns `None` if no complete event has arrived yet (need more data).
///
/// # Example
///
/// ```
/// let buf = b"data: {\"x\":1}\n\ndata: [DONE]\n\n";
/// let end = find_event_end(buf).unwrap(); // end = 15
/// assert_eq!(&buf[..end], b"data: {\"x\":1}\n\n");
/// ```
fn find_event_end(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i + 2);
        }
    }
    None
}

/// Strip trailing `\r` and `\n` bytes from a byte slice.
///
/// After we drain an event from the buffer, the event bytes include the
/// terminating `\n\n`. This helper trims them so we get clean event text.
fn trim_trailing_newlines(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|&b| b != b'\r' && b != b'\n')
        .map_or(0, |p| p + 1);
    &bytes[..end]
}

/// Extract the JSON payload from an SSE event's `data:` lines.
///
/// The SSE spec allows a single event to span multiple `data:` lines:
/// ```text
/// data: {"key":
/// data:  "value"}
/// ```
/// This function strips the `data: ` prefix from every line and joins them.
/// DeepSeek emits one JSON object per event on a single line, but this
/// implementation handles the multi-line case correctly too.
fn extract_sse_data(event_text: &str) -> String {
    event_text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect::<Vec<_>>()
        .join("")
}

impl DeepSeekStream {
    /// Read the next raw SSE event text from the network.
    ///
    /// This is the low-level reader. It pulls HTTP chunks, accumulates them
    /// in `self.buffer`, and yields one complete event (everything up to and
    /// including the terminating `\n\n`) at a time.
    ///
    /// Returns `None` when the HTTP response body has been fully consumed.
    async fn read_event(&mut self) -> Option<Result<String, DeepSeekError>> {
        loop {
            // ── Do we already have a complete event in the buffer? ──
            if let Some(end) = find_event_end(&self.buffer) {
                // Drain the bytes for this event (including trailing \n\n).
                let event_bytes: Vec<u8> = self.buffer.drain(..end).collect();
                let trimmed = trim_trailing_newlines(&event_bytes);
                let text = String::from_utf8_lossy(trimmed).into_owned();
                return Some(Ok(text));
            }

            // ── Not yet — read another chunk from the HTTP stream ──
            // `response.chunk()` returns the next piece of the body:
            //  • `Ok(Some(bytes))` — more data arrived
            //  • `Ok(None)`        — body is done
            //  • `Err(e)`          — network error
            match self.response.chunk().await {
                Ok(Some(bytes)) => {
                    self.buffer.extend_from_slice(&bytes);
                    // Loop back to check if we now have a full event.
                }
                Ok(None) => {
                    // Connection closed. Flush any leftover data.
                    if self.buffer.is_empty() {
                        return None;
                    }
                    let remaining = std::mem::take(&mut self.buffer);
                    let trimmed = trim_trailing_newlines(&remaining);
                    let text = String::from_utf8_lossy(trimmed).into_owned();
                    return Some(Ok(text));
                }
                Err(e) => return Some(Err(DeepSeekError::Http(e))),
            }
        }
    }

    /// Return the next parsed `DeepSeekChunk`, or `None` when the stream ends.
    ///
    /// This is the main public API. It orchestrates the full pipeline:
    ///
    /// 1. **Read** raw bytes from the HTTP response until a complete SSE event
    ///    arrives (events end with `\n\n`).
    /// 2. **Extract** the JSON payload from the event's `data:` lines.
    /// 3. **Parse** the JSON into a strongly-typed `DeepSeekChunk`.
    ///
    /// Empty events (comments, keepalive pings, blank lines between events)
    /// are silently skipped. The `data: [DONE]` sentinel ends the stream.
    pub async fn next(&mut self) -> Option<Result<DeepSeekChunk, DeepSeekError>> {
        if self.finished {
            return None;
        }

        loop {
            // Step 1 & 2: read raw event + extract `data:` payload.
            let event_text = match self.read_event().await? {
                Ok(t) => t,
                Err(e) => {
                    self.finished = true;
                    return Some(Err(e));
                }
            };

            let data = extract_sse_data(&event_text);

            // Empty data → comment, keepalive, or whitespace between events.
            // Skip it and read the next event.
            if data.is_empty() {
                continue;
            }

            // The `[DONE]` sentinel means the stream is complete.
            // It's not valid JSON, so we handle it before deserialization.
            if data.trim() == "[DONE]" {
                self.finished = true;
                return None;
            }

            // Step 3: deserialize the JSON into our typed chunk.
            match serde_json::from_str::<DeepSeekChunk>(&data) {
                Ok(chunk) => return Some(Ok(chunk)),
                Err(e) => {
                    self.finished = true;
                    return Some(Err(DeepSeekError::Parse(e.to_string())));
                }
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_deserialization() {
        // Simulates an SSE "data:" line
        let raw = r#"{
            "id": "chatcmpl-xxx",
            "object": "chat.completion.chunk",
            "created": 1781984231,
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "delta": {
                    "content": "Hello!",
                    "reasoning_content": null
                },
                "finish_reason": null
            }],
            "usage": null
        }"#;
        let chunk: DeepSeekChunk = serde_json::from_str(raw).unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello!"));
        assert_eq!(chunk.choices[0].finish_reason, None);
    }

    #[test]
    fn test_chunk_with_tool_call() {
        let raw = r#"{
            "id": "c",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Beijing\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": null
        }"#;
        let chunk: DeepSeekChunk = serde_json::from_str(raw).unwrap();
        let tc = &chunk.choices[0].delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.function.name, "get_weather");
        assert_eq!(tc.function.arguments, r#"{"city":"Beijing"}"#);
        assert_eq!(
            chunk.choices[0].finish_reason,
            Some(FinishReason::ToolCalls),
        );
    }

    #[test]
    fn test_find_event_end() {
        let buf = b"data: {\"x\":1}\n\ndata: {\"y\":2}\n\n";
        let end = find_event_end(buf).unwrap();
        // First event including the \n\n separator
        assert_eq!(&buf[..end], b"data: {\"x\":1}\n\n");
    }

    #[test]
    fn test_find_event_end_no_match() {
        assert!(find_event_end(b"").is_none());
        assert!(find_event_end(b"data: partial").is_none());
        assert!(find_event_end(b"data: almost\n").is_none());
    }

    #[test]
    fn test_extract_sse_data_single_line() {
        let event = "data: {\"x\":1}";
        assert_eq!(extract_sse_data(event), "{\"x\":1}");
    }

    #[test]
    fn test_extract_sse_data_multi_line() {
        let event = "data: {\"x\":\ndata: 1}";
        assert_eq!(extract_sse_data(event), "{\"x\":1}");
    }

    #[test]
    fn test_extract_sse_data_skips_non_data_lines() {
        let event = ":comment\n:another comment\ndata: {\"x\":1}\n";
        assert_eq!(extract_sse_data(event), "{\"x\":1}");
    }

    #[test]
    fn test_trim_trailing_newlines() {
        assert_eq!(trim_trailing_newlines(b"hello\n\n"), b"hello");
        assert_eq!(trim_trailing_newlines(b"hello"), b"hello");
        assert_eq!(trim_trailing_newlines(b"\n\n"), b"");
        assert_eq!(trim_trailing_newlines(b""), b"");
    }
}
