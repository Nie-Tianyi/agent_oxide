use serde::{Deserialize, Serialize};

use crate::message::{Role, ToolCall};
use crate::response::{FinishReason, Usage};

/// A single chunk in a streaming response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StreamChunk {
    /// Unique identifier for this completion.
    pub id: String,
    /// The object type (e.g. `"chat.completion.chunk"`).
    pub object: String,
    /// Unix timestamp (seconds) of when this chunk was created.
    pub created: u64,
    /// The model that produced this chunk.
    pub model: String,
    /// One or more choice deltas in this chunk.
    pub choices: Vec<ChunkChoice>,
    /// Token-usage statistics (may only appear in the final chunk).
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// A single choice delta within a [`StreamChunk`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkChoice {
    /// The index of this choice.
    pub index: u32,
    /// The incremental delta for this choice.
    pub delta: Delta,
    /// The reason the model stopped (only in the final chunk for this choice).
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

/// An incremental update in a streaming response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delta {
    /// The role of the author (e.g. `Assistant`) — usually only in the first chunk.
    #[serde(default)]
    pub role: Option<Role>,
    /// A fragment of text content.
    #[serde(default)]
    pub content: Option<String>,
    /// A fragment of chain-of-thought / reasoning content.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Tool-call fragments being accumulated across chunks.
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_deserialization() {
        let raw = r#"{
            "id": "chatcmpl-xxx",
            "object": "chat.completion.chunk",
            "created": 1781984231,
            "model": "test-model",
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

    #[test]
    fn test_chunk_with_tool_call() {
        let raw = r#"{
            "id": "c",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "test-model",
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
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        let tc = &chunk.choices[0].delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.function.name, "get_weather");
    }
}
