//! [`EditTool`] — 行级文件编辑工具。
//!
//! 替换文件中指定行范围的内容。支持删除行（传入空字符串）。
//!
//! 通过 [`Progress::InProgress`] 事件将替换内容流式预览到 TUI，
//! 让用户在工具执行期间即时看到编辑的内容。

use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use tools::WorkspaceFs;
use tools::{FsError, Progress, ProgressStream, ToolError, tool};

#[cfg(test)]
use tools::SandboxConfig;

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
        // Validate and edit synchronously first (errors surface immediately).
        let output = self
            .fs
            .edit_lines(
                &args.file_path,
                args.start_line as usize,
                args.end_line as usize,
                &args.new_content,
            )
            .map_err(map_fs_err)?;

        let file_path = args.file_path.clone();
        let start = args.start_line;
        let end = args.end_line;
        let range_label = if start == end {
            format!("line {}", start)
        } else {
            format!("lines {}-{}", start, end)
        };
        let preview = content_preview(&args.new_content);

        // Stream progress events with small delays so the TUI can render
        // intermediate states before Done transitions to Complete.
        let (tx, rx) = mpsc::unbounded_channel::<Progress>();

        tokio::spawn(async move {
            tx.send(Progress::InProgress(format!(
                "Editing {}: {}...",
                file_path, range_label
            )))
            .ok();
            tokio::time::sleep(Duration::from_millis(80)).await;

            if !preview.is_empty() {
                tx.send(Progress::InProgress(preview)).ok();
                tokio::time::sleep(Duration::from_millis(80)).await;
            }

            tx.send(Progress::Done(output)).ok();
        });

        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(ProgressStream::new(Box::pin(stream)))
    }
}

/// Build a single-line content preview for progress display.
///
/// Shows the first non-empty line, truncated to 80 characters.
/// Appends a line-count hint for multi-line content.
fn content_preview(content: &str) -> String {
    if content.is_empty() {
        return String::new(); // empty replacement (delete): skip preview
    }

    let first_line = content.lines().next().unwrap_or("");
    let line_count = content.lines().count();

    let truncated = if first_line.len() > 80 {
        let boundary = first_line.floor_char_boundary(77);
        format!("{}...", &first_line[..boundary])
    } else {
        first_line.to_string()
    };

    if line_count > 1 {
        format!(
            "Replace with: {} (+{} more lines)",
            truncated,
            line_count - 1
        )
    } else {
        format!("Replace with: {}", truncated)
    }
}

fn map_fs_err(e: FsError) -> ToolError {
    match e {
        FsError::NotAFile(_) | FsError::WorkspaceEscape(_) => ToolError::InvalidArgs(e.to_string()),
        _ => ToolError::Execution(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tools::Tool;

    /// Drive a progress stream to completion, collecting all messages.
    /// Returns the final `Done` payload.
    async fn stream_done(mut stream: ProgressStream) -> String {
        let mut in_progress = vec![];
        while let Some(progress) = stream.next().await {
            match progress {
                Progress::InProgress(msg) => in_progress.push(msg),
                Progress::Done(output) => {
                    assert!(
                        !in_progress.is_empty(),
                        "expected at least one InProgress before Done"
                    );
                    return output;
                }
            }
        }
        panic!("stream ended without Progress::Done");
    }

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

    #[tokio::test]
    async fn test_name() {
        let (_dir, tool) = setup();
        assert_eq!(tool.name(), "edit");
    }

    #[tokio::test]
    async fn test_parameters_schema() {
        let (_dir, tool) = setup();
        let params = tool.parameter_schema();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[tokio::test]
    async fn test_replace_single_line() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "line1\nline2\nline3\n");

        let stream = Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "start_line": 2, "end_line": 2, "new_content": "REPLACED"}"#,
        )
        .unwrap();
        let output = stream_done(stream).await;
        assert!(output.contains("Replaced"));
        assert_eq!(read_file(&dir, "f.txt"), "line1\nREPLACED\nline3\n");
    }

    #[tokio::test]
    async fn test_replace_range() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\nc\nd\ne\n");

        stream_done(
            Tool::execute_stream(
                &tool,
                r#"{"file_path": "f.txt", "start_line": 2, "end_line": 4, "new_content": "X\nY"}"#,
            )
            .unwrap(),
        )
        .await;
        assert_eq!(read_file(&dir, "f.txt"), "a\nX\nY\ne\n");
    }

    #[tokio::test]
    async fn test_delete_lines() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\nc\n");

        stream_done(
            Tool::execute_stream(
                &tool,
                r#"{"file_path": "f.txt", "start_line": 2, "end_line": 2, "new_content": ""}"#,
            )
            .unwrap(),
        )
        .await;
        assert_eq!(read_file(&dir, "f.txt"), "a\nc\n");
    }

    #[tokio::test]
    async fn test_insert_at_end() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\n");

        // 替换超出行范围的"append"行为（替换不存在的行就变成 append）
        stream_done(
            Tool::execute_stream(
                &tool,
                r#"{"file_path": "f.txt", "start_line": 3, "end_line": 3, "new_content": "c"}"#,
            )
            .unwrap(),
        )
        .await;
        assert_eq!(read_file(&dir, "f.txt"), "a\nb\nc\n");
    }

    #[tokio::test]
    async fn test_missing_start_line() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "end_line": 1, "new_content": "x"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn test_nonexistent_file() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(
            &tool,
            r#"{"file_path": "nope.txt", "start_line": 1, "end_line": 1, "new_content": "x"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn test_content_preview_delete() {
        // Empty content (delete) should return empty preview.
        assert!(content_preview("").is_empty());
    }

    #[tokio::test]
    async fn test_content_preview_single_line() {
        let preview = content_preview("hello world");
        assert_eq!(preview, "Replace with: hello world");
    }

    #[tokio::test]
    async fn test_content_preview_multi_line() {
        let preview = content_preview("line1\nline2\nline3");
        assert!(preview.contains("line1"));
        assert!(preview.contains("+2 more lines"));
    }
}
