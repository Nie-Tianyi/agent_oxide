use provider::StreamChunk;

use crate::error::DeepSeekError;

/// Server-Sent Events (SSE) response stream from DeepSeek.
///
/// Yields [`StreamChunk`] values as they arrive from the API.
pub struct DeepSeekStream {
    pub(crate) response: reqwest::Response,
    pub(crate) buffer: Vec<u8>,
    pub(crate) finished: bool,
}

// ── SSE Parsing Helpers ────────────────────────────────────────────────────

fn find_event_end(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i + 2);
        }
    }
    None
}

fn trim_trailing_newlines(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|&b| b != b'\r' && b != b'\n')
        .map_or(0, |p| p + 1);
    &bytes[..end]
}

fn extract_sse_data(event_text: &str) -> String {
    event_text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect::<Vec<_>>()
        .join("")
}

const MAX_SSE_EVENT_SIZE: usize = 16 * 1024 * 1024;

impl DeepSeekStream {
    /// Read the next raw SSE event text.
    async fn read_event(&mut self) -> Option<Result<String, DeepSeekError>> {
        loop {
            if let Some(end) = find_event_end(&self.buffer) {
                let event_bytes: Vec<u8> = self.buffer.drain(..end).collect();
                let trimmed = trim_trailing_newlines(&event_bytes);
                let text = String::from_utf8_lossy(trimmed).into_owned();
                return Some(Ok(text));
            }

            match self.response.chunk().await {
                Ok(Some(bytes)) => {
                    self.buffer.extend_from_slice(&bytes);
                    if self.buffer.len() > MAX_SSE_EVENT_SIZE {
                        tracing::error!(
                            max_bytes = MAX_SSE_EVENT_SIZE,
                            "SSE stream buffer overflow",
                        );
                        self.finished = true;
                        return Some(Err(DeepSeekError::Parse(format!(
                            "SSE buffer overflow: exceeded {} bytes",
                            MAX_SSE_EVENT_SIZE,
                        ))));
                    }
                }
                Ok(None) => {
                    if self.buffer.is_empty() {
                        return None;
                    }
                    let remaining = std::mem::take(&mut self.buffer);
                    let trimmed = trim_trailing_newlines(&remaining);
                    let text = String::from_utf8_lossy(trimmed).into_owned();
                    return Some(Ok(text));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "error while reading SSE stream chunk");
                    return Some(Err(DeepSeekError::Http(e)));
                }
            }
        }
    }

    /// Return the next parsed [`StreamChunk`], or `None` when the stream ends.
    pub async fn next(&mut self) -> Option<Result<StreamChunk, DeepSeekError>> {
        if self.finished {
            return None;
        }

        loop {
            let event_text = match self.read_event().await? {
                Ok(t) => t,
                Err(e) => {
                    self.finished = true;
                    return Some(Err(e));
                }
            };

            let data = extract_sse_data(&event_text);
            if data.is_empty() {
                continue;
            }

            if data.trim() == "[DONE]" {
                self.finished = true;
                return None;
            }

            match serde_json::from_str::<StreamChunk>(&data) {
                Ok(chunk) => return Some(Ok(chunk)),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        data = %data.chars().take(300).collect::<String>(),
                        "failed to parse SSE stream chunk",
                    );
                    self.finished = true;
                    return Some(Err(DeepSeekError::Parse(e.to_string())));
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_event_end() {
        let buf = b"data: {\"x\":1}\n\ndata: {\"y\":2}\n\n";
        let end = find_event_end(buf).unwrap();
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
        assert_eq!(extract_sse_data("data: {\"x\":1}"), "{\"x\":1}");
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

    #[test]
    fn test_stream_chunk_deserialization() {
        let raw = r#"{
            "id": "c",
            "object": "chat.completion.chunk",
            "created": 1,
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
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello!"));
    }
}
