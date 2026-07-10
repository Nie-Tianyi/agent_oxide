//! [`EditTool`] — 行级文件编辑工具。
//!
//! 替换文件中指定行范围的内容。支持删除行（传入空字符串）。

use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[cfg(test)]
use tools::SandboxConfig;
use tools::WorkspaceFs;
use tools::{FsError, ProgressStream, ToolError, tool};

/// Edit 工具的参数。
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditArgs {
    /// Path to the file to edit, relative to workspace root.
    #[schemars(
        description = "Path to the file to edit, relative to workspace root. Must be an existing file. Always use forward slashes."
    )]
    pub file_path: String,

    /// First line to replace (1-indexed, inclusive).
    #[schemars(
        description = "First line to replace (1-indexed, inclusive). MUST match the file's current state — always read the file first to get accurate line numbers."
    )]
    pub start_line: u64,

    /// Last line to replace (1-indexed, inclusive).
    #[schemars(
        description = "Last line to replace (1-indexed, inclusive). Must be >= start_line. Same line number as start_line to replace a single line."
    )]
    pub end_line: u64,

    /// Replacement text to insert in place of the selected lines.
    #[schemars(
        description = "Replacement text to insert in place of the selected lines. Pass empty string to delete the line range. Use \\n for multiple lines."
    )]
    pub new_content: String,
}

/// 用行号替换文件内容的工具。
///
/// # 参数
///
/// ```json
/// {
///     "file_path": "src/main.rs",
///     "start_line": 5,
///     "end_line": 7,
///     "new_content": "    let x = 42;\n    println!(\"{x}\");"
/// }
/// ```
///
/// 行号是 1-indexed，`start_line` 和 `end_line` 都是包含的。
#[tool(
    name = "edit",
    description = "Replace a specific range of lines in a file by line number. \
         start_line and end_line are 1-indexed and inclusive (e.g. start=3, end=5 \
         replaces lines 3, 4, and 5). Pass empty new_content to delete the range.\n\n\
         IMPORTANT: Always read the file first to get accurate line numbers. The \
         line numbers you provide MUST match the file's current state — stale line \
         numbers from memory or a prior read will corrupt the file.\n\n\
         When to use: modifying a few lines of an existing file, deleting lines, \
         inserting lines at a specific position.\n\n\
         When NOT to use: creating a new file (use write), replacing the entire file \
         (use write — simpler and less error-prone).\n\n\
         Return format: 'Edited {file_path}: replaced lines {start}-{end} with {N} \
         new lines'.",
    args = EditArgs
)]
pub struct EditTool {
    fs: Arc<WorkspaceFs>,
}

impl EditTool {
    pub fn new(fs: Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }

    fn execute_stream(&self, args: EditArgs) -> Result<ProgressStream, ToolError> {
        let output = self
            .fs
            .edit_lines(
                &args.file_path,
                args.start_line as usize,
                args.end_line as usize,
                &args.new_content,
            )
            .map_err(map_fs_err)?;
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

    fn setup() -> (tempfile::TempDir, EditTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path(), &SandboxConfig::default()).unwrap();
        let tool = EditTool::new(Arc::new(fs));
        (dir, tool)
    }

    fn write_file(dir: &tempfile::TempDir, path: &str, content: &str) {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    fn read_file(dir: &tempfile::TempDir, path: &str) -> String {
        std::fs::read_to_string(dir.path().join(path)).unwrap()
    }

    #[test]
    fn test_name() {
        let (_dir, tool) = setup();
        assert_eq!(tool.name(), "edit");
    }

    #[test]
    fn test_parameters_schema() {
        let (_dir, tool) = setup();
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_replace_single_line() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "line1\nline2\nline3\n");

        let mut result = Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "start_line": 2, "end_line": 2, "new_content": "REPLACED"}"#,
        )
        .unwrap();
        let output = result.poll_done();
        assert!(output.contains("Replaced"));
        assert_eq!(read_file(&dir, "f.txt"), "line1\nREPLACED\nline3\n");
    }

    #[test]
    fn test_replace_range() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\nc\nd\ne\n");

        Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "start_line": 2, "end_line": 4, "new_content": "X\nY"}"#,
        )
        .unwrap()
        .poll_done();
        assert_eq!(read_file(&dir, "f.txt"), "a\nX\nY\ne\n");
    }

    #[test]
    fn test_delete_lines() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\nc\n");

        Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "start_line": 2, "end_line": 2, "new_content": ""}"#,
        )
        .unwrap();
        assert_eq!(read_file(&dir, "f.txt"), "a\nc\n");
    }

    #[test]
    fn test_insert_at_end() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\n");

        // 替换超出行范围的"append"行为（替换不存在的行就变成 append）
        Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "start_line": 3, "end_line": 3, "new_content": "c"}"#,
        )
        .unwrap();
        assert_eq!(read_file(&dir, "f.txt"), "a\nb\nc\n");
    }

    #[test]
    fn test_missing_start_line() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "end_line": 1, "new_content": "x"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_nonexistent_file() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(
            &tool,
            r#"{"file_path": "nope.txt", "start_line": 1, "end_line": 1, "new_content": "x"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
