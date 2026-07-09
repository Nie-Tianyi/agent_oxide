use serde::Serialize;

use provider::{CompletionRequest, Message, ToolChoice, ToolDef};

/// DeepSeek-specific completion request.
///
/// Wraps [`CompletionRequest`] with additional DeepSeek-only fields
/// (`thinking`, `reasoning_effort`, `response_format`, etc.).
#[derive(Clone, Debug, Serialize)]
pub struct DeepSeekRequest {
    pub messages: Vec<Message>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default)]
    pub logprobs: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,
}

impl DeepSeekRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            messages,
            model: model.into(),
            thinking: None,
            reasoning_effort: None,
            max_tokens: None,
            response_format: None,
            stop: None,
            stream: false,
            stream_options: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            logprobs: false,
            top_logprobs: None,
        }
    }
}

impl From<CompletionRequest> for DeepSeekRequest {
    fn from(req: CompletionRequest) -> Self {
        Self {
            messages: req.messages,
            model: req.model,
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            top_p: req.top_p,
            stop: req.stop,
            stream: req.stream,
            tools: req.tools,
            tool_choice: req.tool_choice,
            ..Self::default()
        }
    }
}

impl Default for DeepSeekRequest {
    fn default() -> Self {
        Self::new("", vec![])
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Thinking {
    #[serde(rename = "type")]
    pub r#type: ThinkingType,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingType {
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub r#type: ResponseFormatType,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormatType {
    Text,
    JsonObject,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use provider::Role;

    #[test]
    fn test_request_serialization() {
        let req = DeepSeekRequest::new("deepseek-v4-pro", vec![Message::new(Role::User, "Hi")]);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""model":"deepseek-v4-pro""#));
        assert!(json.contains(r#""stream":false"#));
    }

    #[test]
    fn test_from_completion_request() {
        let cr = CompletionRequest::new("m", vec![])
            .with_stream(true)
            .with_max_tokens(100);
        let ds: DeepSeekRequest = cr.into();
        assert!(ds.stream);
        assert_eq!(ds.max_tokens, Some(100));
    }
}
