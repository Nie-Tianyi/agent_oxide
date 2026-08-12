use serde::{Deserialize, Serialize};

/// A single message in a conversation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    /// The role of the message author.
    pub role: Role,
    /// The text content of the message.
    pub content: String,
    /// Chain-of-thought / reasoning content (provider-specific).
    /// Only meaningful for `Assistant` role messages from models that
    /// support thinking/reasoning (e.g. DeepSeek R1, Claude Opus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Present when role is `Assistant` and the model wants to call tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Present when role is `Tool` — the id of the tool call this message responds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional name: tool name (role `Tool`) or participant name (role `User`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            reasoning_content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Role {
    /// System prompt / instructions.
    System,
    /// End-user message.
    User,
    /// Model-generated response.
    Assistant,
    /// Result of a tool execution.
    Tool,
}

impl Role {
    /// Human-readable label for this role.
    pub const fn label(self) -> &'static str {
        match self {
            Role::System => "System",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool",
        }
    }
}

/// A tool call emitted by the model in a streaming or non-streaming response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    /// Tool-call index within the choice.
    #[serde(default)]
    pub index: u32,
    /// Unique identifier for this tool call.
    #[serde(default)]
    pub id: String,
    /// The kind of tool call (currently only `function`).
    #[serde(rename = "type", default)]
    pub kind: ToolCallKind,
    /// The function name and arguments.
    pub function: ToolCallFunction,
}

/// The kind of tool call — currently only `Function` is supported.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ToolCallKind {
    /// A function-call tool invocation.
    #[default]
    Function,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCallFunction {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_new() {
        let msg = Message::new(Role::User, "Hello");
        assert_eq!(msg.content, "Hello");
        assert!(matches!(msg.role, Role::User));
    }

    #[test]
    fn test_role_serialization() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), r#""system""#);
    }

    #[test]
    fn test_assistant_with_tools() {
        let tc = ToolCall {
            index: 0,
            id: "call_1".into(),
            kind: ToolCallKind::Function,
            function: ToolCallFunction {
                name: "echo".into(),
                arguments: r#"{"text":"hi"}"#.into(),
            },
        };
        let msg = Message::assistant_with_tools("", vec![tc]);
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.tool_calls.unwrap().len(), 1);
    }

    #[test]
    fn test_tool_result() {
        let msg = Message::tool_result("call_1", "output");
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));
    }
}
