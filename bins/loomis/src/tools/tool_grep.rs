//! [`GrepTool`] — 文件内容搜索工具。
//!
//! 使用正则表达式在文件内容中搜索，返回匹配的文件路径、行号和行内容。

use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[cfg(test)]
use tools::SandboxConfig;
use tools::WorkspaceFs;
use tools::{FsError, ProgressStream, ToolError, tool};

/// Grep 工具的参数。
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrepArgs {
    /// Regular expression to search for.
    #[schemars(
        description = "Regular expression to search for. Rust regex syntax. Examples: 'fn main', 'pub struct \\w+', 'TODO|FIXME', '(?i)error' for case-insensitive."
    )]
    pub pattern: String,

    /// Optional glob to limit which files to search.
    #[schemars(
        description = "Optional glob to limit which files to search. Default: all text files. Example: 'src/**/*.rs' to search only Rust sources. Use when searching broadly returns too much noise."
    )]
    pub path_glob: Option<String>,
}

/// 使用正则表达式搜索文件内容的工具。
///
/// # 参数
///
/// ```json
/// {"pattern": "fn\\s+main", "path_glob": "src/**/*.rs"}
/// ```
///
/// `path_glob` 是可选的；默认搜索所有文件。
#[tool(
    name = "grep",
    description = "Search file contents using a regular expression. Returns every matching \
         line with its file path and line number: `{path}:{line}: {content}`. Use \
         `path_glob` to limit the search to specific files.\n\n\
         When to use: finding where a function, type, or variable is defined or used; \
         searching for error messages or configuration keys; locating all occurrences \
         of a pattern before refactoring.\n\n\
         When NOT to use: finding files by name (use glob), reading a file's contents \
         (use read), searching a single known file (read it and scan — grep searches \
         ALL files and may return noise).\n\n\
         Pattern examples:\n\
         - `fn main` — literal substring match\n\
         - `pub struct \\w+` — regex for struct definitions\n\
         - `TODO|FIXME` — alternation\n\
         - `(?i)error` — case-insensitive (use (?i) prefix)\n\n\
         Returns 'No matches found.' when nothing matches.",
    args = GrepArgs
)]
pub struct GrepTool {
    fs: Arc<WorkspaceFs>,
}

impl GrepTool {
    pub fn new(fs: Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }

    fn execute_stream(&self, args: GrepArgs) -> Result<ProgressStream, ToolError> {
        let matches = self
            .fs
            .grep(&args.pattern, args.path_glob.as_deref())
            .map_err(map_fs_err)?;

        let output = if matches.is_empty() {
            "No matches found.".to_string()
        } else {
            matches
                .iter()
                .map(|m| format!("{}:{}: {}", m.file_path, m.line_number, m.line_content))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(ProgressStream::done(output))
    }
}

fn map_fs_err(e: FsError) -> ToolError {
    match e {
        FsError::Regex(_) => ToolError::InvalidArgs(e.to_string()),
        _ => ToolError::Execution(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    fn setup() -> (tempfile::TempDir, GrepTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path(), &SandboxConfig::default()).unwrap();
        let tool = GrepTool::new(Arc::new(fs));
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
        assert_eq!(tool.name(), "grep");
    }

    #[test]
    fn test_parameters_schema() {
        let (_dir, tool) = setup();
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_grep_find_function() {
        let (dir, tool) = setup();
        write_file(
            &dir,
            "a.rs",
            "fn main() {\n    let x = 1;\n}\nfn test() {}\n",
        );

        let result = Tool::execute_stream(&tool, r#"{"pattern": "fn "}"#)
            .unwrap()
            .poll_done();
        assert!(result.contains("fn main()"));
        assert!(result.contains("fn test()"));
        assert!(!result.contains("let x"));
    }

    #[test]
    fn test_grep_with_path_glob() {
        let (dir, tool) = setup();
        write_file(&dir, "src/lib.rs", "pub fn add() {}");
        write_file(&dir, "tests/test.rs", "fn it_works() {}");

        let result =
            Tool::execute_stream(&tool, r#"{"pattern": "fn", "path_glob": "src/**/*.rs"}"#)
                .unwrap()
                .poll_done();
        let normalized = result.replace('\\', "/");
        assert!(normalized.contains("src/lib.rs"));
        assert!(!normalized.contains("tests/"));
    }

    #[test]
    fn test_grep_no_match() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "hello world\n");

        let result = Tool::execute_stream(&tool, r#"{"pattern": "NOMATCH"}"#)
            .unwrap()
            .poll_done();
        assert!(result.contains("No matches"));
    }

    #[test]
    fn test_grep_invalid_regex() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(&tool, r#"{"pattern": "[unclosed"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_missing_pattern() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(&tool, r#"{}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_grep_output_format() {
        let (dir, tool) = setup();
        write_file(&dir, "test.rs", "fn hello() {\n    println!();\n}\n");

        let result = Tool::execute_stream(&tool, r#"{"pattern": "fn"}"#)
            .unwrap()
            .poll_done();
        // 格式: file_path:line_number: line_content
        assert!(result.contains("test.rs:1: fn hello()"));
    }
}
