//! Hook that seeds the initial system messages into memory on first run.
//!
//! Pushes three `Role::System` messages:
//! 1. Main system prompt (from `prompts/system.md` with dynamic tool list)
//! 2. Environment context (platform, OS, shell, cwd, date, git)
//! 3. Project rules (LOOMIS.md → AGENTS.md → CLAUDE.md)
//!
//! Seeding happens exactly once — after `/new` (ClearConversation), system
//! messages are preserved, so the hook detects them and skips.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use engine::AgentHook;
use memory::SharedMemory;
use provider::{Message, Role};
use skills::SkillRegistry;

/// Maximum bytes to load from a project-rules file before truncating.
const PROJECT_RULES_MAX_BYTES: usize = 10_000;

/// Project-level rules file candidates, in priority order.
const PROJECT_RULES_FILES: &[&str] = &["LOOMIS.md", "AGENTS.md", "CLAUDE.md"];

// ── SystemPromptHook ─────────────────────────────────────────────────────────────

/// Seeds the three initial system messages on `on_run_start`.
///
/// Uses an [`AtomicBool`] flag for idempotency — after the first seed,
/// subsequent runs (including after `/new`) are no-ops because
/// `ClearConversation` preserves `Role::System` messages.
pub struct SystemPromptHook {
    workspace_root: PathBuf,
    tool_names: Vec<String>,
    skill_registry: Arc<SkillRegistry>,
    seeded: AtomicBool,
}

impl SystemPromptHook {
    pub fn new(
        workspace_root: PathBuf,
        tool_names: Vec<String>,
        skill_registry: Arc<SkillRegistry>,
    ) -> Self {
        Self {
            workspace_root,
            tool_names,
            skill_registry,
            seeded: AtomicBool::new(false),
        }
    }
}

impl AgentHook for SystemPromptHook {
    fn on_run_start(&self, _session_id: &str, _user_input: &str, memory: &SharedMemory) {
        // Seed only once. After /new, system messages survive the clear.
        if self.seeded.swap(true, Ordering::SeqCst) {
            return;
        }

        let mut mem = memory.write().expect("memory lock poisoned");

        // 1. Main system prompt (dynamic tool list + skill list)
        mem.push(Message::new(
            Role::System,
            build_system_prompt(&self.tool_names, &self.skill_registry),
        ));

        // 2. Environment context (platform, shell, cwd, date, git)
        mem.push(Message::new(
            Role::System,
            build_environment_context(&self.workspace_root),
        ));

        // 3. Project rules (LOOMIS.md → AGENTS.md → CLAUDE.md)
        if let Some(rules) = try_load_project_rules(&self.workspace_root) {
            mem.push(Message::new(Role::System, rules));
        }
    }
}

// ── System Prompt ─────────────────────────────────────────────────────────────────

/// Build the main system prompt with tool list and skill list injected dynamically.
///
/// Loaded from `prompts/system.md` at compile time via `include_str!()`.
/// `{tool_list}` and `{skill_list}` are dynamic — simple `str::replace` handles them.
fn build_system_prompt(tool_names: &[String], skill_registry: &SkillRegistry) -> String {
    let tool_list = tool_names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");

    let skill_list = if skill_registry.is_empty() {
        "  (none available. Define skills as .md files in .loomis/skills/)".to_string()
    } else {
        skill_registry
            .list()
            .iter()
            .map(|s| format!("- `{}` — {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n")
    };

    include_str!("../../prompts/system.md")
        .replace("{tool_list}", &tool_list)
        .replace("{skill_list}", &skill_list)
}

// ── Environment Context ─────────────────────────────────────────────────────────

/// Build a System message with runtime environment information.
fn build_environment_context(workspace_root: &Path) -> String {
    let platform = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);
    let os_ver = detect_os_version();
    let shell = detect_shell();
    let cwd = workspace_root.display().to_string();
    let date = memory::iso8601_now();
    let git_info = detect_git_info(workspace_root);

    let mut block = format!(
        "\
## Environment

- Platform: {platform}
- OS version: {os_ver}
- Shell: {shell}
- Workspace: {cwd}
- Date: {date}"
    );

    if let Some(git) = git_info {
        block.push_str(&format!("\n- Git: {git}"));
    }

    block
}

/// Best-effort OS version string.
fn detect_os_version() -> String {
    if cfg!(windows) {
        for (cmd, args) in [
            (
                "powershell",
                &[
                    "-NoProfile",
                    "-Command",
                    "[System.Environment]::OSVersion.VersionString",
                ] as &[_],
            ),
            ("cmd", &["/C", "ver"] as &[_]),
        ] {
            if let Ok(out) = std::process::Command::new(cmd).args(args).output()
                && out.status.success()
            {
                let s = String::from_utf8_lossy(&out.stdout);
                let s = s.trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
        std::env::consts::OS.to_string()
    } else {
        match std::process::Command::new("uname").args(["-srm"]).output() {
            Ok(out) => {
                let s = String::from_utf8_lossy(&out.stdout);
                s.trim().to_string()
            }
            Err(_) => format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        }
    }
}

/// Detect which shell the user is running under.
fn detect_shell() -> String {
    if std::env::var("MSYSTEM").is_ok() || std::env::var("MINGW_PREFIX").is_ok() {
        return "Git Bash (MSYS2 / MinGW)".to_string();
    }
    #[cfg(windows)]
    {
        if std::env::var("PSModulePath").is_ok() {
            return "PowerShell".to_string();
        }
        if let Ok(comspec) = std::env::var("ComSpec") {
            return comspec;
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(shell) = std::env::var("SHELL") {
            return shell;
        }
    }
    "unknown".to_string()
}

/// Best-effort git branch and dirty-status string.
///
/// Returns `None` when git is not installed or we're not inside a repo.
fn detect_git_info(workspace_root: &Path) -> Option<String> {
    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                let s = s.trim().to_string();
                if !s.is_empty() { Some(s) } else { None }
            } else {
                None
            }
        })?;

    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace_root)
        .output()
        .ok()
        .map(|out| !String::from_utf8_lossy(&out.stdout).trim().is_empty())
        .unwrap_or(false);

    let status = if dirty { "dirty" } else { "clean" };
    Some(format!("branch `{branch}`, {status}"))
}

// ── Project Rules ───────────────────────────────────────────────────────────────

/// Try to load project-level rules from the workspace root.
///
/// Resolution priority: `LOOMIS.md` → `AGENTS.md` → `CLAUDE.md`.
/// Only the **first found** file is returned.  If no file exists or all
/// reads fail, returns `None`.
fn try_load_project_rules(workspace_root: &Path) -> Option<String> {
    for filename in PROJECT_RULES_FILES {
        let path = workspace_root.join(filename);
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                let truncated = if content.len() > PROJECT_RULES_MAX_BYTES {
                    let boundary = content
                        .char_indices()
                        .take(PROJECT_RULES_MAX_BYTES)
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(PROJECT_RULES_MAX_BYTES);
                    format!(
                        "{}…\n\n[Truncated from {} bytes — original file is {} bytes]",
                        &content[..boundary],
                        PROJECT_RULES_MAX_BYTES,
                        content.len()
                    )
                } else {
                    content
                };
                return Some(format!("## Project Rules ({filename})\n\n{truncated}"));
            }
            Ok(_) => {
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Cannot read project rules file");
                continue;
            }
        }
    }
    None
}
