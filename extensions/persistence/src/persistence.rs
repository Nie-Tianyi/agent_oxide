//! # Conversation Persistence
//!
//! Saves and loads conversation threads under the workspace root using
//! [`PersistenceConfig`] to determine paths.

use serde::{Deserialize, Serialize};

use provider::Message;
use provider::Role;

use memory::Memory;

use std::path::Path;
use std::{fs, io};

// ── PersistenceConfig ──────────────────────────────────────────────────────────

/// Configuration for conversation persistence — storage layout and naming.
///
/// The [`Default`] impl provides generic values; applications should override
/// with their own paths (e.g. `.your-app/threads` for the downstream binary).
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// Subdirectory under `workspace_root` where thread files are stored.
    pub threads_dir: String,
    /// Path (relative to `workspace_root`) for the current-thread marker file.
    pub current_thread_file: String,
    /// Fallback thread name when no current thread is recorded.
    pub default_thread_name: String,
    /// Title used in the markdown export header.
    pub markdown_title: String,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            threads_dir: ".agent/threads".into(),
            current_thread_file: ".agent/current".into(),
            default_thread_name: "autosave".into(),
            markdown_title: "Agent Conversation".into(),
        }
    }
}

// ── Framework-level constants ──────────────────────────────────────────────────

const CURRENT_VERSION: u32 = 1;

// ── ThreadInfo ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub name: String,
    pub saved_at: String,
    pub message_count: usize,
    pub total_chars: usize,
}

// ── ConversationFile (internal) ────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct ConversationFile {
    version: u32,
    saved_at: String,
    messages: Vec<Message>,
}

// ── Public API ─────────────────────────────────────────────────────────────────

pub fn save_conversation(
    name: &str,
    workspace_root: &Path,
    memory: &Memory,
    config: &PersistenceConfig,
) -> io::Result<()> {
    let name = sanitize_filename(name);
    let dir = workspace_root.join(&config.threads_dir);
    fs::create_dir_all(&dir)?;

    let cf = ConversationFile {
        version: CURRENT_VERSION,
        saved_at: util::iso8601_now(),
        messages: memory.to_context_vec(),
    };

    let json = serde_json::to_string_pretty(&cf).map_err(io::Error::other)?;
    fs::write(dir.join(format!("{name}.json")), &json)?;

    let md = format_conversation_md(&cf, config);
    fs::write(dir.join(format!("{name}.md")), &md)?;

    tracing::debug!(
        name = %name,
        message_count = cf.messages.len(),
        bytes = json.len() + md.len(),
        "conversation saved",
    );
    Ok(())
}

pub fn load_conversation(
    name: &str,
    workspace_root: &Path,
    config: &PersistenceConfig,
) -> io::Result<Memory> {
    let name = sanitize_filename(name);
    let path = workspace_root
        .join(&config.threads_dir)
        .join(format!("{name}.json"));
    let json = fs::read_to_string(&path)?;
    let cf: ConversationFile =
        serde_json::from_str(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    tracing::debug!(
        name = %name,
        message_count = cf.messages.len(),
        "conversation loaded",
    );
    Ok(Memory::from(cf.messages))
}

pub fn list_threads(
    workspace_root: &Path,
    config: &PersistenceConfig,
) -> io::Result<Vec<ThreadInfo>> {
    let dir = workspace_root.join(&config.threads_dir);

    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut threads: Vec<ThreadInfo> = Vec::new();

    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }

        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };

        let json = match fs::read_to_string(&path) {
            Ok(j) => j,
            Err(_) => continue,
        };

        let cf: ConversationFile = match serde_json::from_str(&json) {
            Ok(cf) => cf,
            Err(_) => continue,
        };

        let total_chars: usize = cf.messages.iter().map(|m| m.content.len()).sum();

        threads.push(ThreadInfo {
            name,
            saved_at: cf.saved_at,
            message_count: cf.messages.len(),
            total_chars,
        });
    }

    threads.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
    Ok(threads)
}

