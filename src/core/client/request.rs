use serde::{Deserialize, Serialize};

// ── Request ─────────────────────────────────────────────────────────────────

/// Exact match of the DeepSeek `/chat/completions` request body.
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Present when role is `assistant` and the model wants to call tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Present when role is `tool` — the id of the tool call this message responds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub r#type: ToolCallType,
    pub function: ToolCallFunction,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallType {
    Function,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// JSON-encoded arguments string.
    pub arguments: String,
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

/// Matches the DeepSeek `tools` array element.
#[derive(Clone, Debug, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub r#type: ToolDefType,
    pub function: FunctionDef,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolDefType {
    Function,
}

#[derive(Clone, Debug, Serialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Matches the DeepSeek `tool_choice` field.
#[derive(Clone, Debug)]
pub enum ToolChoice {
    /// `"none"` — never call a tool.
    None,
    /// `"auto"` — model decides.
    Auto,
    /// `"required"` — model must call a tool.
    Required,
    /// `{"type": "function", "function": {"name": "..."}}` — force a specific function.
    Specific {
        r#type: ToolDefType,
        function: ToolChoiceFunction,
    },
}

impl Serialize for ToolChoice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::None => serializer.serialize_str("none"),
            Self::Auto => serializer.serialize_str("auto"),
            Self::Required => serializer.serialize_str("required"),
            Self::Specific { r#type, function } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", r#type)?;
                map.serialize_entry("function", function)?;
                map.end()
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolChoiceFunction {
    pub name: String,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = DeepSeekRequest::new(
            "deepseek-chat",
            vec![
                Message::new(Role::System, "You are helpful"),
                Message::new(Role::User, "Hi"),
            ],
        );
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""model":"deepseek-chat""#));
        assert!(json.contains(r#""role":"system""#));
        assert!(json.contains(r#""stream":false"#));
        assert!(json.contains(r#""logprobs":false"#));
    }

    #[test]
    fn test_request_with_thinking() {
        let req = DeepSeekRequest {
            thinking: Some(Thinking {
                r#type: ThinkingType::Enabled,
            }),
            reasoning_effort: Some(ReasoningEffort::High),
            ..DeepSeekRequest::new("deepseek-v4-pro", vec![])
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""thinking":{"type":"enabled"}"#));
        assert!(json.contains(r#""reasoning_effort":"high""#));
    }

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
    fn test_tool_choice_serialization() {
        use serde_json::json;

        assert_eq!(
            serde_json::to_value(&ToolChoice::None).unwrap(),
            json!("none")
        );
        assert_eq!(
            serde_json::to_value(&ToolChoice::Auto).unwrap(),
            json!("auto")
        );
        assert_eq!(
            serde_json::to_value(&ToolChoice::Required).unwrap(),
            json!("required")
        );
        assert_eq!(
            serde_json::to_value(&ToolChoice::Specific {
                r#type: ToolDefType::Function,
                function: ToolChoiceFunction { name: "f".into() },
            })
            .unwrap(),
            json!({"type": "function", "function": {"name": "f"}})
        );
    }
}
