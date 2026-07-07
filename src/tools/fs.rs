//! [`WorkspaceFs`] — 带沙箱的文件系统操作。
//!
//! 所有路径操作都通过 [`WorkspaceFs::resolve`] 进行，确保路径
//! 不会逃逸出 `workspace_root`。
//!
//! # 设计
//!
//! `WorkspaceFs` 提供底层文件操作，返回 [`FsError`]。
//! 每个工具（[`ReadTool`], [`WriteTool`] 等）持有 `Arc<WorkspaceFs>`，
//! 在其 [`Tool::execute`] 中将 [`FsError`] 转换为 [`ToolError`]。
//!
//! # 路径解析
//!
//! ```text
//! 输入路径 → join(workspace_root) → canonicalize → 验证前缀 = workspace_root
//!                                                         ↓
//!                                                   FsError::PathEscapesWorkspace
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use super::error::FsError;

// ── WorkspaceFs ────────────────────────────────────────────────────────────

/// 带沙箱的文件系统操作句柄。
///
/// 所有操作都被限制在 `workspace_root` 目录内。
/// 路径在操作前会被规范化并验证。
#[derive(Debug)]
pub struct WorkspaceFs {
    workspace_root: PathBuf,
}

impl WorkspaceFs {
    /// 创建新的工作空间文件系统句柄。
    ///
    /// 验证 `root` 存在且为目录。规范化路径后存储。
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, FsError> {
        let root: PathBuf = root.into();

        // 如果路径不存在，尝试规范化也无效，直接检查存在性
        if !root.try_exists().map_err(FsError::Io)? {
            return Err(FsError::NotFound(root.display().to_string()));
        }
        if !root.is_dir() {
            return Err(FsError::NotADirectory(root.display().to_string()));
        }

        let workspace_root = root.canonicalize().map_err(FsError::Io)?;

        Ok(Self { workspace_root })
    }

    /// 返回工作空间根目录。
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    // ── 核心：路径解析与边界检查 ─────────────────────────────

    /// 将相对路径解析为工作空间内的绝对路径。
    ///
    /// 规范化路径后，验证其前缀等于 `workspace_root`。
    /// 空字符串视为指向根目录本身。
    fn resolve(&self, path: &str) -> Result<PathBuf, FsError> {
        // 空路径 → 工作空间根
        let joined = if path.is_empty() {
            self.workspace_root.clone()
        } else {
            self.workspace_root.join(path)
        };

        // canonicalize 要求路径存在。
        // resolve 不要求目标存在（例如 write 会先创建文件），
        // 因此先尝试 canonicalize，文件不存在时再手动归一化。
        let normalized = match joined.canonicalize() {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 手动归一化：先规范化存在的父目录，再拼接文件名
                normalize_partial(&joined)?
            }
            Err(e) => return Err(FsError::Io(e)),
        };

        // 边界检查：规范化路径必须以 workspace_root 开头
        if !normalized.starts_with(&self.workspace_root) {
            return Err(FsError::PathEscapesWorkspace(format!(
                "'{}' resolves outside workspace",
                path
            )));
        }

