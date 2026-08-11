//! [`WorkspaceFs`] — sandboxed file-system operations.
//!
//! All path operations go through [`WorkspaceFs::resolve`], which ensures
//! paths cannot escape the `workspace_root` (and, for read-only operations,
//! the optional [`FilesystemConfig::read_only_paths`] roots).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::FilesystemConfig;

/// Sandboxed file-system handle.
///
/// Writes are confined to `workspace_root`. Reads (`read`, `ls`, `glob`,
/// `grep`) may additionally access `read_only_roots` — absolute directories
/// outside the workspace (e.g. the cargo registry cache) that are readable
/// but never writable.
///
/// Policy knobs (file-size caps, extension blocklist, hidden-file protection)
/// come from [`FilesystemConfig`] and are baked into the handle at construction.
#[derive(Debug)]
pub struct WorkspaceFs {
    workspace_root: PathBuf,
    /// Read-only directories outside the workspace (canonicalized where
    /// possible). Read operations may resolve into these; writes never can.
    read_only_roots: Vec<PathBuf>,
    max_read_bytes: usize,
    max_write_bytes: usize,
    forbid_binary_writes: bool,
    forbid_hidden_file_writes: bool,
    blocked_write_extensions: Vec<String>,
    /// Per-file write locks — serialize read-modify-write operations
    /// (`edit_lines`, `write`) targeting the same file so concurrent
    /// tool calls cannot silently overwrite each other's changes.
    write_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl WorkspaceFs {
    /// Create a new workspace file-system handle.
    ///
    /// Validates that `root` exists and is a directory, then canonicalizes
    /// it. Sandbox policies are taken from `config`.
    pub fn new(root: impl Into<PathBuf>, config: &FilesystemConfig) -> Result<Self, FsError> {
        let root: PathBuf = root.into();

        if !root.try_exists().map_err(FsError::Io)? {
            return Err(FsError::NotFound(root.display().to_string()));
        }
        if !root.is_dir() {
            return Err(FsError::NotADirectory(root.display().to_string()));
        }

        let workspace_root = root.canonicalize().map_err(FsError::Io)?;

        // Resolve read-only roots: absolute entries as-is, relative entries
        // against the workspace root. Canonicalize where possible (the path
        // may not exist yet). Empty entries are ignored — an empty root
        // would be a prefix of every path and silently allow all reads.
        let read_only_roots: Vec<PathBuf> = config
            .read_only_paths
            .iter()
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                let pb = PathBuf::from(p);
                let base = if pb.is_absolute() {
                    pb
                } else {
                    workspace_root.join(pb)
                };
                base.canonicalize().unwrap_or(base)
            })
            .collect();

        if !read_only_roots.is_empty() {
            tracing::debug!(roots = ?read_only_roots, "Read-only roots configured");
        }

