use serde::{Deserialize, Serialize};

use crate::message::{Role, ToolCall};

/// Provider-agnostic completion response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

/// The reason the model stopped generating tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishReason {
    /// The model reached a natural stopping point.
    Stop,
    /// The model reached the maximum token limit.
    Length,
    /// The model stopped because it wants to call tools.
    ToolCalls,
    /// The content was filtered by the provider's safety system.
    ContentFilter,
    /// The provider's system resources were exhausted.
    InsufficientSystemResource,
    /// Forward-compatibility catch-all for unknown reasons.
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

/// A single choice in a completion response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    /// The index of this choice in the response.
    pub index: u32,
    /// The message content for this choice.
    pub message: ChoiceMessage,
    /// The reason the model stopped generating (if known).
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

/// The message within a [`Choice`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChoiceMessage {
    /// The role of the message author (e.g. `Assistant`).
    pub role: Role,
    /// The text content (may be `None` when only tool calls are present).
    pub content: Option<String>,
    /// Chain-of-thought / reasoning content (provider-specific).
    pub reasoning_content: Option<String>,
    /// Tool calls requested by the model (when `role` is `Assistant`).
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Token-usage statistics for a completion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens consumed by the input (prompt + context).
    pub prompt_tokens: u32,
    /// Tokens generated in the completion.
    pub completion_tokens: u32,
    /// Total tokens (prompt + completion).
    pub total_tokens: u32,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_finish_reason_serialize() {
        assert_eq!(
            serde_json::to_value(FinishReason::Stop).unwrap(),
            json!("stop")
        );
        assert_eq!(
            serde_json::to_value(FinishReason::ToolCalls).unwrap(),
            json!("tool_calls")
        );
    }

    #[test]
    fn test_finish_reason_deserialize() {
        let fr: FinishReason = serde_json::from_str(r#""stop""#).unwrap();
        assert_eq!(fr, FinishReason::Stop);
    }

    #[test]
    fn test_finish_reason_unknown_is_other() {
        let fr: FinishReason = serde_json::from_str(r#""future_reason""#).unwrap();
        assert_eq!(fr, FinishReason::Other("future_reason".into()));
    }
}