        Ok(normalized)
    }

    // ── 文件读取 ─────────────────────────────────────────────

    /// 读取文件内容。
    ///
    /// - `path`: 相对于工作空间根目录的路径
    /// - `offset`: 起始行（1-indexed，`None` 表示从第 1 行开始）
    /// - `limit`: 最大行数（`None` 表示不限制）
    pub fn read(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, FsError> {
        let resolved = self.resolve(path)?;

        if !resolved.exists() {
            return Err(FsError::NotFound(path.to_string()));
        }

        if !resolved.is_file() {
            return Err(FsError::NotAFile(path.to_string()));
        }

        let content = fs::read_to_string(&resolved).map_err(FsError::Io)?;

        let all_lines: Vec<&str> = content.lines().collect();

        let start = offset.map(|o| o.saturating_sub(1)).unwrap_or(0);
        let end = limit
            .map(|l| (start + l).min(all_lines.len()))
            .unwrap_or(all_lines.len());

        if start >= all_lines.len() {
            return Ok(String::new());
        }

        let selected = &all_lines[start..end];

        // 格式化为 cat -n 风格的行号输出
        let numbered: String = selected
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(numbered)
    }

    // ── 文件写入 ─────────────────────────────────────────────

    /// 创建或覆写文件。
    ///
    /// 自动创建缺失的父目录。
    pub fn write(&self, path: &str, content: &str) -> Result<(), FsError> {
        let resolved = self.resolve(path)?;

        // 不允许写入目录
        if resolved.exists() && resolved.is_dir() {
            return Err(FsError::NotAFile(path.to_string()));
        }

        // 确保父目录存在
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(FsError::Io)?;
        }

        fs::write(&resolved, content).map_err(FsError::Io)?;

        Ok(())
    }

    // ── 行级编辑 ─────────────────────────────────────────────

    /// 替换文件中的指定行范围。
    ///
    /// - `start`: 1-indexed 起始行（含）
    /// - `end`: 1-indexed 结束行（含）
    /// - `new_content`: 用于替换的文本（可以为空字符串以删除行）
    ///
    /// 返回变更摘要。
    pub fn edit_lines(
        &self,
        path: &str,
        start: usize,
        end: usize,
        new_content: &str,
    ) -> Result<String, FsError> {
        if start == 0 || end == 0 {
            return Err(FsError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "line numbers are 1-indexed; 0 is invalid",
            )));
        }
        if start > end {
            return Err(FsError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("start ({start}) > end ({end})"),
            )));
        }

        let resolved = self.resolve(path)?;

        if !resolved.is_file() {
            return Err(FsError::NotAFile(path.to_string()));
        }

        let content = fs::read_to_string(&resolved).map_err(FsError::Io)?;

        // Detect the file's line-end style so we preserve it.
        let line_end = detect_line_end(&content);

        let lines: Vec<&str> = content.lines().collect();

        let start_idx = start - 1; // 转为 0-indexed
        let end_idx = end - 1;

        let mut new_lines: Vec<String> = Vec::new();

        // 保留 start 之前的行
        for line in lines.iter().take(start_idx.min(lines.len())) {
            new_lines.push(line.to_string());
        }

        // 插入替换内容
        if !new_content.is_empty() {
            for line in new_content.lines() {
                new_lines.push(line.to_string());
            }
        }

        // 保留 end 之后的行
        for line in lines.iter().skip(end_idx + 1) {
            new_lines.push(line.to_string());
        }

        let new_file = new_lines.join(&line_end);

        // 如果原文件以换行结尾，则新文件也保持
        let new_file = if content.ends_with('\n') && !new_file.ends_with('\n') {
            new_file + &line_end
        } else {
            new_file
        };

        fs::write(&resolved, &new_file).map_err(FsError::Io)?;

        Ok(format!(
            "Replaced lines {start}-{end} in {path} ({} lines removed, {} lines inserted)",
            (end_idx - start_idx + 1).min(lines.len().saturating_sub(start_idx)),
            new_content.lines().count(),
        ))
    }

    // ── glob 文件匹配 ────────────────────────────────────────

    /// 使用 glob 模式匹配文件。
    ///
    /// 模式相对于工作空间根目录。返回排序后的相对路径列表。
    pub fn glob(&self, pattern: &str) -> Result<Vec<String>, FsError> {
        let full_pattern = self.workspace_root.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let entries = glob::glob(&pattern_str)
            .map_err(FsError::from)?
            .filter_map(|entry| entry.ok())
            .filter(|p| p.is_file())
            .filter_map(|p| {
                p.strip_prefix(&self.workspace_root)
                    .ok()
                    .map(|rel| rel.to_string_lossy().to_string())
            })
            .collect::<Vec<String>>();

        let mut entries = entries;
        entries.sort();
        Ok(entries)
    }

    // ── grep 内容搜索 ────────────────────────────────────────

    /// 在文件中搜索正则表达式。
    ///
    /// - `pattern`: 正则表达式
    /// - `path_glob`: 可选的文件过滤 glob。默认搜索所有文件。
    pub fn grep(&self, pattern: &str, path_glob: Option<&str>) -> Result<Vec<GrepMatch>, FsError> {
        let re = regex::Regex::new(pattern).map_err(FsError::from)?;
        let glob_pattern = path_glob.unwrap_or("**/*");
        let files = self.glob(glob_pattern)?;

        let mut matches = Vec::new();

        for file_path in &files {
            let resolved = self.resolve(file_path)?;
            let content = fs::read_to_string(&resolved).map_err(FsError::Io)?;

            for (line_num, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    matches.push(GrepMatch {
                        file_path: file_path.clone(),
                        line_number: line_num + 1,
                        line_content: line.to_string(),
                    });
                }
            }
        }

        Ok(matches)
    }

    // ── 目录列表 ─────────────────────────────────────────────

    /// 列出目录内容。
    ///
    /// - `path`: 相对于工作空间根目录的路径。`None` 或空字符串表示根目录。
    pub fn ls(&self, path: Option<&str>) -> Result<Vec<DirEntry>, FsError> {
        let resolved = self.resolve(path.unwrap_or(""))?;

        if !resolved.is_dir() {
            return Err(FsError::NotADirectory(path.unwrap_or("").to_string()));
        }

        let mut entries = Vec::new();

        let dir = fs::read_dir(&resolved).map_err(FsError::Io)?;
        for entry in dir {
            let entry = entry.map_err(FsError::Io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let metadata = entry.metadata().map_err(FsError::Io)?;
            let entry_type = if metadata.is_dir() {
                EntryType::Dir
            } else if metadata.is_symlink() {
                EntryType::Symlink
            } else {
                EntryType::File
            };
            let size = metadata.len();
            entries.push(DirEntry {
                name,
                entry_type,
                size,
            });
        }

        // 按名称排序，目录优先
        entries.sort_by(|a, b| {
            use std::cmp::Ordering;
            match (a.entry_type, b.entry_type) {
                (EntryType::Dir, EntryType::Dir)
                | (EntryType::File, EntryType::File)
                | (EntryType::Symlink, EntryType::Symlink) => a.name.cmp(&b.name),
                (EntryType::Dir, _) => Ordering::Less,
                (_, EntryType::Dir) => Ordering::Greater,
                _ => a.name.cmp(&b.name),
            }
        });

        Ok(entries)
    }
}

