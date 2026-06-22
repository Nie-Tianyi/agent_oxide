use serde::{Deserialize, Serialize};

use super::request::ToolCall;

// ── Response ────────────────────────────────────────────────────────────────

/// Exact match of the DeepSeek `/chat/completions` response body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeepSeekResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
    #[serde(default)]
    pub system_fingerprint: Option<String>,
}

/// The reason the model stopped generating tokens.
///
/// OpenAI-compatible APIs use a fixed vocabulary. `Other` catches any value
/// the API might add in the future so deserialisation never fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishReason {
    /// Model completed the response naturally.
    Stop,
    /// Response was cut off because `max_tokens` was reached.
    Length,
    /// Model emitted one or more tool calls instead of plain text.
    ToolCalls,
    /// Content was omitted due to a safety filter.
    ContentFilter,
    /// DeepSeek-specific: system under heavy load, response may be partial.
    InsufficientSystemResource,
    /// Forward-compatibility catch-all for values not yet in the known set.
    Other(String),
}

impl Serialize for FinishReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolCalls => "tool_calls",
            Self::ContentFilter => "content_filter",
            Self::InsufficientSystemResource => "insufficient_system_resource",
            Self::Other(s) => s.as_str(),
        };
        serializer.serialize_str(s)
    }
}

impl<'de> Deserialize<'de> for FinishReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "stop" => Self::Stop,
            "length" => Self::Length,
            "tool_calls" => Self::ToolCalls,
            "content_filter" => Self::ContentFilter,
            "insufficient_system_resource" => Self::InsufficientSystemResource,
            _ => Self::Other(s),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChoiceMessage,
    #[serde(default)]
    pub logprobs: Option<serde_json::Value>,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChoiceMessage {
    pub role: String,
    pub content: Option<String>,
    /// Thinking/reasoning output from DeepSeek-R1 / V4 thinking mode.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Present when the model emits tool calls.
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_response_deserialization() {
        let raw = r#"{
            "id": "abc-123",
            "object": "chat.completion",
            "created": 1781984231,
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello!",
                    "reasoning_content": "The user said hi...",
                    "tool_calls": null
                },
                "logprobs": null,
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 56,
                "total_tokens": 66
            },
            "system_fingerprint": "fp_9954b31ca7"
        }"#;
        let resp: DeepSeekResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.id, "abc-123");
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("Hello!"));
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("The user said hi...")
        );
        assert_eq!(resp.choices[0].finish_reason, Some(FinishReason::Stop));
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, 66);
    }

    #[test]
    fn test_finish_reason_serialize() {
        use serde_json::json;
        assert_eq!(serde_json::to_value(FinishReason::Stop).unwrap(), json!("stop"));
        assert_eq!(serde_json::to_value(FinishReason::Length).unwrap(), json!("length"));
        assert_eq!(serde_json::to_value(FinishReason::ToolCalls).unwrap(), json!("tool_calls"));
        assert_eq!(
            serde_json::to_value(FinishReason::ContentFilter).unwrap(),
            json!("content_filter"),
        );
        assert_eq!(
            serde_json::to_value(FinishReason::InsufficientSystemResource).unwrap(),
            json!("insufficient_system_resource"),
        );
        assert_eq!(
            serde_json::to_value(FinishReason::Other("custom_reason".into())).unwrap(),
            json!("custom_reason"),
        );
    }

    #[test]
    fn test_finish_reason_deserialize() {
        let fr: FinishReason = serde_json::from_str(r#""stop""#).unwrap();
        assert_eq!(fr, FinishReason::Stop);

        let fr: FinishReason = serde_json::from_str(r#""tool_calls""#).unwrap();
        assert_eq!(fr, FinishReason::ToolCalls);
    }

    /// If DeepSeek adds a new finish reason tomorrow, it's captured as
    /// `Other(...)` so the caller can still inspect it.
    #[test]
    fn test_finish_reason_field_unknown_is_other() {
        let raw = r#"{
            "id": "c",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "brand_new_reason_2030"
            }],
            "usage": null
        }"#;
        // Deserialised via DeepSeekChunk, which lives in stream.rs. We test the
        // FinishReason field directly here.
        use serde_json::Value;
        let v: Value = serde_json::from_str(raw).unwrap();
        let fr_str = v["choices"][0]["finish_reason"].as_str().unwrap();
        let fr: FinishReason = serde_json::from_str(&format!("\"{fr_str}\"")).unwrap();
        assert_eq!(fr, FinishReason::Other("brand_new_reason_2030".into()));
    }

    #[test]
    fn test_finish_reason_deserialize_direct_unknown() {
        let fr: FinishReason = serde_json::from_str(r#""future_reason""#).unwrap();
        assert_eq!(fr, FinishReason::Other("future_reason".into()));
    }
}
