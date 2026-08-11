use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::tool_def::{ToolChoice, ToolDef};

/// Provider-agnostic completion request.
///
/// Contains the common fields across LLM providers. Provider-specific
/// crates (like `deepseek`) wrap or extend this with additional fields.
#[derive(Clone, Debug)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stop: Option<Vec<String>>,
    pub stream: bool,
    pub tools: Option<Vec<ToolDef>>,
    pub tool_choice: Option<ToolChoice>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl CompletionRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            messages,
            model: model.into(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: None,
            stream: false,
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }
}

/// Reasoning effort level for models that support it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    ExtraHigh,
    Max,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;

    #[test]
    fn test_completion_request_new() {
        let req = CompletionRequest::new("test-model", vec![Message::new(Role::User, "Hi")]);
        assert_eq!(req.model, "test-model");
        assert_eq!(req.messages.len(), 1);
        assert!(!req.stream);
    }

    #[test]
    fn test_builder_methods() {
        let req = CompletionRequest::new("m", vec![])
            .with_stream(true)
            .with_max_tokens(100);
        assert!(req.stream);
        assert_eq!(req.max_tokens, Some(100));
    }
}
