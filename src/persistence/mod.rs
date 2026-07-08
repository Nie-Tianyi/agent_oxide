//! # Conversation Persistence
//!
//! Saves and loads conversation threads to `.loomis/threads/` under the
//! workspace root. Each thread is stored as a JSON file (machine-readable,
//! used by `/resume`) and a Markdown file (human-readable, for debugging).
//!
//! A `.loomis/current` text file tracks the active thread name across
//! sessions so auto-save always targets the right thread.

use serde::{Deserialize, Serialize};

use crate::core::client::Message;
use crate::memory::Memory;

use std::path::Path;
use std::{fs, io};

// ── Constants ──────────────────────────────────────────────────────────────────

const THREADS_DIR: &str = ".loomis/threads";
const CURRENT_FILE: &str = ".loomis/current";
const DEFAULT_THREAD: &str = "autosave";
const CURRENT_VERSION: u32 = 1;

// ── ThreadInfo ─────────────────────────────────────────────────────────────────

/// Lightweight metadata for a saved conversation thread.
///
/// Returned by [`list_threads`] without deserializing full message bodies.
#[derive(Debug, Clone)]
pub struct ThreadInfo {
    /// Thread name (filename stem without `.json` extension).
    pub name: String,
    /// ISO 8601 UTC timestamp of last save.
    pub saved_at: String,
    /// Number of messages in the thread.
    pub message_count: usize,
    /// Total character count across all message content.
    pub total_chars: usize,
}

// ── ConversationFile (internal serialization format) ───────────────────────────

#[derive(Serialize, Deserialize)]
struct ConversationFile {
    version: u32,
    saved_at: String,
    compact_threshold: usize,
    keep_last_n: usize,
    messages: Vec<Message>,
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Saves the current conversation to a named thread.
///
/// Creates `.loomis/threads/` if it doesn't exist, then writes both a
/// pretty-printed JSON file (`{name}.json`) and a human-readable Markdown
/// file (`{name}.md`).
///
/// # Errors
///
/// Returns [`io::Error`] if:
/// - The directory cannot be created
/// - Serialization fails
/// - The files cannot be written
pub fn save_conversation(name: &str, workspace_root: &Path, memory: &Memory) -> io::Result<()> {
    let dir = workspace_root.join(THREADS_DIR);
    fs::create_dir_all(&dir)?;

    let cf = ConversationFile {
        version: CURRENT_VERSION,
        saved_at: iso_now(),
        compact_threshold: memory.compact_threshold(),
        keep_last_n: memory.keep_last_n(),
        messages: memory.to_context_vec(),
    };

    // Machine-readable JSON
    let json =
        serde_json::to_string_pretty(&cf).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(dir.join(format!("{name}.json")), &json)?;

    // Human-readable Markdown
    let md = format_conversation_md(&cf);
    fs::write(dir.join(format!("{name}.md")), &md)?;

    Ok(())
}

/// Loads a named thread, returning a new [`Memory`] ready to replace the
/// current conversation.
///
/// Preserves the saved `compact_threshold` and `keep_last_n` settings from
/// the thread so the restored conversation behaves identically.
///
/// # Errors
///
/// Returns [`io::Error`] if the file doesn't exist, cannot be read, or
/// contains invalid JSON.
pub fn load_conversation(name: &str, workspace_root: &Path) -> io::Result<Memory> {
    let path = workspace_root
        .join(THREADS_DIR)
        .join(format!("{name}.json"));
    let json = fs::read_to_string(&path)?;
    let cf: ConversationFile =
        serde_json::from_str(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    Ok(Memory::builder()
        .threshold(cf.compact_threshold)
        .keep_last(cf.keep_last_n)
        .with_messages(cf.messages)
        .build())
}

/// Lists all saved conversation threads, sorted by save time (newest first).
///
/// Each entry is a [`ThreadInfo`] — metadata only, no message bodies loaded.
///
/// # Errors
///
/// Returns [`io::Error`] if the threads directory can't be read.
pub fn list_threads(workspace_root: &Path) -> io::Result<Vec<ThreadInfo>> {
    let dir = workspace_root.join(THREADS_DIR);

    // Directory might not exist yet — that's fine, return empty.
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut threads: Vec<ThreadInfo> = Vec::new();

    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();

        // Only process .json files
        if path.extension().map_or(true, |ext| ext != "json") {
            continue;
        }

        // Extract thread name from filename stem
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };

        // Read just enough for metadata — parse the whole file (these are
        // typically < 1 MB for compacted conversations).
        let json = match fs::read_to_string(&path) {
            Ok(j) => j,
            Err(_) => continue, // skip unreadable files
        };

        let cf: ConversationFile = match serde_json::from_str(&json) {
            Ok(cf) => cf,
            Err(_) => continue, // skip invalid files
        };

        let total_chars: usize = cf.messages.iter().map(|m| m.content.len()).sum();

        threads.push(ThreadInfo {
            name,
            saved_at: cf.saved_at,
            message_count: cf.messages.len(),
            total_chars,
        });
    }

    // Newest first
    threads.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));

    Ok(threads)
}