        Ok(Self {
            workspace_root,
            read_only_roots,
            max_read_bytes: config.max_read_bytes,
            max_write_bytes: config.max_write_bytes,
            forbid_binary_writes: config.forbid_binary_writes,
            forbid_hidden_file_writes: config.forbid_hidden_file_writes,
            blocked_write_extensions: config.blocked_write_extensions.clone(),
            write_locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Check whether a file's extension is in the blocked list (e.g. `.exe`, `.dll`).
    fn is_extension_blocked(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|ext| {
                let dot_ext = format!(".{}", ext);
                self.blocked_write_extensions
                    .iter()
                    .any(|blocked| blocked.eq_ignore_ascii_case(&dot_ext))
            })
            .unwrap_or(false)
    }

    /// Heuristic: check whether raw bytes look like binary content.
    ///
    /// Scans the first 8 KiB for null bytes �?a reliable indicator of binary
    /// formats (executables, images, archives, etc.).
    fn is_likely_binary(bytes: &[u8]) -> bool {
        let check_len = bytes.len().min(8192);
        bytes[..check_len].contains(&0)
    }

    /// Acquire the per-file write lock for `resolved`.
    ///
    /// Returns an `Arc` clone of the file's mutex; the caller locks it while
    /// performing the read → modify → write sequence, so two concurrent
    /// operations on the same file serialize while different files proceed
    /// in parallel. The map lock is held only briefly to look up/create the
    /// per-file mutex, never while waiting on it — no nested locking, no
    /// deadlock.
    ///
    /// Recovers from a poisoned lock (a previous holder panicked) rather than
    /// propagating the panic, so one broken operation cannot permanently
    /// disable writes to that file.
    fn file_lock(&self, resolved: &Path) -> Arc<Mutex<()>> {
        let mut map = self.write_locks.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(resolved.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Resolve a path (write-mode): must stay within the workspace.
    fn resolve(&self, path: &str) -> Result<PathBuf, FsError> {
        self.resolve_within(path, &[&self.workspace_root])
    }

    /// Resolve a path (read-mode): may be within the workspace **or** any
    /// configured read-only root (absolute paths outside the sandbox, e.g.
    /// the cargo registry cache).
    fn resolve_read(&self, path: &str) -> Result<PathBuf, FsError> {
        let mut roots: Vec<&Path> = Vec::with_capacity(1 + self.read_only_roots.len());
        roots.push(&self.workspace_root);
        roots.extend(self.read_only_roots.iter().map(|p| p.as_path()));
        self.resolve_within(path, &roots)
    }

    /// Resolve a relative path to an absolute path within one of `roots`.
    ///
    /// On success the returned path is guaranteed to start with one of the
    /// allowed roots.  When the resolved path already exists on disk
    /// we also perform a **TOCTOU re-check** (see below).
    ///
    /// ## Known limitations
    ///
    /// 1. **Non-existing paths** bypass the TOCTOU re-check entirely —
    ///    if a file is created by an attacker between resolution and
    ///    the subsequent I/O operation, it will not be detected.
    /// 2. **File identity** is verified via `(len, modified)` heuristic
    ///    rather than platform-specific inode/file-index APIs. This is
    ///    not cryptographically strong — a determined attacker with
    ///    write access can craft a file with matching size and mtime.
    ///
    /// A truly race-free design would require handle-based I/O (open
    /// file, then `fstat` the handle).
    fn resolve_within(&self, path: &str, roots: &[&Path]) -> Result<PathBuf, FsError> {
        let joined = if path.is_empty() {
            self.workspace_root.clone()
        } else {
            self.workspace_root.join(path)
        };

        let normalized = match joined.canonicalize() {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => normalize_partial(&joined)?,
            Err(e) => return Err(FsError::Io(e)),
        };

        if !roots.iter().any(|root| normalized.starts_with(root)) {
            tracing::warn!(path = %path, "Path resolves outside sandbox roots — blocked");
            return Err(FsError::WorkspaceEscape(format!(
                "'{}' resolves outside the sandbox (workspace or read-only roots)",
                path
            )));
        }

        // ── TOCTOU re-check for existing paths ──────────────────────────
        // Re-canonicalize and verify the file identity hasn't changed.
        // We compare file length + modification time as a heuristic for
        // "same file" — this is NOT an inode/file-index comparison, and
        // can be defeated by a determined attacker with write access.
        // If the path didn't exist at the first canonicalize (normalize_partial
        // path), this re-check is skipped — new files are not covered.
        if let Ok(meta) = normalized.metadata() {
            let re_canon = normalized.canonicalize().map_err(FsError::Io)?;
            if !roots.iter().any(|root| re_canon.starts_with(root)) {
                tracing::error!(path = %path, "TOCTOU re-check failed: path escapes sandbox roots");
                return Err(FsError::WorkspaceEscape(format!(
                    "'{}' escapes workspace (TOCTOU re-check)",
                    path
                )));
            }
            // Compare file identity: same length + same modification time
            // is a decent heuristic for "same file" without platform-specific
            // inode APIs.
            if let Ok(re_meta) = re_canon.metadata()
                && (meta.len() != re_meta.len() || meta.modified().ok() != re_meta.modified().ok())
            {
                tracing::error!(
                    path = %path,
                    "TOCTOU re-check failed: file identity changed between checks — possible symlink swap"
                );
                return Err(FsError::WorkspaceEscape(format!(
                    "'{}' file identity changed between checks — possible symlink swap",
                    path
                )));
            }
        }

        Ok(normalized)
    }

    /// Read file content with optional `offset` (1-indexed line) and `limit`.
    ///
    /// Files larger than `max_read_bytes` are rejected before reading to
    /// avoid accidental OOM on huge files.
    pub fn read(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, FsError> {
        let resolved = self.resolve_read(path)?;

        if !resolved.exists() {
            return Err(FsError::NotFound(path.to_string()));
        }
        if !resolved.is_file() {
            return Err(FsError::NotAFile(path.to_string()));
        }

        // ── Size limit check ────────────────────────────────────────────
        let metadata = resolved.metadata().map_err(FsError::Io)?;
        let file_size = metadata.len();
        if file_size > self.max_read_bytes as u64 {
            tracing::warn!(
                path = %path,
                size = file_size,
                max = self.max_read_bytes as u64,
                "Read blocked: file exceeds max_read_bytes"
            );
            return Err(FsError::FileTooLarge {
                path: path.to_string(),
                size: file_size,
                max: self.max_read_bytes as u64,
            });
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
        let numbered: String = selected
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6} {}", start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        tracing::debug!(path = %path, size = file_size, "Read file");
        Ok(numbered)
    }

    /// Create or overwrite a file. Creates parent directories as needed.
    ///
    /// Enforces content size limits, extension blocklist, hidden-file
    /// protection, and binary-content detection (null-byte heuristic).
    ///
    /// **TOCTOU note**: There is a window between [`resolve`](Self::resolve)
    /// and the actual `fs::write` call. A symlink-swap in that window can
    /// bypass the path sandbox. See [`resolve`](Self::resolve) for details.
    pub fn write(&self, path: &str, content: &str) -> Result<(), FsError> {
        let resolved = self.resolve(path)?;

        // ── Content size limit ──────────────────────────────────────────
        if content.len() > self.max_write_bytes {
            tracing::warn!(
                path = %path,
                size = content.len(),
                max = self.max_write_bytes,
                "Write blocked: content exceeds max_write_bytes"
            );
            return Err(FsError::FileTooLarge {
                path: path.to_string(),
                size: content.len() as u64,
                max: self.max_write_bytes as u64,
            });
        }

        // ── Extension blocklist ─────────────────────────────────────────
        if let Some(ext) = resolved.extension().and_then(|e| e.to_str()) {
            let dot_ext = format!(".{}", ext);
            if self
                .blocked_write_extensions
                .iter()
                .any(|blocked| blocked.eq_ignore_ascii_case(&dot_ext))
            {
                tracing::warn!(
                    path = %path,
                    extension = %dot_ext,
                    "Write blocked: extension on blocklist"
                );
                return Err(FsError::ExtensionBlocked(path.to_string()));
            }
        }

        // ── Binary content detection ────────────────────────────────────
        if self.forbid_binary_writes && content.contains('\0') {
            tracing::warn!(path = %path, "Write blocked: binary content detected");
            return Err(FsError::BinaryContentDetected(path.to_string()));
        }

        // ── Hidden file protection ──────────────────────────────────────
        if self.forbid_hidden_file_writes
            && let Some(name) = resolved.file_name().and_then(|n| n.to_str())
            && name.starts_with('.')
        {
            tracing::warn!(path = %path, "Write blocked: hidden file");
            return Err(FsError::HiddenFileBlocked(path.to_string()));
        }

        // ── Serialize concurrent writes to the same file ───────────────
        // Without this, a parallel `edit` on the same file could read the
        // pre-write state and write back stale content, silently wiping
        // this write (or vice versa). The lock makes overlapping
        // operations apply in a clean order.
        let lock = self.file_lock(&resolved);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        if resolved.exists() && resolved.is_dir() {
            return Err(FsError::NotAFile(path.to_string()));
        }

        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(FsError::Io)?;
        }

        fs::write(&resolved, content).map_err(FsError::Io)?;
        tracing::debug!(path = %path, size = content.len(), "Wrote file");
        Ok(())
    }

    /// Replace lines `start..=end` (1-indexed) with `new_content`.
    ///
    /// **TOCTOU note**: There is a window between [`resolve`](Self::resolve)
    /// and the actual `fs::write` call. See [`resolve`](Self::resolve) for
    /// the limitations of our TOCTOU protection.
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

        // ── Serialize concurrent edits to the same file ────────────────
        // Two parallel `edit`/`write` tool calls on the same file would
        // each read-modify-write independently, and the later write would
        // silently clobber the earlier one while both report success.
        // Hold the per-file lock across the whole read → modify → write.
        // (`lock` must outlive `_guard`, hence the separate binding.)
        let lock = self.file_lock(&resolved);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        let content = fs::read_to_string(&resolved).map_err(FsError::Io)?;
        let line_end = detect_line_end(&content);
        let lines: Vec<&str> = content.lines().collect();

        let start_idx = start - 1;
        let end_idx = end - 1;
        let mut new_lines: Vec<String> = Vec::new();

        for line in lines.iter().take(start_idx.min(lines.len())) {
            new_lines.push(line.to_string());
        }

        if !new_content.is_empty() {
            for line in new_content.lines() {
                new_lines.push(line.to_string());
            }
        }

        for line in lines.iter().skip(end_idx + 1) {
            new_lines.push(line.to_string());
        }

        let new_file = new_lines.join(&line_end);
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

    /// Replace the unique occurrence of `old` with `new` in the file at
    /// `path`.
    ///
    /// `old` must appear in the file's CURRENT content exactly once:
    /// - zero matches → [`FsError::NoMatch`] — the file changed since the
    ///   caller's last read, or the needle is stale
    /// - multiple matches → [`FsError::AmbiguousMatch`] — the caller must
    ///   include more surrounding context to disambiguate
    ///
    /// This is the content-based counterpart of
    /// [`edit_lines`](Self::edit_lines). Matching against the file's
    /// *current* state turns stale reads into loud errors instead of silent
    /// corruption: the needle itself is the freshness check. Matching is
    /// CRLF-tolerant (`\r` ignored on both sides) because the `read` tool
    /// normalizes output to `\n`; a caller echoing read output back must
    /// not fail on Windows line endings. The file's own line endings are
    /// preserved, and `new`'s `\n` is converted to them.
    ///
    /// Returns the 1-indexed inclusive line range the replacement occupies
    /// in the NEW file.
    pub fn edit_content(&self, path: &str, old: &str, new: &str) -> Result<EditSpan, FsError> {
        let resolved = self.resolve(path)?;
        if !resolved.is_file() {
            return Err(FsError::NotAFile(path.to_string()));
        }
        if old.is_empty() {
            return Err(FsError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "old_content must not be empty",
            )));
        }

        // ── Serialize concurrent edits to the same file ────────────────
        // Same rationale as `edit_lines`: parallel `edit`/`write` calls on
        // the same file must apply in a clean order. Content matching also
        // means a stale second edit (whose needle was consumed by the
        // first) fails loudly instead of applying at wrong coordinates.
        let lock = self.file_lock(&resolved);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        let content = fs::read_to_string(&resolved).map_err(FsError::Io)?;
        let matches = find_crlf_tolerant(&content, old);
        match matches.as_slice() {
            [] => {
                let needle = old.chars().take(80).collect::<String>();
                return Err(FsError::NoMatch {
                    path: path.to_string(),
                    needle,
                });
            }
            [(_start, _end)] => {}
            _ => {
                return Err(FsError::AmbiguousMatch {
                    path: path.to_string(),
                    count: matches.len(),
                });
            }
        }

        let (start, end) = matches[0];
        let line_end = detect_line_end(&content);
        let new_content = if line_end == "\n" {
            new.to_string()
        } else {
            new.replace('\n', &line_end)
        };
        let edited = format!("{}{}{}", &content[..start], new_content, &content[end..]);
        fs::write(&resolved, &edited).map_err(FsError::Io)?;

        // Newline counts work for fragments that start or end mid-line,
        // which `lines().count()` misreports (a trailing partial line
        // counts as one).
        let start_line = content[..start].matches('\n').count() + 1;
        let end_line = start_line + new_content.matches('\n').count();
        Ok(EditSpan {
            start_line,
            end_line,
        })
    }

    /// Glob files matching a pattern. Relative patterns are matched against
    /// the workspace root; absolute patterns may target read-only roots.
    ///
    /// Workspace files are returned as workspace-relative paths (the
    /// contract other tools rely on); read-only-root files are returned as
    /// absolute paths (they cannot be expressed relative to the workspace).
    pub fn glob(&self, pattern: &str) -> Result<Vec<String>, FsError> {
        // Reject bases that escape the workspace ∪ read-only roots before
        // any filesystem traversal: `..` traversal, absolute paths outside
        // the roots, etc. This turns a misleading "no matches" (results
        // outside the sandbox used to be silently dropped by strip_prefix
        // below) into a clear error.
        let base = glob_base_prefix(pattern);
        if !base.is_empty() {
            self.resolve_read(&base)?;
        }

        // Absolute patterns target read-only roots directly; relative
        // patterns are joined onto the workspace root.
        let full_pattern = if Path::new(pattern).is_absolute() {
            PathBuf::from(pattern)
        } else {
            self.workspace_root.join(pattern)
        };
        let pattern_str = full_pattern.to_string_lossy();

        // Backstop: if the glob matched only files outside every allowed
        // root (odd cases the prefix check above misses, e.g. symlinked
        // bases), report an error instead of a misleading empty list.
        let mut dropped_outside = 0usize;
        let mut entries = glob::glob(&pattern_str)
            .map_err(FsError::from)?
            .filter_map(|entry| entry.ok())
            .filter(|p| p.is_file())
            .filter_map(|p| {
                // Canonicalize so path form (long/short names, `..`) cannot
                // defeat the root prefix checks.
                let canon = match p.canonicalize() {
                    Ok(c) => c,
                    Err(_) => return None, // vanished between glob and check
                };
                // Workspace files → workspace-relative paths.
                if let Ok(rel) = canon.strip_prefix(&self.workspace_root) {
                    return Some(rel.to_string_lossy().to_string());
                }
                // Read-only-root files → absolute paths.
                if self
                    .read_only_roots
                    .iter()
                    .any(|root| canon.strip_prefix(root).is_ok())
                {
                    return Some(canon.to_string_lossy().to_string());
                }
                dropped_outside += 1;
                None
            })
            .collect::<Vec<String>>();

        if entries.is_empty() && dropped_outside > 0 {
            return Err(FsError::WorkspaceEscape(format!(
                "'{pattern}' matched only files outside the workspace and read-only roots"
            )));
        }

        entries.sort();
        Ok(entries)
    }

    /// Search files with a regex pattern.
    pub fn grep(&self, pattern: &str, path_glob: Option<&str>) -> Result<Vec<GrepMatch>, FsError> {
        let re = regex::Regex::new(pattern).map_err(FsError::from)?;
        let glob_pattern = path_glob.unwrap_or("**/*");
        let files = self.glob(glob_pattern)?;

        let mut matches = Vec::new();
        for file_path in &files {
            // Absolute paths from read-only-root globs resolve through
            // read-mode (workspace ∪ read-only roots).
            let resolved = self.resolve_read(file_path)?;

            // Skip files with blocked extensions (binary formats like .exe, .dll, .bin).
            if self.is_extension_blocked(&resolved) {
                continue;
            }

            // Skip files too large to read (consistent with `read()` behavior).
            let metadata = resolved.metadata().map_err(FsError::Io)?;
            if metadata.len() > self.max_read_bytes as u64 {
                continue;
            }

            // Read as raw bytes and convert to UTF-8 losslessly. Binary files
            // (null bytes in first 8 KiB) are skipped �?text search is only
            // meaningful in text files.
            let bytes = fs::read(&resolved).map_err(FsError::Io)?;
            if Self::is_likely_binary(&bytes) {
                continue;
            }
            let content = String::from_utf8_lossy(&bytes);

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

    /// List directory contents. `None` or `""` = root.
    pub fn ls(&self, path: Option<&str>) -> Result<Vec<DirEntry>, FsError> {
        let resolved = self.resolve_read(path.unwrap_or(""))?;
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
            entries.push(DirEntry {
                name,
                entry_type,
                size: metadata.len(),
            });
        }

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

// ── Supporting types ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub entry_type: EntryType,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone)]
pub struct GrepMatch {
    pub file_path: String,
    pub line_number: usize,
    pub line_content: String,
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Extract the leading prefix of a glob pattern up to (but excluding) the
/// first component containing glob metacharacters (`*`, `?`, `[`, `{`).
/// Used to validate that the search base stays within the workspace.
///
/// Examples: `src/**/*.rs` → `src`, `../outside/**` → `..`,
/// `C:/Users/foo/*.rs` → `C:/Users/foo`, `**/*.rs` → ``.
fn glob_base_prefix(pattern: &str) -> String {
    let normalized = pattern.replace('\\', "/");
    let mut parts: Vec<&str> = normalized.split('/').collect();
    let first_glob = parts
        .iter()
        .position(|p| p.contains(['*', '?', '[', '{']))
        .unwrap_or(parts.len());
    parts.truncate(first_glob);
    parts.join("/")
}

/// 1-indexed inclusive line range of a replacement in the file's NEW state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditSpan {
    /// First line of the replacement (1-indexed).
    pub start_line: usize,
    /// Last line of the replacement (1-indexed; equals `start_line` when
    /// the replacement is empty or a single line).
    pub end_line: usize,
}

/// Byte ranges of every CRLF-tolerant occurrence of `needle` in `content`,
/// as `(start, end)` offsets into the ORIGINAL content.
///
/// `\r` is ignored on both sides — the `read` tool normalizes file content
/// to `\n`, so a caller echoing read output back must not fail on Windows
/// line endings. `\r` is never semantically significant in edited text.
fn find_crlf_tolerant(content: &str, needle: &str) -> Vec<(usize, usize)> {
    let (norm, map) = normalize_cr(content);
    let (needle_norm, _) = normalize_cr(needle);
    if needle_norm.is_empty() {
        return Vec::new();
    }
    norm.windows(needle_norm.len())
        .enumerate()
        .filter(|(_, win)| *win == needle_norm)
        // End is one past the LAST matched byte, not `map[idx + len]`:
        // a `\r` skipped between the needle's last byte and the next
        // byte belongs to the file's line ending and must survive the
        // replacement.
        .map(|(idx, _)| (map[idx], map[idx + needle_norm.len() - 1] + 1))
        .collect()
}

/// Copy `text` without `\r` as raw bytes, recording each kept byte's
/// original offset. `map[i]` is the original offset of normalized byte `i`;
/// `map[norm.len()]` is `text.len()` (sentinel for end-of-string).
fn normalize_cr(text: &str) -> (Vec<u8>, Vec<usize>) {
    let bytes = text.as_bytes();
    let mut norm = Vec::with_capacity(bytes.len());
    let mut map = Vec::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'\r' {
            map.push(i);
            norm.push(b);
        }
    }
    map.push(bytes.len());
    (norm, map)
}

fn detect_line_end(text: &str) -> String {
    if text.contains("\r\n") {
        "\r\n".to_string()
    } else {
        "\n".to_string()
    }
}

fn normalize_partial(path: &Path) -> Result<PathBuf, FsError> {
    let mut existing = path.to_path_buf();
    let mut tail_components: Vec<PathBuf> = Vec::new();

    loop {
        if existing.exists() {
            let canon = existing.canonicalize().map_err(FsError::Io)?;
            let mut result = canon;
            for comp in tail_components.iter().rev() {
                if comp == Path::new("..") {
                    if result.parent().is_some() {
                        result.pop();
                    }
                } else if comp != Path::new(".") {
                    result.push(comp);
                }
            }
            return Ok(result);
        }
        if let (Some(parent), Some(file_name)) = (existing.parent(), existing.file_name()) {
            tail_components.push(PathBuf::from(file_name));
            existing = parent.to_path_buf();
        } else {
            return Ok(path.to_path_buf());
        }
    }
}

// ── File-system error ──────────────────────────────────────────────────────────

/// File-system operation error returned by [`WorkspaceFs`].
#[derive(Debug)]
#[non_exhaustive]
pub enum FsError {
    WorkspaceEscape(String),
    FileTooLarge {
        path: String,
        size: u64,
        max: u64,
    },
    BinaryContentDetected(String),
    HiddenFileBlocked(String),
    ExtensionBlocked(String),
    NotFound(String),
    NotAFile(String),
    NotADirectory(String),
    /// `edit_content` found zero matches for `old_content` (file changed
    /// since the caller's last read, or the needle is stale).
    NoMatch {
        path: String,
        needle: String,
    },
    /// `edit_content` found multiple matches for `old_content`.
    AmbiguousMatch {
        path: String,
        count: usize,
    },
    Io(std::io::Error),
    GlobPatternError(String),
    RegexError(String),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkspaceEscape(path) => {
                write!(f, "path escapes workspace: {path}")
            }
            Self::FileTooLarge { path, size, max } => {
                write!(
                    f,
                    "file too large for operation: {path} ({size} bytes, max {max})"
                )
            }
            Self::BinaryContentDetected(path) => {
                write!(f, "binary content detected, write blocked: {path}")
            }
            Self::HiddenFileBlocked(path) => {
                write!(f, "hidden file write blocked: {path}")
            }
            Self::ExtensionBlocked(path) => {
                write!(f, "file extension blocked: {path}")
            }
            Self::NotFound(path) => write!(f, "not found: {path}"),
            Self::NotAFile(path) => write!(f, "not a file: {path}"),
            Self::NotADirectory(path) => write!(f, "not a directory: {path}"),
            Self::NoMatch { path, needle } => write!(
                f,
                "no match for old_content \"{needle}\" in {path} — the file may have \
                 changed since it was read; re-read the file and retry with fresh content"
            ),
            Self::AmbiguousMatch { path, count } => write!(
                f,
                "old_content matches {count} locations in {path} — include more \
                 surrounding context to make the match unique"
            ),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::GlobPatternError(msg) => write!(f, "glob error: {msg}"),
            Self::RegexError(msg) => write!(f, "regex error: {msg}"),
        }
    }
}

impl std::error::Error for FsError {}

impl From<std::io::Error> for FsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<glob::PatternError> for FsError {
    fn from(e: glob::PatternError) -> Self {
        Self::GlobPatternError(e.to_string())
    }
}

impl From<regex::Error> for FsError {
    fn from(e: regex::Error) -> Self {
        Self::RegexError(e.to_string())
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_config() -> FilesystemConfig {
        let mut cfg = FilesystemConfig::default();
        // Use generous limits for tests — we're testing sandbox logic,
        // not the specific limit values.
        cfg.max_read_bytes = 10_000_000;
        cfg.max_write_bytes = 1_000_000;
        cfg.forbid_binary_writes = true;
        cfg.forbid_hidden_file_writes = false; // allow .files in tests
        cfg.read_only_paths = vec![]; // hermetic: no auto-detected roots
        cfg
    }

    /// A workspace plus a separate temp dir configured as a read-only root.
    fn setup_fs_with_read_root() -> (tempfile::TempDir, tempfile::TempDir, WorkspaceFs) {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.read_only_paths = vec![outside.path().to_string_lossy().into_owned()];
        let fs = WorkspaceFs::new(dir.path(), &cfg).unwrap();
        (dir, outside, fs)
    }

    fn setup_fs() -> (tempfile::TempDir, WorkspaceFs) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path(), &test_config()).unwrap();
        (dir, fs)
    }