// ── 辅助类型 ───────────────────────────────────────────────────────────────

/// 目录条目。
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub entry_type: EntryType,
    pub size: u64,
}

/// 条目类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    File,
    Dir,
    Symlink,
}

/// grep 匹配结果。
#[derive(Debug, Clone)]
pub struct GrepMatch {
    pub file_path: String,
    pub line_number: usize,
    pub line_content: String,
}

// ── 内部辅助函数 ──────────────────────────────────────────────────────────

/// Detect the line-ending style used in `text`.
///
/// Returns `"\r\n"` if any CRLF sequence is found, otherwise `"\n"`.
/// This ensures `edit_lines` preserves the file's original line style.
fn detect_line_end(text: &str) -> String {
    if text.contains("\r\n") {
        "\r\n".to_string()
    } else {
        "\n".to_string()
    }
}

/// 对不存在的文件做部分路径规范化。
///
/// 找到路径中最长的已存在前缀并 canonicalize，
/// 然后拼接剩余部分。途中遇到 `..` 会从规范化前缀中移除最后一级，
/// 遇到 `.` 则直接跳过，从而防止 `..` 逃逸出 workspace。
fn normalize_partial(path: &Path) -> Result<PathBuf, FsError> {
    // Walk up from the path to find the first existing ancestor,
    // canonicalize it, then re-append the non-existent tail.
    let mut existing = path.to_path_buf();
    let mut tail_components: Vec<std::path::PathBuf> = Vec::new();

    loop {
        if existing.exists() {
            let canon = existing.canonicalize().map_err(FsError::Io)?;
            let mut result = canon;
            // Re-apply tail components, resolving `..` and `.` to prevent
            // path-traversal attacks through non-existent intermediate dirs.
            for comp in tail_components.iter().rev() {
                if comp == std::path::Path::new("..") {
                    // `..` escapes upward — pop from the canonicalized prefix.
                    // Guard against popping past the filesystem root.
                    if result.parent().is_some() {
                        result.pop();
                    }
                } else if comp != std::path::Path::new(".") {
                    result.push(comp);
                }
                // `.` is a no-op — skip it.
            }
            return Ok(result);
        }
        if let (Some(parent), Some(file_name)) = (existing.parent(), existing.file_name()) {
            tail_components.push(std::path::PathBuf::from(file_name));
            existing = parent.to_path_buf();
        } else {
            // No parent — path is relative and nothing exists.
            // Return the original path (boundary check will catch issues).
            return Ok(path.to_path_buf());
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 创建临时目录并初始化 WorkspaceFs。
    fn setup_fs() -> (tempfile::TempDir, WorkspaceFs) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path()).unwrap();
        (dir, fs)
    }

    // ── new / 边界检查 ──────────────────────────────────────

    #[test]
    fn test_new_valid_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(WorkspaceFs::new(dir.path()).is_ok());
    }

    #[test]
    fn test_new_nonexistent() {
        let result = WorkspaceFs::new("/tmp/__agent_oxide_nonexistent_dir__");
        assert!(matches!(result, Err(FsError::NotFound(_))));
    }

    #[test]
    fn test_path_escapes_workspace() {
        let (_dir, fs) = setup_fs();
        let result = fs.read("../outside_file.txt", None, None);
        assert!(matches!(result, Err(FsError::PathEscapesWorkspace(_))));
    }

    #[test]
    fn test_path_with_dotdot_rejected() {
        let (_dir, fs) = setup_fs();
        // 创建子目录，尝试通过 .. 逃脱
        let sub = _dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let result = fs.read("sub/../../../etc/passwd", None, None);
        assert!(matches!(result, Err(FsError::PathEscapesWorkspace(_))));
    }

    /// Regression: path traversal through a non-existent intermediate
    /// directory should be caught by `normalize_partial` resolving `..`
    /// components rather than treating them as literal path segments.
    #[test]
    fn test_path_traversal_via_nonexistent_dir() {
        let (_dir, fs) = setup_fs();
        let result = fs.write(
            "nonexistent/../../../Windows/System32/evil.exe",
            "malicious",
        );
        assert!(
            matches!(result, Err(FsError::PathEscapesWorkspace(_))),
            "traversal via nonexistent dir should be rejected; got {result:?}"
        );
    }

    // ── read ─────────────────────────────────────────────────

    #[test]
    fn test_read_simple() {
        let (_dir, fs) = setup_fs();
        fs.write("test.txt", "hello\nworld\n").unwrap();

        let result = fs.read("test.txt", None, None).unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
    }

    #[test]
    fn test_read_with_offset() {
        let (_dir, fs) = setup_fs();
        fs.write("test.txt", "line1\nline2\nline3\nline4\n")
            .unwrap();

        let result = fs.read("test.txt", Some(2), None).unwrap();
        assert!(!result.contains("line1"));
        assert!(result.contains("line2"));
    }

    #[test]
    fn test_read_with_limit() {
        let (_dir, fs) = setup_fs();
        fs.write("test.txt", "line1\nline2\nline3\nline4\n")
            .unwrap();

        let result = fs.read("test.txt", None, Some(2)).unwrap();
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(!result.contains("line3"));
    }

    #[test]
    fn test_read_with_offset_and_limit() {
        let (_dir, fs) = setup_fs();
        fs.write("test.txt", "line1\nline2\nline3\nline4\n")
            .unwrap();

        let result = fs.read("test.txt", Some(2), Some(2)).unwrap();
        assert!(!result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
        assert!(!result.contains("line4"));
    }

    #[test]
    fn test_read_not_found() {
        let (_dir, fs) = setup_fs();
        let result = fs.read("nonexistent.txt", None, None);
        assert!(matches!(result, Err(FsError::NotFound(_))));
    }

    #[test]
    fn test_read_directory() {
        let (_dir, fs) = setup_fs();
        fs::create_dir(_dir.path().join("subdir")).unwrap();
        let result = fs.read("subdir", None, None);
        assert!(matches!(result, Err(FsError::NotAFile(_))));
    }

    #[test]
    fn test_read_offset_beyond_file() {
        let (_dir, fs) = setup_fs();
        fs.write("test.txt", "only\n").unwrap();
        let result = fs.read("test.txt", Some(100), None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_read_empty_file() {
        let (_dir, fs) = setup_fs();
        fs.write("empty.txt", "").unwrap();
        let result = fs.read("empty.txt", None, None).unwrap();
        assert!(result.is_empty());
    }

    // ── write ────────────────────────────────────────────────

    #[test]
    fn test_write_new_file() {
        let (_dir, fs) = setup_fs();
        fs.write("new.txt", "hello world").unwrap();
        let content = fs::read_to_string(_dir.path().join("new.txt")).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_write_overwrite() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "old").unwrap();
        fs.write("f.txt", "new").unwrap();
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "new");
    }

    #[test]
    fn test_write_creates_parent_dirs() {
        let (_dir, fs) = setup_fs();
        fs.write("a/b/c/file.txt", "nested").unwrap();
        let content = fs::read_to_string(_dir.path().join("a/b/c/file.txt")).unwrap();
        assert_eq!(content, "nested");
    }

    // ── edit_lines ───────────────────────────────────────────

    #[test]
    fn test_edit_single_line() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "line1\nline2\nline3\n").unwrap();
        fs.edit_lines("f.txt", 2, 2, "replaced").unwrap();
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "line1\nreplaced\nline3\n");
    }

    #[test]
    fn test_edit_range() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "a\nb\nc\nd\ne\n").unwrap();
        fs.edit_lines("f.txt", 2, 4, "X\nY").unwrap();
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "a\nX\nY\ne\n");
    }

    #[test]
    fn test_edit_delete_lines() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "a\nb\nc\n").unwrap();
        fs.edit_lines("f.txt", 2, 2, "").unwrap();
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "a\nc\n");
    }

    #[test]
    fn test_edit_zero_line_invalid() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "a\n").unwrap();
        let result = fs.edit_lines("f.txt", 0, 1, "x");
        assert!(matches!(result, Err(FsError::Io(_))));
    }

    #[test]
    fn test_edit_start_gt_end() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "a\nb\n").unwrap();
        let result = fs.edit_lines("f.txt", 3, 1, "x");
        assert!(matches!(result, Err(FsError::Io(_))));
    }

    #[test]
    fn test_edit_nonexistent_file() {
        let (_dir, fs) = setup_fs();
        let result = fs.edit_lines("nope.txt", 1, 1, "x");
        assert!(matches!(result, Err(FsError::NotAFile(_))));
    }

    // ── glob ─────────────────────────────────────────────────

    #[test]
    fn test_glob_basic() {
        let (_dir, fs) = setup_fs();
        fs.write("a.rs", "").unwrap();
        fs.write("b.rs", "").unwrap();
        fs.write("c.txt", "").unwrap();

        let results = fs.glob("*.rs").unwrap();
        assert_eq!(results, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn test_glob_nested() {
        let (_dir, fs) = setup_fs();
        fs.write("src/lib.rs", "").unwrap();
        fs.write("src/main.rs", "").unwrap();
        fs.write("test.rs", "").unwrap();

        let results = fs.glob("**/*.rs").unwrap();
        let normalized: Vec<String> = results.iter().map(|p| p.replace('\\', "/")).collect();
        assert!(normalized.contains(&"src/lib.rs".to_string()));
        assert!(normalized.contains(&"src/main.rs".to_string()));
        assert!(normalized.contains(&"test.rs".to_string()));
    }

    #[test]
    fn test_glob_no_match() {
        let (_dir, fs) = setup_fs();
        let results = fs.glob("*.rs").unwrap();
        assert!(results.is_empty());
    }

    // ── grep ─────────────────────────────────────────────────

    #[test]
    fn test_grep_basic() {
        let (_dir, fs) = setup_fs();
        fs.write("a.rs", "fn main() {\n    println!(\"hello\");\n}\n")
            .unwrap();
        fs.write("b.rs", "fn test() {}\n").unwrap();

        let results = fs.grep("fn", None).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].file_path, "a.rs");
        assert_eq!(results[0].line_number, 1);
    }

    #[test]
    fn test_grep_with_path_glob() {
        let (_dir, fs) = setup_fs();
        fs.write("src/a.rs", "fn a() {}").unwrap();
        fs.write("tests/b.rs", "fn b() {}").unwrap();

        let results = fs.grep("fn", Some("src/**/*.rs")).unwrap();
        assert_eq!(results.len(), 1);
        let path = results[0].file_path.replace('\\', "/");
        assert_eq!(path, "src/a.rs");
    }

    #[test]
    fn test_grep_invalid_regex() {
        let (_dir, fs) = setup_fs();
        let result = fs.grep("[unclosed", None);
        assert!(matches!(result, Err(FsError::Regex(_))));
    }

    // ── ls ───────────────────────────────────────────────────

    #[test]
    fn test_ls_root() {
        let (_dir, fs) = setup_fs();
        fs.write("a.txt", "").unwrap();
        fs::create_dir(_dir.path().join("sub")).unwrap();

        let entries = fs.ls(None).unwrap();
        assert_eq!(entries.len(), 2);
        // 目录优先
        assert_eq!(entries[0].name, "sub");
        assert_eq!(entries[0].entry_type, EntryType::Dir);
        assert_eq!(entries[1].name, "a.txt");
        assert_eq!(entries[1].entry_type, EntryType::File);
    }

    #[test]
    fn test_ls_subdirectory() {
        let (_dir, fs) = setup_fs();
        fs.write("sub/a.txt", "").unwrap();
        fs.write("sub/b.txt", "").unwrap();

        let entries = fs.ls(Some("sub")).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_ls_empty() {
        let (_dir, fs) = setup_fs();
        let entries = fs.ls(None).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_ls_not_a_directory() {
        let (_dir, fs) = setup_fs();
        fs.write("file.txt", "").unwrap();
        let result = fs.ls(Some("file.txt"));
        assert!(matches!(result, Err(FsError::NotADirectory(_))));
    }
}
