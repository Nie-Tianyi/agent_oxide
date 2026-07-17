//! [`WriteTool`] — 文件写入工具。
//!
//! 创建或覆写文件内容。自动创建缺失的父目录。
//!
//! 通过 [`Progress::InProgress`] 事件将写入内容流式预览到 TUI，
//! 让用户在工具执行期间即时看到正在写入的内容。

use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use tools::WorkspaceFs;
use tools::{FsError, Progress, ProgressStream, ToolError, tool};

#[cfg(test)]
use tools::SandboxConfig;

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
        // Validate and write synchronously first (errors surface immediately).
        self.fs
            .write(&args.file_path, &args.content)
            .map_err(map_fs_err)?;

        let file_path = args.file_path.clone();
        let content_len = args.content.len();
        let preview = content_preview(&args.content);

        // Stream progress events with small delays so the TUI can render
        // intermediate states before Done transitions to Complete.
        let (tx, rx) = mpsc::unbounded_channel::<Progress>();

        tokio::spawn(async move {
            tx.send(Progress::InProgress(format!(
                "Writing {} bytes to {}...",
                content_len, file_path
            )))
            .ok();
            tokio::time::sleep(Duration::from_millis(80)).await;

            if !preview.is_empty() {
                tx.send(Progress::InProgress(preview)).ok();
                tokio::time::sleep(Duration::from_millis(80)).await;
            }

            tx.send(Progress::Done(format!(
                "Wrote {} bytes to {}",
                content_len, file_path
            )))
            .ok();
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
        return String::new(); // empty content: skip preview
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
        format!("Content: {} (+{} more lines)", truncated, line_count - 1)
    } else {
        format!("Content: {}", truncated)
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
                    // Verify we emitted at least one InProgress.
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

    fn setup() -> (tempfile::TempDir, WriteTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path(), &SandboxConfig::default()).unwrap();
        let tool = WriteTool::new(Arc::new(fs));
        (dir, tool)
    }

    fn read_file(dir: &tempfile::TempDir, path: &str) -> String {
        std::fs::read_to_string(dir.path().join(path)).unwrap()
    }

    #[tokio::test]
    async fn test_name() {
        let (_dir, tool) = setup();
        assert_eq!(tool.name(), "write");
    }

    #[tokio::test]
    async fn test_description() {
        let (_dir, tool) = setup();
        assert!(tool.description().contains("workspace"));
    }

    #[tokio::test]
    async fn test_parameters_schema() {
        let (_dir, tool) = setup();
        let params = tool.parameter_schema();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[tokio::test]
    async fn test_write_new_file() {
        let (dir, tool) = setup();
        let stream = Tool::execute_stream(
            &tool,
            r#"{"file_path": "hello.txt", "content": "hello world"}"#,
        )
        .unwrap();
        let output = stream_done(stream).await;
        assert!(output.contains("hello.txt"));
        assert!(output.contains("11 bytes"));
        assert_eq!(read_file(&dir, "hello.txt"), "hello world");
    }

    #[tokio::test]
    async fn test_write_overwrite() {
        let (dir, tool) = setup();
        stream_done(
            Tool::execute_stream(&tool, r#"{"file_path": "f.txt", "content": "old"}"#).unwrap(),
        )
        .await;
        stream_done(
            Tool::execute_stream(&tool, r#"{"file_path": "f.txt", "content": "new"}"#).unwrap(),
        )
        .await;
        assert_eq!(read_file(&dir, "f.txt"), "new");
    }

    #[tokio::test]
    async fn test_write_nested_path() {
        let (dir, tool) = setup();
        stream_done(
            Tool::execute_stream(
                &tool,
                r#"{"file_path": "a/b/c/file.txt", "content": "deep"}"#,
            )
            .unwrap(),
        )
        .await;
        assert_eq!(read_file(&dir, "a/b/c/file.txt"), "deep");
    }

    #[tokio::test]
    async fn test_write_empty_content() {
        let (dir, tool) = setup();
        stream_done(
            Tool::execute_stream(&tool, r#"{"file_path": "empty.txt", "content": ""}"#).unwrap(),
        )
        .await;
        assert_eq!(read_file(&dir, "empty.txt"), "");
    }

    #[tokio::test]
    async fn test_missing_file_path() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(&tool, r#"{"content": "stuff"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn test_content_preview_single_line() {
        let preview = content_preview("hello world");
        assert_eq!(preview, "Content: hello world");
    }

    #[tokio::test]
    async fn test_content_preview_multi_line() {
        let preview = content_preview("line1\nline2\nline3");
        assert!(preview.contains("line1"));
        assert!(preview.contains("+2 more lines"));
    }

    #[tokio::test]
    async fn test_content_preview_long_line() {
        let long = "a".repeat(100);
        let preview = content_preview(&long);
        assert!(preview.ends_with("..."));
        assert!(preview.len() <= "Content: ".len() + 83); // 80 chars + "..."
    }

    #[tokio::test]
    async fn test_content_preview_empty() {
        assert!(content_preview("").is_empty());
    }
}