    #[test]
    fn test_new_valid_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(WorkspaceFs::new(dir.path(), &test_config()).is_ok());
    }

    #[test]
    fn test_new_nonexistent() {
        let cfg = test_config();
        let result = WorkspaceFs::new("/tmp/__nonexistent_dir__", &cfg);
        assert!(matches!(result, Err(FsError::NotFound(_))));
    }

    #[test]
    fn test_path_escapes_workspace() {
        let (_dir, fs) = setup_fs();
        let result = fs.read("../outside_file.txt", None, None);
        assert!(matches!(result, Err(FsError::WorkspaceEscape(_))));
    }

    #[test]
    fn test_read_simple() {
        let (_dir, fs) = setup_fs();
        fs.write("test.txt", "hello\nworld\n").unwrap();
        let result = fs.read("test.txt", None, None).unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
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
    fn test_write_new_file() {
        let (_dir, fs) = setup_fs();
        fs.write("new.txt", "hello").unwrap();
        let content = fs::read_to_string(_dir.path().join("new.txt")).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn test_write_creates_parent_dirs() {
        let (_dir, fs) = setup_fs();
        fs.write("a/b/c/file.txt", "nested").unwrap();
        assert!(_dir.path().join("a/b/c/file.txt").exists());
    }

    #[test]
    fn test_edit_single_line() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "line1\nline2\nline3\n").unwrap();
        fs.edit_lines("f.txt", 2, 2, "replaced").unwrap();
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "line1\nreplaced\nline3\n");
    }

    #[test]
    fn test_edit_content_replaces_unique_fragment() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "a\nb\nc\n").unwrap();
        let span = fs.edit_content("f.txt", "b", "X\nY").unwrap();
        assert_eq!(
            span,
            EditSpan {
                start_line: 2,
                end_line: 3
            }
        );
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "a\nX\nY\nc\n");
    }

    #[test]
    fn test_edit_content_deletes_fragment() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "a\nb\nc\n").unwrap();
        let span = fs.edit_content("f.txt", "b\n", "").unwrap();
        assert_eq!(
            span,
            EditSpan {
                start_line: 2,
                end_line: 2
            }
        );
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "a\nc\n");
    }

    #[test]
    fn test_edit_content_mid_line_span() {
        // Partial-line match: span covers the line the fragment lives on.
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "fn foo() {\n    let x = 1;\n}\n")
            .unwrap();
        let span = fs
            .edit_content("f.txt", "let x = 1;", "let x = 2;")
            .unwrap();
        assert_eq!(
            span,
            EditSpan {
                start_line: 2,
                end_line: 2
            }
        );
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "fn foo() {\n    let x = 2;\n}\n");
    }

    #[test]
    fn test_edit_content_no_match() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "a\nb\nc\n").unwrap();
        let err = fs.edit_content("f.txt", "zzz", "x").unwrap_err();
        match err {
            FsError::NoMatch { path, needle } => {
                assert_eq!(path, "f.txt");
                assert_eq!(needle, "zzz");
            }
            other => panic!("expected NoMatch, got {other}"),
        }
        // File untouched on failure.
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "a\nb\nc\n");
    }

    #[test]
    fn test_edit_content_ambiguous_match() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "a\nb\na\n").unwrap();
        let err = fs.edit_content("f.txt", "a", "x").unwrap_err();
        match err {
            FsError::AmbiguousMatch { path, count } => {
                assert_eq!(path, "f.txt");
                assert_eq!(count, 2);
            }
            other => panic!("expected AmbiguousMatch, got {other}"),
        }
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "a\nb\na\n");
    }

    #[test]
    fn test_edit_content_empty_old_rejected() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "a\n").unwrap();
        assert!(fs.edit_content("f.txt", "", "x").is_err());
    }

    #[test]
    fn test_edit_content_crlf_tolerant() {
        // read() normalizes \r\n to \n; echoing that back must still match.
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "line1\r\nline2\r\nline3\r\n").unwrap();
        let span = fs.edit_content("f.txt", "line2", "EDITED").unwrap();
        assert_eq!(
            span,
            EditSpan {
                start_line: 2,
                end_line: 2
            }
        );
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "line1\r\nEDITED\r\nline3\r\n");
        // Multi-line needle echoing normalized read output (LF) matches CRLF file.
        let span = fs.edit_content("f.txt", "line1\nEDITED", "A\nB").unwrap();
        assert_eq!(
            span,
            EditSpan {
                start_line: 1,
                end_line: 2
            }
        );
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "A\r\nB\r\nline3\r\n");
    }

    // ── Concurrency: parallel edits/writes must not silently lose updates ──

    #[test]
    fn test_concurrent_edits_all_survive() {
        let (_dir, fs) = setup_fs();
        let fs = Arc::new(fs);
        let mut content = String::new();
        for i in 1..=100 {
            content.push_str(&format!("line {i}\n"));
        }
        fs.write("f.txt", &content).unwrap();

        // 4 threads × 5 single-line edits (1→1 replacements keep line
        // numbers stable) at distinct lines 10..29. Without the per-file
        // lock, the read-modify-write races lose some edits silently while
        // both calls report success.
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let mut handles = Vec::new();
        for t in 0..4usize {
            let fs = Arc::clone(&fs);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for i in 0..5usize {
                    let line = 10 + t * 5 + i;
                    fs.edit_lines("f.txt", line, line, &format!("EDITED-{t}-{i}"))
                        .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let final_content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        for t in 0..4usize {
            for i in 0..5usize {
                assert!(
                    final_content.contains(&format!("EDITED-{t}-{i}")),
                    "edit {t}-{i} lost to a concurrent edit"
                );
            }
        }
        // Unedited lines survive too.
        assert!(final_content.contains("line 1"));
        assert!(final_content.contains("line 100"));
    }

    #[test]
    fn test_concurrent_write_and_edit_consistent() {
        let (_dir, fs) = setup_fs();
        let fs = Arc::new(fs);

        // A parallel `write` (all 'b') and `edit` (line 50 → "CCCC") on the
        // same file must never yield the stale-edit result (all 'a' with
        // line 50 "CCCC") — that means the edit read pre-write state and
        // silently clobbered the write. Looped to make the no-lock race
        // likely. Both contents are 100 lines, so line 50 stays valid
        // whichever operation lands first.
        for round in 0..20 {
            let original = (0..100).map(|_| "a").collect::<Vec<_>>().join("\n") + "\n";
            fs.write("f.txt", &original).unwrap();
            let new_content = (0..100).map(|_| "b").collect::<Vec<_>>().join("\n") + "\n";

            let fs1 = Arc::clone(&fs);
            let fs2 = Arc::clone(&fs);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let write_handle = {
                let b = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    b.wait();
                    fs1.write("f.txt", &new_content).unwrap();
                })
            };
            let edit_handle = {
                let b = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    b.wait();
                    fs2.edit_lines("f.txt", 50, 50, "CCCC").unwrap();
                })
            };
            write_handle.join().unwrap();
            edit_handle.join().unwrap();

            let final_content = std::fs::read_to_string(_dir.path().join("f.txt")).unwrap();
            let lines: Vec<&str> = final_content.lines().collect();
            assert_eq!(
                lines.len(),
                100,
                "round {round}: file corrupted: {final_content}"
            );
            for (i, line) in lines.iter().enumerate() {
                let is_b = *line == "b";
                let is_marker = i == 49 && *line == "CCCC";
                assert!(
                    is_b || is_marker,
                    "round {round}: unexpected line {}: {line:?} — write lost to stale edit",
                    i + 1,
                );
            }
        }
    }

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
    fn test_grep_basic() {
        let (_dir, fs) = setup_fs();
        fs.write("a.rs", "fn main() {}\n").unwrap();
        let results = fs.grep("fn", None).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_ls_root() {
        let (_dir, fs) = setup_fs();
        fs.write("a.txt", "").unwrap();
        fs::create_dir(_dir.path().join("sub")).unwrap();
        let entries = fs.ls(None).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "sub"); // directories first
        assert_eq!(entries[1].name, "a.txt");
    }

    // ── New sandbox enforcement tests ───────────────────────────────────

    #[test]
    fn test_read_file_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.max_read_bytes = 10; // tiny limit
        let fs = WorkspaceFs::new(dir.path(), &cfg).unwrap();
        fs.write("big.txt", "this is more than ten bytes of content")
            .unwrap();
        let result = fs.read("big.txt", None, None);
        assert!(
            matches!(result, Err(FsError::FileTooLarge { .. })),
            "expected FileTooLarge, got {result:?}"
        );
    }

    #[test]
    fn test_write_binary_blocked() {
        let (_dir, fs) = setup_fs();
        // Use .txt so the extension check doesn't intercept first.
        let result = fs.write("evil.txt", "MZ\u{0}binary");
        assert!(
            matches!(result, Err(FsError::BinaryContentDetected(_))),
            "expected BinaryContentDetected, got {result:?}"
        );
    }

    #[test]
    fn test_write_extension_blocked() {
        let (_dir, fs) = setup_fs();
        let result = fs.write("malware.exe", "harmless text");
        assert!(
            matches!(result, Err(FsError::ExtensionBlocked(_))),
            "expected ExtensionBlocked, got {result:?}"
        );
    }

    #[test]
    fn test_write_hidden_file_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.forbid_hidden_file_writes = true;
        let fs = WorkspaceFs::new(dir.path(), &cfg).unwrap();
        let result = fs.write(".env", "SECRET=123");
        assert!(
            matches!(result, Err(FsError::HiddenFileBlocked(_))),
            "expected HiddenFileBlocked, got {result:?}"
        );
    }

    #[test]
    fn test_glob_rejects_escape_pattern() {
        let (_dir, fs) = setup_fs();
        // `..` traversal must error, not silently return an empty list.
        let result = fs.glob("../outside/**/*.rs");
        assert!(
            matches!(result, Err(FsError::WorkspaceEscape(_))),
            "expected WorkspaceEscape, got {result:?}"
        );
    }

    #[test]
    fn test_glob_rejects_absolute_pattern() {
        let (_dir, fs) = setup_fs();
        // Absolute paths are outside the sandbox and must be rejected.
        let pattern = if cfg!(windows) {
            "C:/Windows/System32/*.dll"
        } else {
            "/etc/**/*.conf"
        };
        let result = fs.glob(pattern);
        assert!(
            matches!(result, Err(FsError::WorkspaceEscape(_))),
            "expected WorkspaceEscape, got {result:?}"
        );
    }

    #[test]
    fn test_grep_rejects_out_of_workspace_glob() {
        let (_dir, fs) = setup_fs();
        let result = fs.grep("fn", Some("../outside/**/*.rs"));
        assert!(
            matches!(result, Err(FsError::WorkspaceEscape(_))),
            "expected WorkspaceEscape, got {result:?}"
        );
    }

    #[test]
    fn test_glob_base_prefix() {
        assert_eq!(glob_base_prefix("*.rs"), "");
        assert_eq!(glob_base_prefix("src/**/*.rs"), "src");
        assert_eq!(glob_base_prefix("src/tui/*.rs"), "src/tui");
        assert_eq!(glob_base_prefix("a/b/c.txt"), "a/b/c.txt");
        assert_eq!(glob_base_prefix("**/*.rs"), "");
        assert_eq!(glob_base_prefix("../outside/**/*.rs"), "../outside");
    }

    // ── Read-only roots ────────────────────────────────────────────────

    #[test]
    fn test_read_only_root_allows_read() {
        let (_dir, outside, fs) = setup_fs_with_read_root();
        let f = outside.path().join("lib.rs");
        std::fs::write(&f, "pub fn f() {}\n").unwrap();
        let result = fs.read(&f.to_string_lossy(), None, None).unwrap();
        assert!(result.contains("pub fn f"), "got: {result}");
    }

    #[test]
    fn test_read_only_root_allows_ls() {
        let (_dir, outside, fs) = setup_fs_with_read_root();
        std::fs::write(outside.path().join("a.rs"), "").unwrap();
        let entries = fs.ls(Some(&outside.path().to_string_lossy())).unwrap();
        assert!(entries.iter().any(|e| e.name == "a.rs"), "got: {entries:?}");
    }

    #[test]
    fn test_read_only_root_allows_glob_and_grep() {
        let (_dir, outside, fs) = setup_fs_with_read_root();
        std::fs::create_dir_all(outside.path().join("src")).unwrap();
        std::fs::write(outside.path().join("src/lib.rs"), "fn hello() {}\n").unwrap();
        let pat = format!(
            "{}/src/*.rs",
            outside.path().to_string_lossy().replace('\\', "/")
        );

        // Glob returns absolute paths for read-only-root files.
        let files = fs.glob(&pat).unwrap();
        assert_eq!(files.len(), 1, "got: {files:?}");
        assert!(files[0].contains("lib.rs"), "got: {files:?}");

        // Grep resolves those absolute paths via read-mode.
        let matches = fs.grep("hello", Some(&pat)).unwrap();
        assert_eq!(matches.len(), 1, "got: {matches:?}");
    }

    #[test]
    fn test_read_only_root_rejects_write() {
        let (_dir, outside, fs) = setup_fs_with_read_root();
        let f = outside.path().join("x.txt");
        let result = fs.write(&f.to_string_lossy(), "content");
        assert!(
            matches!(result, Err(FsError::WorkspaceEscape(_))),
            "expected WorkspaceEscape, got {result:?}"
        );
    }

    #[test]
    fn test_read_only_root_rejects_edit() {
        let (_dir, outside, fs) = setup_fs_with_read_root();
        let f = outside.path().join("x.txt");
        std::fs::write(&f, "line1\n").unwrap();
        let result = fs.edit_lines(&f.to_string_lossy(), 1, 1, "changed");
        assert!(
            matches!(result, Err(FsError::WorkspaceEscape(_))),
            "expected WorkspaceEscape, got {result:?}"
        );
    }

    #[test]
    fn test_read_outside_all_roots_rejected() {
        let (_dir, _outside, fs) = setup_fs_with_read_root();
        // A third directory outside both workspace and read-only root.
        let third = tempfile::tempdir().unwrap();
        std::fs::write(third.path().join("f.txt"), "x").unwrap();
        let result = fs.read(&third.path().join("f.txt").to_string_lossy(), None, None);
        assert!(
            matches!(result, Err(FsError::WorkspaceEscape(_))),
            "expected WorkspaceEscape, got {result:?}"
        );
    }

    #[test]
    fn test_glob_rejects_absolute_outside_read_roots() {
        let (_dir, _outside, fs) = setup_fs_with_read_root();
        // Absolute pattern that is in no allowed root must error.
        let pattern = if cfg!(windows) {
            "C:/Windows/System32/*.dll"
        } else {
            "/etc/**/*.conf"
        };
        let result = fs.glob(pattern);
        assert!(
            matches!(result, Err(FsError::WorkspaceEscape(_))),
            "expected WorkspaceEscape, got {result:?}"
        );
    }

    #[test]
    fn test_write_content_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.max_write_bytes = 5;
        let fs = WorkspaceFs::new(dir.path(), &cfg).unwrap();
        let result = fs.write("small.txt", "this is way too long");
        assert!(
            matches!(result, Err(FsError::FileTooLarge { .. })),
            "expected FileTooLarge, got {result:?}"
        );
    }
}
