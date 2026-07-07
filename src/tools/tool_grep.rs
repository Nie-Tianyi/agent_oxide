//! [`GrepTool`] — 文件内容搜索工具。
//!
//! 使用正则表达式在文件内容中搜索，返回匹配的文件路径、行号和行内容。

use serde_json::Value;

use super::fs::WorkspaceFs;
use super::tool::{Tool, extract_string_arg};
use super::{FsError, ToolError};

/// 使用正则表达式搜索文件内容的工具。
///
/// # 参数
///
/// ```json
/// {"pattern": "fn\\s+main", "path_glob": "src/**/*.rs"}
/// ```
///
/// `path_glob` 是可选的；默认搜索所有文件。
pub struct GrepTool {
    fs: std::sync::Arc<WorkspaceFs>,
}

impl GrepTool {
    pub fn new(fs: std::sync::Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }
}

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents using a regular expression. Returns every matching \
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
         Returns 'No matches found.' when nothing matches."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for. Rust regex syntax. Examples: 'fn main', 'pub struct \\w+', 'TODO|FIXME', '(?i)error' for case-insensitive."
                },
                "path_glob": {
                    "type": "string",
                    "description": "Optional glob to limit which files to search. Default: all text files. Example: 'src/**/*.rs' to search only Rust sources. Use when searching broadly returns too much noise."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn execute(&self, args: &str) -> Result<String, ToolError> {
        let pattern = extract_string_arg(args, "pattern")?;

        let v: Value = serde_json::from_str(args)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid JSON: {e}")))?;

        let path_glob = v
            .get("path_glob")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());

        let matches = self
            .fs
            .grep(&pattern, path_glob.as_deref())
            .map_err(map_fs_err)?;

        if matches.is_empty() {
            return Ok("No matches found.".to_string());
        }

        let output: String = matches
            .iter()
            .map(|m| format!("{}:{}: {}", m.file_path, m.line_number, m.line_content))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(output)
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
    use std::sync::Arc;

    fn setup() -> (tempfile::TempDir, GrepTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path()).unwrap();
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
    fn test_grep_find_function() {
        let (dir, tool) = setup();
        write_file(
            &dir,
            "a.rs",
            "fn main() {\n    let x = 1;\n}\nfn test() {}\n",
        );

        let result = tool.execute(r#"{"pattern": "fn "}"#).unwrap();
        assert!(result.contains("fn main()"));
        assert!(result.contains("fn test()"));
        assert!(!result.contains("let x"));
    }

    #[test]
    fn test_grep_with_path_glob() {
        let (dir, tool) = setup();
        write_file(&dir, "src/lib.rs", "pub fn add() {}");
        write_file(&dir, "tests/test.rs", "fn it_works() {}");

        let result = tool
            .execute(r#"{"pattern": "fn", "path_glob": "src/**/*.rs"}"#)
            .unwrap();
        let normalized = result.replace('\\', "/");
        assert!(normalized.contains("src/lib.rs"));
        assert!(!normalized.contains("tests/"));
    }

    #[test]
    fn test_grep_no_match() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "hello world\n");

        let result = tool.execute(r#"{"pattern": "NOMATCH"}"#).unwrap();
        assert!(result.contains("No matches"));
    }

    #[test]
    fn test_grep_invalid_regex() {
        let (_dir, tool) = setup();
        let err = tool.execute(r#"{"pattern": "[unclosed"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_missing_pattern() {
        let (_dir, tool) = setup();
        let err = tool.execute(r#"{}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_grep_output_format() {
        let (dir, tool) = setup();
        write_file(&dir, "test.rs", "fn hello() {\n    println!();\n}\n");

        let result = tool.execute(r#"{"pattern": "fn"}"#).unwrap();
        // 格式: file_path:line_number: line_content
        assert!(result.contains("test.rs:1: fn hello()"));
    }
}
