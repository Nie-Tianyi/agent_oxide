//! [`WriteTool`] — 文件写入工具。
//!
//! 创建或覆写文件内容。自动创建缺失的父目录。

use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[cfg(test)]
use tools::SandboxConfig;
use tools::WorkspaceFs;
use tools::{FsError, ProgressStream, ToolError, tool};

/// Write 工具的参数。
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteArgs {
    /// Path to write to, relative to workspace root.
    #[schemars(
        description = "Path to write to, relative to workspace root. Parent directories are created automatically. Always use forward slashes."
    )]
    pub file_path: String,

    /// The full content to write.
    #[schemars(
        description = "The full content to write. Multi-line text is supported via \\n newlines. CAUTION: existing content at this path is silently overwritten — read the file first."
    )]
    pub content: String,
}

/// 写入文件内容的工具。
///
/// # 参数
///
/// ```json
/// {"file_path": "output/result.md", "content": "# Hello\n\nWorld"}
/// ```
#[tool(
    name = "write",
    description = "Write content to a file in the workspace. Creates the file if it does not \
         exist; silently overwrites if it does. Parent directories are created \
         automatically.\n\n\
         IMPORTANT: Read the file first before overwriting, so you understand the \
         current state and don't accidentally destroy work.\n\n\
         When to use: creating a new file, replacing an entire file's contents, \
         writing a file that does not yet exist.\n\n\
         When NOT to use: modifying part of a file (use edit), appending (use shell \
         with >>), checking if a file exists (use ls or glob).\n\n\
         Return format: 'Wrote {N} bytes to {file_path}'.",
    args = WriteArgs
)]
pub struct WriteTool {
    fs: Arc<WorkspaceFs>,
}

impl WriteTool {
    pub fn new(fs: Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }

    fn execute_stream(&self, args: WriteArgs) -> Result<ProgressStream, ToolError> {
        self.fs
            .write(&args.file_path, &args.content)
            .map_err(map_fs_err)?;

        let output = format!("Wrote {} bytes to {}", args.content.len(), args.file_path);
        Ok(ProgressStream::done(output))
    }
}

fn map_fs_err(e: FsError) -> ToolError {
    match e {
        FsError::NotAFile(_) | FsError::PathEscapesWorkspace(_) => {
            ToolError::InvalidArgs(e.to_string())
        }
        _ => ToolError::Execution(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    fn setup() -> (tempfile::TempDir, WriteTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path(), &SandboxConfig::default()).unwrap();
        let tool = WriteTool::new(Arc::new(fs));
        (dir, tool)
    }

    fn read_file(dir: &tempfile::TempDir, path: &str) -> String {
        std::fs::read_to_string(dir.path().join(path)).unwrap()
    }

    #[test]
    fn test_name() {
        let (_dir, tool) = setup();
        assert_eq!(tool.name(), "write");
    }

    #[test]
    fn test_description() {
        let (_dir, tool) = setup();
        assert!(tool.description().contains("workspace"));
    }

    #[test]
    fn test_parameters_schema() {
        let (_dir, tool) = setup();
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_write_new_file() {
        let (dir, tool) = setup();
        let mut result = Tool::execute_stream(
            &tool,
            r#"{"file_path": "hello.txt", "content": "hello world"}"#,
        )
        .unwrap();
        let output = result.poll_done();
        assert!(output.contains("hello.txt"));
        assert!(output.contains("11 bytes"));
        assert_eq!(read_file(&dir, "hello.txt"), "hello world");
    }

    #[test]
    fn test_write_overwrite() {
        let (dir, tool) = setup();
        Tool::execute_stream(&tool, r#"{"file_path": "f.txt", "content": "old"}"#)
            .unwrap()
            .poll_done();
        Tool::execute_stream(&tool, r#"{"file_path": "f.txt", "content": "new"}"#)
            .unwrap()
            .poll_done();
        assert_eq!(read_file(&dir, "f.txt"), "new");
    }

    #[test]
    fn test_write_nested_path() {
        let (dir, tool) = setup();
        Tool::execute_stream(
            &tool,
            r#"{"file_path": "a/b/c/file.txt", "content": "deep"}"#,
        )
        .unwrap();
        assert_eq!(read_file(&dir, "a/b/c/file.txt"), "deep");
    }

    #[test]
    fn test_write_empty_content() {
        let (dir, tool) = setup();
        Tool::execute_stream(&tool, r#"{"file_path": "empty.txt", "content": ""}"#)
            .unwrap()
            .poll_done();
        assert_eq!(read_file(&dir, "empty.txt"), "");
    }

    #[test]
    fn test_missing_file_path() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(&tool, r#"{"content": "stuff"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
