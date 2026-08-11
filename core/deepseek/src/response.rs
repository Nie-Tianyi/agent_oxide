//! DeepSeek-specific response types — mirrors the provider crate's types
//! with serde derives needed for the DeepSeek API wire format.

use serde::Deserialize;

use provider::{Choice, ChoiceMessage, CompletionResponse, FinishReason, Role, ToolCall, Usage};

/// DeepSeek-specific API response. Maps to [`CompletionResponse`] via [`From`].
#[derive(Clone, Debug, Deserialize)]
pub struct DeepSeekResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<DeepSeekChoice>,
    pub usage: Option<DeepSeekUsage>,
    #[serde(default)]
    pub system_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeepSeekChoice {
    pub index: u32,
    pub message: DeepSeekChoiceMessage,
    #[serde(default)]
    pub logprobs: Option<serde_json::Value>,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeepSeekChoiceMessage {
    pub role: Role,
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeepSeekUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl From<DeepSeekResponse> for CompletionResponse {
    fn from(resp: DeepSeekResponse) -> Self {
        Self {
            id: resp.id,
            object: resp.object,
            created: resp.created,
            model: resp.model,
            choices: resp.choices.into_iter().map(Into::into).collect(),
            usage: resp.usage.map(Into::into),
        }
    }
}

impl From<DeepSeekChoice> for Choice {
    fn from(c: DeepSeekChoice) -> Self {
        Self {
            index: c.index,
            message: c.message.into(),
            finish_reason: c.finish_reason,
        }
    }
}

impl From<DeepSeekChoiceMessage> for ChoiceMessage {
    fn from(m: DeepSeekChoiceMessage) -> Self {
        Self {
            role: m.role,
            content: m.content,
            reasoning_content: m.reasoning_content,
            tool_calls: m.tool_calls,
        }
    }
}

impl From<DeepSeekUsage> for Usage {
    fn from(u: DeepSeekUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use provider::FinishReason;

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
                    "reasoning_content": null,
                    "tool_calls": null
                },
                "logprobs": null,
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 56,
                "total_tokens": 66
            }
        }"#;
        let resp: DeepSeekResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.id, "abc-123");

        let cr: CompletionResponse = resp.into();
        assert_eq!(cr.choices[0].message.content.as_deref(), Some("Hello!"));
        assert_eq!(cr.choices[0].finish_reason, Some(FinishReason::Stop));
        assert_eq!(cr.usage.as_ref().unwrap().total_tokens, 66);
    }
}
