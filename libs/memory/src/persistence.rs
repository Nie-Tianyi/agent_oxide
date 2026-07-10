//! # Conversation Persistence
//!
//! Saves and loads conversation threads to `.loomis/threads/` under the
//! workspace root.

use serde::{Deserialize, Serialize};

use provider::Message;
use provider::Role;

use crate::memory::Memory;

use std::path::Path;
use std::{fs, io};

// ── Constants ──────────────────────────────────────────────────────────────────

const THREADS_DIR: &str = ".loomis/threads";
const CURRENT_FILE: &str = ".loomis/current";
const DEFAULT_THREAD: &str = "autosave";
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

pub fn save_conversation(name: &str, workspace_root: &Path, memory: &Memory) -> io::Result<()> {
    let dir = workspace_root.join(THREADS_DIR);
    fs::create_dir_all(&dir)?;

    let cf = ConversationFile {
        version: CURRENT_VERSION,
        saved_at: iso_now(),
        messages: memory.to_context_vec(),
    };

    let json =
        serde_json::to_string_pretty(&cf).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(dir.join(format!("{name}.json")), &json)?;

    let md = format_conversation_md(&cf);
    fs::write(dir.join(format!("{name}.md")), &md)?;

    Ok(())
}

pub fn load_conversation(name: &str, workspace_root: &Path) -> io::Result<Memory> {
    let path = workspace_root
        .join(THREADS_DIR)
        .join(format!("{name}.json"));
    let json = fs::read_to_string(&path)?;
    let cf: ConversationFile =
        serde_json::from_str(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    Ok(Memory::from(cf.messages))
}

pub fn list_threads(workspace_root: &Path) -> io::Result<Vec<ThreadInfo>> {
    let dir = workspace_root.join(THREADS_DIR);

    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut threads: Vec<ThreadInfo> = Vec::new();

    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map_or(true, |ext| ext != "json") {
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

pub fn read_current_thread(workspace_root: &Path) -> Option<String> {
    let path = workspace_root.join(CURRENT_FILE);
    let content = fs::read_to_string(&path).ok()?;
    let name = content.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

pub fn write_current_thread(name: &str, workspace_root: &Path) -> io::Result<()> {
    let path = workspace_root.join(CURRENT_FILE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, name)
}

pub fn default_thread_name(workspace_root: &Path) -> String {
    read_current_thread(workspace_root).unwrap_or_else(|| DEFAULT_THREAD.to_string())
}

pub fn generate_thread_name(first_message: &str) -> String {
    let end = first_message.floor_char_boundary(60.min(first_message.len()));
    let snippet = &first_message[..end];

    let mut slug = String::with_capacity(snippet.len());
    for ch in snippet.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if ch == '-' {
            slug.push('-');
        } else {
            slug.push('-');
        }
    }

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

    let trimmed = collapsed.trim_matches('-');
    if trimmed.is_empty() {
        format!("conversation-{}", iso_now().replace([':', 'T'], "-"))
    } else {
        trimmed.to_string()
    }
}

// ── Internal Helpers ───────────────────────────────────────────────────────────

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

fn format_conversation_md(cf: &ConversationFile) -> String {
    let mut md = String::new();
    md.push_str("# Loomis Conversation\n\n");
    md.push_str(&format!("- **Saved**: {}\n", cf.saved_at));
    md.push_str(&format!("- **Version**: {}\n", cf.version));
    md.push_str(&format!("- **Messages**: {}\n", cf.messages.len()));
    let total_chars: usize = cf.messages.iter().map(|m| m.content.len()).sum();
    md.push_str(&format!("- **Total chars**: {total_chars}\n"));
    md.push_str("\n---\n\n");

    for msg in &cf.messages {
        let role_str = match msg.role {
            Role::System => "System",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => {
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

    #[test]
    fn test_round_trip_save_and_load() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mem = Memory::from(vec![
            Message::new(Role::System, "You are helpful."),
            Message::new(Role::User, "Hello"),
            Message::new(Role::Assistant, "Hi there!"),
        ]);

        save_conversation("test-thread", root, &mem).unwrap();
        assert!(root.join(".loomis/threads/test-thread.json").exists());

        let loaded = load_conversation("test-thread", root).unwrap();
        assert_eq!(loaded.message_count(), 3);
        let msgs = loaded.to_context_vec();
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[0].content, "You are helpful.");
    }

    #[test]
    fn test_load_nonexistent_thread() {
        let tmp = TempDir::new().unwrap();
        assert!(load_conversation("no-such-thread", tmp.path()).is_err());
    }

    #[test]
    fn test_current_thread_read_write() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        assert!(read_current_thread(root).is_none());
        write_current_thread("my-session", root).unwrap();
        assert_eq!(read_current_thread(root).unwrap(), "my-session");
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
    fn test_generate_thread_name_chinese_fallback() {
        let name = generate_thread_name("你好世界");
        assert!(name.starts_with("conversation-"));
    }
}