pub fn read_current_thread_name(
    workspace_root: &Path,
    config: &PersistenceConfig,
) -> Option<String> {
    let path = workspace_root.join(&config.current_thread_file);
    let content = fs::read_to_string(&path).ok()?;
    let name = content.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

pub fn write_current_thread_name(
    name: &str,
    workspace_root: &Path,
    config: &PersistenceConfig,
) -> io::Result<()> {
    let path = workspace_root.join(&config.current_thread_file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, name)
}

pub fn default_thread_name(workspace_root: &Path, config: &PersistenceConfig) -> String {
    read_current_thread_name(workspace_root, config)
        .unwrap_or_else(|| config.default_thread_name.clone())
}

/// Maximum length of a thread name (in bytes). Must leave headroom for the
/// directory prefix, `.json` extension, and Windows MAX_PATH (260) limits.
const MAX_THREAD_NAME_CHARS: usize = 120;

/// Windows reserved DOS device names. If a sanitized filename matches one of
/// these (case-insensitive), an underscore is appended to avoid filesystem issues.
const RESERVED_DOS_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Transform any string into a filesystem-safe filename.
///
/// Preserves Unicode characters (CJK, accented Latin, etc.) and only replaces
/// characters that are illegal in filenames on Windows / macOS / Linux:
///
/// - Control characters (0x00–0x1F) are **stripped**.
/// - `/`, `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|` are **replaced** with `_`.
/// - Everything else (letters, digits, CJK, spaces, most punctuation) passes through.
///
/// Additionally:
/// - The result is truncated to [`MAX_THREAD_NAME_CHARS`] at a `char` boundary.
/// - Consecutive `_` and spaces are collapsed into a single `_`.
/// - Leading / trailing `.`, ` `, and `_` are trimmed.
/// - Windows reserved DOS names (`CON`, `PRN`, …) get a trailing `_` appended.
/// - If the result is empty, a timestamp-based fallback is returned.
///
/// This function is idempotent: applying it to its own output is a no-op.
pub fn sanitize_filename(name: &str) -> String {
    // 1. Truncate to MAX_THREAD_NAME_CHARS at a char boundary.
    let end = name.floor_char_boundary(MAX_THREAD_NAME_CHARS.min(name.len()));
    let snippet = &name[..end];

    // 2. Map characters: keep Unicode, replace illegal chars, strip control chars.
    let mut mapped = String::with_capacity(snippet.len());
    for ch in snippet.chars() {
        if ch.is_control() {
            // strip silently
            continue;
        } else if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            mapped.push('_');
        } else {
            mapped.push(ch);
        }
    }

    // 3. Collapse consecutive '_' (from illegal-char replacement) into a single '_'.
    //    Spaces are legal on all major filesystems and are preserved as-is.
    let mut collapsed = String::with_capacity(mapped.len());
    let mut last_was_underscore = false;
    for ch in mapped.chars() {
        if ch == '_' {
            if !last_was_underscore {
                collapsed.push('_');
            }
            last_was_underscore = true;
        } else {
            collapsed.push(ch);
            last_was_underscore = false;
        }
    }

    // 4. Trim leading / trailing dots, spaces, underscores.
    let trimmed = collapsed.trim_matches(&['.', ' ', '_'][..]);

    // 5. Guard against reserved DOS names (case-insensitive).
    let lower = trimmed.to_ascii_lowercase();
    let guarded = if RESERVED_DOS_NAMES.contains(&lower.as_str()) {
        format!("{trimmed}_")
    } else {
        trimmed.to_string()
    };

    // 6. Fallback if empty after all processing.
    if guarded.is_empty() {
        format!(
            "conversation-{}",
            util::iso8601_now().replace([':', 'T'], "-")
        )
    } else {
        guarded
    }
}

/// Generate a filesystem-safe thread name from the user's first message.
///
/// Appends a `_YYYY-MM-DD` date suffix so that identical queries on different
/// days produce distinct filenames and don't overwrite each other.
///
/// If [`sanitize_filename`] already produced a timestamp-based fallback
/// (i.e. the message contained no usable characters), the fallback is
/// returned as-is — it already embeds a full timestamp.
pub fn thread_name_from_message(first_message: &str) -> String {
    let base = sanitize_filename(first_message);
    // Fallback names already carry a full UTC timestamp; don't double-suffix.
    if base.starts_with("conversation-") {
        return base;
    }
    // `iso8601_now()` → "YYYY-MM-DDTHH:MM:SSZ"; take just the date portion.
    let date = &util::iso8601_now()[..10];
    // "_" + "YYYY-MM-DD" = 11 chars.  Keep the total under MAX_THREAD_NAME_CHARS.
    let max_base = MAX_THREAD_NAME_CHARS.saturating_sub(11);
    let end = base.floor_char_boundary(max_base.min(base.len()));
    let base = &base[..end];
    format!("{base}_{date}")
}

// ── Internal Helpers ───────────────────────────────────────────────────────────

