//! [`GlobTool`] — 文件模式匹配工具。
//!
//! 使用 glob 模式查找匹配的文件，返回排序后的相对路径列表。

use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[cfg(test)]
use tools::SandboxConfig;
use tools::WorkspaceFs;
use tools::{FsError, ProgressStream, ToolError, tool};

/// Glob 工具的参数。
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GlobArgs {
    /// Glob pattern relative to workspace root.
    #[schemars(
        description = "Glob pattern relative to workspace root. Use ** for recursive matching, * for any name segment, ? for single character. Examples: '**/*.rs', 'src/**/*.rs', '*.toml'. Always use forward slashes; backslashes are not valid."
    )]
    pub pattern: String,
}

/// 使用 glob 模式查找文件的工具。
///
/// # 参数
///
/// ```json
/// {"pattern": "**/*.rs"}
/// ```
#[tool(
    name = "glob",
    description = "Find files matching a glob pattern. Returns a sorted list of relative file \
         paths, one per line. Supports `**` for recursive directory matching.\n\n\
         When to use: finding files by name pattern (e.g. all .rs files), discovering \
         project structure before reading, checking if a file exists without knowing \
         its exact path.\n\n\
         When NOT to use: searching file contents (use grep), listing a single \
         directory (use ls — more readable output for one directory), reading a file \
         at a known path (use read directly).\n\n\
         Pattern examples:\n\
         - `**/*.rs` — all Rust files recursively\n\
         - `src/**/*.rs` — Rust files under src/ only\n\
         - `*.toml` — files in workspace root only (no recursion)\n\
         - `src/tui/*.rs` — files directly in src/tui/, non-recursive\n\n\
         Returns 'No files matched.' when nothing matches. Always use forward \
         slashes; backslashes are not valid glob separators.",
    args = GlobArgs
)]
pub struct GlobTool {
    fs: Arc<WorkspaceFs>,
}

impl GlobTool {
    pub fn new(fs: Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }

    fn execute_stream(&self, args: GlobArgs) -> Result<ProgressStream, ToolError> {
        let files = self.fs.glob(&args.pattern).map_err(map_fs_err)?;

        let output = if files.is_empty() {
            "No files matched.".to_string()
        } else {
            files.join("\n")
        };
        Ok(ProgressStream::done(output))
    }
}

fn map_fs_err(e: FsError) -> ToolError {
    match e {
        FsError::Glob(_) => ToolError::InvalidArgs(e.to_string()),
        _ => ToolError::Execution(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    fn setup() -> (tempfile::TempDir, GlobTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path(), &SandboxConfig::default()).unwrap();
        let tool = GlobTool::new(Arc::new(fs));
        (dir, tool)
    }

    fn write_file(dir: &tempfile::TempDir, path: &str, content: &str) {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    #[test]
    fn test_name() {
        let (_dir, tool) = setup();
        assert_eq!(tool.name(), "glob");
    }

    #[test]
    fn test_parameters_schema() {
        let (_dir, tool) = setup();
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_glob_rs_files() {
        let (dir, tool) = setup();
        write_file(&dir, "main.rs", "");
        write_file(&dir, "lib.rs", "");
        write_file(&dir, "Cargo.toml", "");

        let result = Tool::execute_stream(&tool, r#"{"pattern": "*.rs"}"#)
            .unwrap()
            .poll_done();
        assert!(result.contains("lib.rs"));
        assert!(result.contains("main.rs"));
        assert!(!result.contains("Cargo.toml"));
    }

    #[test]
    fn test_glob_recursive() {
        let (dir, tool) = setup();
        write_file(&dir, "src/a.rs", "");
        write_file(&dir, "src/deep/b.rs", "");
        write_file(&dir, "tests/c.rs", "");

        let result = Tool::execute_stream(&tool, r#"{"pattern": "**/*.rs"}"#)
            .unwrap()
            .poll_done();
        let normalized = result.replace('\\', "/");
        assert!(normalized.contains("src/a.rs"));
        assert!(normalized.contains("src/deep/b.rs"));
        assert!(normalized.contains("tests/c.rs"));
    }

    #[test]
    fn test_glob_no_match() {
        let (_dir, tool) = setup();
        let result = Tool::execute_stream(&tool, r#"{"pattern": "*.nonexistent"}"#)
            .unwrap()
            .poll_done();
        assert_eq!(result, "No files matched.");
    }

    #[test]
    fn test_missing_pattern() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(&tool, r#"{}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