/// Reads the active thread name from `.loomis/current`.
///
/// Returns `None` if the file doesn't exist or is empty.
pub fn read_current_thread(workspace_root: &Path) -> Option<String> {
    let path = workspace_root.join(CURRENT_FILE);
    let content = fs::read_to_string(&path).ok()?;
    let name = content.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// Writes the active thread name to `.loomis/current`.
pub fn write_current_thread(name: &str, workspace_root: &Path) -> io::Result<()> {
    let path = workspace_root.join(CURRENT_FILE);
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, name)
}

/// Returns the current thread name for auto-save, falling back to
/// [`DEFAULT_THREAD`] if no `.loomis/current` file exists.
///
/// This is the single entry point for auto-save callers — they don't need to
/// handle the missing-file case themselves.
pub fn default_thread_name(workspace_root: &Path) -> String {
    read_current_thread(workspace_root).unwrap_or_else(|| DEFAULT_THREAD.to_string())
}

/// Generates a filesystem-safe thread name from the user's first message.
///
/// Rules: take first ~60 chars, lowercase, replace non-alphanumeric chars
/// with hyphens, collapse consecutive hyphens, trim leading/trailing hyphens.
/// Falls back to a timestamp-based name if the result is empty.
pub fn generate_thread_name(first_message: &str) -> String {
    // Take first ~60 chars
    let end = first_message.floor_char_boundary(60.min(first_message.len()));
    let snippet = &first_message[..end];

    let mut slug = String::with_capacity(snippet.len());
    for ch in snippet.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if ch == '-' {
            slug.push('-');
        } else {
            // Replace any other character with hyphen
            slug.push('-');
        }
    }

    // Collapse consecutive hyphens
    let mut collapsed = String::with_capacity(slug.len());
    let mut last_was_hyphen = false;
    for ch in slug.chars() {
        if ch == '-' {
            if !last_was_hyphen {
                collapsed.push('-');
            }
            last_was_hyphen = true;
        } else {
            collapsed.push(ch);
            last_was_hyphen = false;
        }
    }

    // Trim leading/trailing hyphens
    let trimmed = collapsed.trim_matches('-');

    if trimmed.is_empty() {
        // Fallback: timestamp-based name
        format!("conversation-{}", iso_now().replace([':', 'T'], "-"))
    } else {
        trimmed.to_string()
    }
}

// ── Internal Helpers ───────────────────────────────────────────────────────────

/// Returns the current UTC time as an ISO 8601 string (e.g.
/// `"2026-07-09T14:30:00Z"`). Pure `std` — no `chrono` dependency.
fn iso_now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = d.as_secs();

    let days = total_secs / 86400;
    let time_secs = total_secs % 86400;

    let mut year = 1970i64;
    let mut remaining = days as i64;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }

    const MONTH_DAYS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1usize;
    for &md in &MONTH_DAYS {
        let dim = if month == 2 && is_leap_year(year) {
            29
        } else {
            md
        };
        if remaining < dim {
            break;
        }
        remaining -= dim;
        month += 1;
    }
    let day = remaining + 1;

    let h = time_secs / 3600;
    let m = (time_secs % 3600) / 60;
    let s = time_secs % 60;

    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

const fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Generates a human-readable Markdown version of a conversation thread.
fn format_conversation_md(cf: &ConversationFile) -> String {
    let mut md = String::new();

    // Header
    md.push_str("# Loomis Conversation\n\n");
    md.push_str(&format!("- **Saved**: {}\n", cf.saved_at));
    md.push_str(&format!("- **Version**: {}\n", cf.version));
    md.push_str(&format!("- **Messages**: {}\n", cf.messages.len()));

    let total_chars: usize = cf.messages.iter().map(|m| m.content.len()).sum();
    md.push_str(&format!("- **Total chars**: {total_chars}\n"));
    md.push_str(&format!(
        "- **Compact threshold**: {}\n",
        cf.compact_threshold
    ));
    md.push_str(&format!("- **Keep last N**: {}\n\n", cf.keep_last_n));

    md.push_str("---\n\n");

    // Messages
    for msg in &cf.messages {
        let role_str = match msg.role {
            crate::core::client::Role::System => "System",
            crate::core::client::Role::User => "User",
            crate::core::client::Role::Assistant => "Assistant",
            crate::core::client::Role::Tool => {
                if let Some(ref id) = msg.tool_call_id {
                    md.push_str(&format!("## [Tool → {id}]\n\n"));
                } else {
                    md.push_str("## [Tool]\n\n");
                }
                md.push_str(&msg.content);
                md.push_str("\n\n---\n\n");
                continue;
            }
        };

        md.push_str(&format!("## [{role_str}]\n\n"));

        // Include tool-call info for assistant messages
        if let Some(ref tool_calls) = msg.tool_calls {
            for tc in tool_calls {
                md.push_str(&format!(
                    "🔧 **{}** (id: `{}`)\n\n",
                    tc.function.name, tc.id
                ));
                md.push_str("```json\n");
                md.push_str(&tc.function.arguments);
                md.push_str("\n```\n\n");
            }
        }

        md.push_str(&msg.content);
        md.push_str("\n\n---\n\n");
    }

    md
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::client::Role;
    use tempfile::TempDir;

    #[test]
    fn test_round_trip_save_and_load() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Build a memory with some messages
        let mem = Memory::builder()
            .threshold(500_000)
            .keep_last(8)
            .with_messages(vec![
                Message::new(Role::System, "You are helpful."),
                Message::new(Role::User, "Hello"),
                Message::new(Role::Assistant, "Hi there!"),
            ])
            .build();

        // Save
        save_conversation("test-thread", root, &mem).unwrap();

        // Verify files exist
        assert!(root.join(".loomis/threads/test-thread.json").exists());
        assert!(root.join(".loomis/threads/test-thread.md").exists());

        // Load back
        let loaded = load_conversation("test-thread", root).unwrap();

        assert_eq!(loaded.compact_threshold(), 500_000);
        assert_eq!(loaded.keep_last_n(), 8);
        assert_eq!(loaded.message_count(), 3);

        let msgs = loaded.to_context_vec();
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[0].content, "You are helpful.");
        assert_eq!(msgs[1].role, Role::User);
        assert_eq!(msgs[1].content, "Hello");
        assert_eq!(msgs[2].role, Role::Assistant);
        assert_eq!(msgs[2].content, "Hi there!");
    }

    #[test]
    fn test_load_nonexistent_thread() {
        let tmp = TempDir::new().unwrap();
        let result = load_conversation("no-such-thread", tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_list_threads_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let threads = list_threads(tmp.path()).unwrap();
        assert!(threads.is_empty());
    }

    #[test]
    fn test_list_threads_sorted() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let mem = Memory::new();
        save_conversation("older", root, &mem).unwrap();
        // Brief sleep so timestamps differ
        std::thread::sleep(std::time::Duration::from_millis(1100));
        save_conversation("newer", root, &mem).unwrap();

        let threads = list_threads(root).unwrap();
        assert_eq!(threads.len(), 2);
        // Newest first
        assert_eq!(threads[0].name, "newer");
        assert_eq!(threads[1].name, "older");
    }

    #[test]
    fn test_current_thread_read_write() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Initially None
        assert!(read_current_thread(root).is_none());

        // Write and read back
        write_current_thread("my-session", root).unwrap();
        assert_eq!(read_current_thread(root).unwrap(), "my-session");

        // Overwrite
        write_current_thread("another", root).unwrap();
        assert_eq!(read_current_thread(root).unwrap(), "another");
    }

    #[test]
    fn test_default_thread_name_fallback() {
        let tmp = TempDir::new().unwrap();
        // No current file → fallback to "autosave"
        assert_eq!(default_thread_name(tmp.path()), "autosave");
    }

    #[test]
    fn test_empty_memory_saves_and_loads() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let mem = Memory::new();
        save_conversation("empty", root, &mem).unwrap();

        let loaded = load_conversation("empty", root).unwrap();
        assert_eq!(loaded.message_count(), 0);
    }

    #[test]
    fn test_markdown_contains_expected_sections() {
        let mem = Memory::builder()
            .with_messages(vec![
                Message::new(Role::System, "System prompt here."),
                Message::new(Role::User, "User question?"),
            ])
            .build();

        let cf = ConversationFile {
            version: 1,
            saved_at: "2026-01-01T00:00:00Z".into(),
            compact_threshold: mem.compact_threshold(),
            keep_last_n: mem.keep_last_n(),
            messages: mem.to_context_vec(),
        };

        let md = format_conversation_md(&cf);
        assert!(md.contains("# Loomis Conversation"));
        assert!(md.contains("## [System]"));
        assert!(md.contains("System prompt here."));
        assert!(md.contains("## [User]"));
        assert!(md.contains("User question?"));
        assert!(md.contains("- **Messages**: 2"));
    }

    #[test]
    fn test_generate_thread_name_english() {
        let name = generate_thread_name("Help me research quantum computing");
        assert_eq!(name, "help-me-research-quantum-computing");
    }

    #[test]
    fn test_generate_thread_name_collapses_hyphens() {
        let name = generate_thread_name("Hello!!! World???");
        assert_eq!(name, "hello-world");
    }

    #[test]
    fn test_generate_thread_name_truncates_long() {
        let long = "A".repeat(100) + " B";
        let name = generate_thread_name(&long);
        assert!(name.len() <= 65); // 60 chars + some hyphens
        assert!(name.starts_with("a"));
    }

    #[test]
    fn test_generate_thread_name_chinese_fallback() {
        // Pure Chinese has no ASCII alphanumeric → falls back to timestamp
        let name = generate_thread_name("你好世界");
        assert!(name.starts_with("conversation-"));
    }

    #[test]
    fn test_generate_thread_name_mixed() {
        let name = generate_thread_name("帮我 debug Rust 代码");
        // 'debug' and 'Rust' survive, Chinese chars become hyphens
        assert!(name.contains("debug"));
        assert!(name.contains("rust"));
    }
}
