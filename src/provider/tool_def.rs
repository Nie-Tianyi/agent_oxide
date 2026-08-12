use serde::{Serialize, Serializer};

/// A tool definition sent to the LLM as part of the request.
#[derive(Clone, Debug, Serialize)]
pub struct ToolDef {
    /// The kind of tool definition (currently only `Function`).
    #[serde(rename = "type")]
    pub kind: ToolDefKind,
    /// The function's name, description, and parameter schema.
    pub function: FunctionDef,
}

/// The kind of tool definition.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ToolDefKind {
    /// A function-based tool definition.
    Function,
}

/// A function definition within a tool definition.
#[derive(Clone, Debug, Serialize)]
pub struct FunctionDef {
    /// The function name (must match the tool's registered name).
    pub name: String,
    /// Human-readable description of what the function does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the function's parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Controls how the model uses tools.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ToolChoice {
    /// `"none"` — never call a tool.
    None,
    /// `"auto"` — model decides.
    Auto,
    /// `"required"` — model must call a tool.
    Required,
    /// `{"type": "function", "function": {"name": "..."}}` — force a specific function.
    Specific {
        /// The kind of tool to force (currently only `Function`).
        kind: ToolDefKind,
        /// Reference to the specific function by name.
        function: ToolChoiceFunction,
    },
}

impl Serialize for ToolChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::None => serializer.serialize_str("none"),
            Self::Auto => serializer.serialize_str("auto"),
            Self::Required => serializer.serialize_str("required"),
            Self::Specific { kind, function } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", kind)?;
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_tool_choice_serialization() {
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
                kind: ToolDefKind::Function,
                function: ToolChoiceFunction { name: "f".into() },
            })
            .unwrap(),
            json!({"type": "function", "function": {"name": "f"}})
        );
    }
}