fn format_conversation_md(cf: &ConversationFile, config: &PersistenceConfig) -> String {
    let mut md = String::new();
    md.push_str(&format!("# {}\n\n", config.markdown_title));
    md.push_str(&format!("- **Saved**: {}\n", cf.saved_at));
    md.push_str(&format!("- **Version**: {}\n", cf.version));
    md.push_str(&format!("- **Messages**: {}\n", cf.messages.len()));
    let total_chars: usize = cf.messages.iter().map(|m| m.content.len()).sum();
    md.push_str(&format!("- **Total chars**: {total_chars}\n"));
    md.push_str("\n---\n\n");

    for msg in &cf.messages {
        // Tool messages get special formatting and skip the standard header.
        if msg.role == Role::Tool {
            if let Some(ref id) = msg.tool_call_id {
                md.push_str(&format!("## [Tool → {id}]\n\n"));
            } else {
                md.push_str("## [Tool]\n\n");
            }
            md.push_str(&msg.content);
            md.push_str("\n\n---\n\n");
            continue;
        }

        let role_str = msg.role.label();
        md.push_str(&format!("## [{role_str}]\n\n"));
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
    use tempfile::TempDir;

    fn test_config() -> PersistenceConfig {
        PersistenceConfig {
            threads_dir: ".agent/threads".into(),
            current_thread_file: ".agent/current".into(),
            default_thread_name: "autosave".into(),
            markdown_title: "Agent Conversation".into(),
        }
    }

    #[test]
    fn test_round_trip_save_and_load() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let config = test_config();
        let mem = Memory::from(vec![
            Message::new(Role::System, "You are helpful."),
            Message::new(Role::User, "Hello"),
            Message::new(Role::Assistant, "Hi there!"),
        ]);

        save_conversation("test-thread", root, &mem, &config).unwrap();
        assert!(
            root.join(&config.threads_dir)
                .join("test-thread.json")
                .exists()
        );

        let loaded = load_conversation("test-thread", root, &config).unwrap();
        assert_eq!(loaded.len(), 3);
        let msgs = loaded.to_context_vec();
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[0].content, "You are helpful.");
    }

    #[test]
    fn test_load_nonexistent_thread() {
        let tmp = TempDir::new().unwrap();
        let config = test_config();
        assert!(load_conversation("no-such-thread", tmp.path(), &config).is_err());
    }

    #[test]
    fn test_current_thread_read_write() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let config = test_config();
        assert!(read_current_thread_name(root, &config).is_none());
        write_current_thread_name("my-session", root, &config).unwrap();
        assert_eq!(
            read_current_thread_name(root, &config).unwrap(),
            "my-session"
        );
    }

    // ── sanitize_filename / thread_name_from_message ───────────────────────

    /// Current date in YYYY-MM-DD for use in `thread_name_from_message` assertions.
    fn today_date() -> String {
        util::iso8601_now()[..10].to_string()
    }

    #[test]
    fn test_thread_name_english_preserved() {
        let name = thread_name_from_message("Help me research quantum computing");
        assert_eq!(
            name,
            format!("Help me research quantum computing_{}", today_date())
        );
    }

    #[test]
    fn test_thread_name_chinese_preserved() {
        let name = thread_name_from_message("你好世界");
        assert_eq!(name, format!("你好世界_{}", today_date()));
    }

    #[test]
    fn test_thread_name_mixed_cjk_ascii() {
        let name = thread_name_from_message("帮我写一个Python脚本");
        assert_eq!(name, format!("帮我写一个Python脚本_{}", today_date()));
    }

    #[test]
    fn test_thread_name_illegal_chars_replaced() {
        let name = thread_name_from_message("Hello? foo:bar*<baz>");
        assert_eq!(name, format!("Hello_ foo_bar_baz_{}", today_date()));
    }

    #[test]
    fn test_thread_name_all_illegal_fallback() {
        let name = thread_name_from_message("***");
        assert!(name.starts_with("conversation-"), "got: {name}");
    }

    #[test]
    fn test_thread_name_control_chars_stripped() {
        let name = thread_name_from_message("hello\x00\x01world");
        assert_eq!(name, format!("helloworld_{}", today_date()));
    }

    #[test]
    fn test_thread_name_max_length_truncation() {
        let long = "a".repeat(200);
        let name = thread_name_from_message(&long);
        // Base (120) gets date suffix → truncation ensures total ≤ 120 chars.
        assert!(name.len() <= 120, "too long: {} chars", name.len());
        let prefix = "a".repeat(109); // 120 - 11 (date overhead)
        assert!(
            name.starts_with(&prefix),
            "expected prefix of {prefix}..., got: {name}"
        );
    }

    #[test]
    fn test_thread_name_leading_trailing_dot_trimmed() {
        let name = thread_name_from_message(".hidden.");
        assert_eq!(name, format!("hidden_{}", today_date()));
    }

    #[test]
    fn test_thread_name_trailing_dot_stripped() {
        let name = thread_name_from_message("name.");
        assert_eq!(name, format!("name_{}", today_date()));
    }

    #[test]
    fn test_thread_name_reserved_dos_name() {
        let name = thread_name_from_message("con");
        assert_eq!(name, format!("con__{}", today_date()));
    }

    #[test]
    fn test_thread_name_reserved_dos_name_case_insensitive() {
        let name = thread_name_from_message("CON");
        assert_eq!(name, format!("CON__{}", today_date()));
    }

    #[test]
    fn test_thread_name_empty_fallback() {
        let name = thread_name_from_message("");
        assert!(name.starts_with("conversation-"), "got: {name}");
    }

    #[test]
    fn test_sanitize_filename_idempotent() {
        let inputs = [
            "Hello world",
            "你好世界",
            "foo/bar:baz*qux?",
            ".hidden.",
            "con",
            "normal-name",
        ];
        for input in &inputs {
            let once = sanitize_filename(input);
            let twice = sanitize_filename(&once);
            assert_eq!(once, twice, "not idempotent for input: {input}");
        }
    }

    #[test]
    fn test_sanitize_filename_spaces_preserved() {
        let name = sanitize_filename("hello   world");
        assert_eq!(name, "hello   world");
    }

    #[test]
    fn test_sanitize_filename_underscores_collapsed() {
        let name = sanitize_filename("a__b");
        assert_eq!(name, "a_b");
    }
}
