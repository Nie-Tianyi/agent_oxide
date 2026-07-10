//! [`EchoTool`] — 最简单的工具实现，将输入原样返回。
//!
//! 主要用于：
//! - 测试 tool-use 循环的端到端连通性
//! - 作为实现自定义工具的最小参考示例
//!
//! # 作为参考实现
//!
//! 如果你要写自己的工具，从复制这个文件开始，然后：
//! 1. 定义 args struct（#[derive(JsonSchema, Deserialize)]）
//! 2. 用 `#[tool(name = "...", description = "...", args = YourArgs)]` 标注 struct
//! 3. 实现 `fn execute_stream(&self, args: YourArgs) -> Result<ProgressStream, ToolError>`
//! 4. 返回 `Ok(result_string)` 或 `Err(ToolError::...)`

use schemars::JsonSchema;
use serde::Deserialize;

use tools::{ProgressStream, ToolError, tool};

/// Echo 工具的参数。
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EchoArgs {
    /// The text to echo back verbatim. Only useful for testing the tool-call pipeline.
    #[schemars(
        description = "The text to echo back verbatim. Only useful for testing the tool-call pipeline."
    )]
    pub text: String,
}

/// 将输入文本原样返回的工具。
///
/// # 参数
///
/// ```json
/// {"text": "要回显的文本"}
/// ```
///
/// # 示例
///
/// ```
/// use loomis::tools::EchoTool;
/// use tools::Tool;
///
/// let tool = EchoTool;
/// assert_eq!(tool.name(), "echo");
/// let result = Tool::execute_stream(&tool, r#"{"text": "hello"}"#).unwrap().poll_done();
/// assert_eq!(result, "hello");
/// ```
#[tool(
    name = "echo",
    description = "Echo the input text back unchanged. This is a no-op tool that exists solely \
         for testing and debugging the agent's tool-dispatch loop.\n\n\
         IMPORTANT: This tool has no practical use for real work. Do NOT call it in \
         normal conversation — it does nothing useful.\n\n\
         When to use: ONLY for verifying that the tool-call pipeline works correctly \
         during development or debugging.\n\n\
         When NOT to use: any actual task or user request.",
    args = EchoArgs
)]
pub struct EchoTool;

impl EchoTool {
    fn execute_stream(&self, args: EchoArgs) -> Result<ProgressStream, ToolError> {
        let output = args.text;
        Ok(ProgressStream::done(output))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    #[test]
    fn test_name() {
        assert_eq!(EchoTool.name(), "echo");
    }

    #[test]
    fn test_description() {
        assert!(EchoTool.description().contains("no-op"));
    }

    #[test]
    fn test_parameters_schema() {
        let params = EchoTool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["text"]["type"] == "string");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_execute_returns_text() {
        let result = Tool::execute_stream(&EchoTool, r#"{"text": "hello world"}"#)
            .unwrap()
            .poll_done();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_execute_empty_string() {
        let result = Tool::execute_stream(&EchoTool, r#"{"text": ""}"#)
            .unwrap()
            .poll_done();
        assert_eq!(result, "");
    }

    #[test]
    fn test_execute_missing_field() {
        let err = Tool::execute_stream(&EchoTool, r#"{"wrong": "x"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_execute_bad_json() {
        let err = Tool::execute_stream(&EchoTool, "garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_execute_extra_field_rejected() {
        // deny_unknown_fields should reject extra fields
        let err =
            Tool::execute_stream(&EchoTool, r#"{"text": "hello", "extra": true}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_to_def_matches() {
        let def = EchoTool.to_def();
        assert_eq!(def.function.name, "echo");
        assert!(def.function.description.is_some());
        assert!(def.function.parameters.is_some());
    }
}
