//! [`ShellFilter`] — classifies shell commands as safe, suspicious, or blocked.
//!
//! The classification is driven by [`SandboxConfig`]:
//!
//! 1. **Strict allowlist** — if `allowed_commands.binaries` is non-empty, only
//!    those exact binary names pass; everything else is rejected.
//! 2. **Deny patterns** — regexes matched against the full command string;
//!    a hit means immediate rejection (no user prompt).
//! 3. **Auto-approve prefixes** — commands whose first word matches a prefix
//!    are allowed without user confirmation.
//! 4. **Fallthrough** — anything that passes filters 1-3 requires a user prompt.

use crate::config::SandboxConfig;
use regex::Regex;

/// The outcome of filtering a shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandVerdict {
    /// Safe — can execute without user confirmation.
    AutoApproved,
    /// Needs user Y/n confirmation before execution.
    RequiresApproval,
    /// Dangerous — rejected outright, no user prompt.
    Blocked { reason: String },
}

/// Compiled shell-command policy from [`SandboxConfig`].
///
/// `Clone` so the same policy can be shared between the [`SandboxHook`]
/// (LLM-initiated calls) and the TUI (user `!command` invocations).
#[derive(Clone)]
pub struct ShellFilter {
    auto_approve_prefixes: Vec<String>,
    deny_patterns: Vec<Regex>,
    allow_binaries: Option<Vec<String>>,
}

impl ShellFilter {
    /// Compile the policy from a sandbox configuration.
    pub fn from_config(config: &SandboxConfig) -> Self {
        let deny_patterns: Vec<Regex> = config
            .shell
            .deny_patterns
            .patterns
            .iter()
            .filter_map(|p| {
                Regex::new(p)
                    .inspect_err(|e| {
                        tracing::warn!(pattern = %p, error = %e, "Invalid deny_pattern regex");
                    })
                    .ok()
            })
            .collect();

        let allow_binaries = if config.shell.allowed_commands.binaries.is_empty() {
            None // permissive mode
        } else {
            Some(config.shell.allowed_commands.binaries.clone())
        };

        Self {
            auto_approve_prefixes: config.shell.auto_approve.prefixes.clone(),
            deny_patterns,
            allow_binaries,
        }
    }

    /// Extract the first word (the binary name) from a command string.
    /// Handles quoted binaries like `"my tool" arg` and `'my tool' arg`.
    fn extract_binary(command: &str) -> &str {
        let trimmed = command.trim();
        if let Some(rest) = trimmed.strip_prefix('"') {
            rest.split('"').next().unwrap_or(trimmed)
        } else if let Some(rest) = trimmed.strip_prefix('\'') {
            rest.split('\'').next().unwrap_or(trimmed)
        } else {
            trimmed.split_whitespace().next().unwrap_or(trimmed)
        }
    }

    /// Classify a command.  The checks are applied in priority order:
    /// deny → strict allowlist → auto-approve → fallthrough.
    pub fn classify(&self, command: &str) -> CommandVerdict {
        if command.trim().is_empty() {
            tracing::warn!("Empty shell command classified");
        }

        let binary = Self::extract_binary(command);

        // 1. Strict allowlist mode
        if let Some(ref allowed) = self.allow_binaries
            && !allowed.iter().any(|a| a == binary)
        {
            tracing::debug!(
                binary = %binary,
                "Classified shell command: blocked (not in allowed-commands list)"
            );
            return CommandVerdict::Blocked {
                reason: format!("'{binary}' is not in the allowed-commands list"),
            };
        }

        // 2. Deny patterns (checked against the full command string)
        for re in &self.deny_patterns {
            if re.is_match(command) {
                tracing::debug!(
                    binary = %binary,
                    pattern = %re.as_str(),
                    "Classified shell command: blocked (deny pattern matched)"
                );
                return CommandVerdict::Blocked {
                    reason: format!("command matches deny-pattern '{}'", re.as_str()),
                };
            }
        }

        // 3. Auto-approve prefixes
        for prefix in &self.auto_approve_prefixes {
            if binary == prefix.as_str() {
                tracing::debug!(
                    binary = %binary,
                    "Classified shell command: auto-approved"
                );
                return CommandVerdict::AutoApproved;
            }
            // Also check "binary args..." against prefix (handles things
            // like "git status", "cargo build" matching "git" / "cargo").
            if command.starts_with(prefix)
                && command
                    .as_bytes()
                    .get(prefix.len())
                    .is_none_or(|&b| b == b' ')
            {
                tracing::debug!(
                    binary = %binary,
                    prefix = %prefix,
                    "Classified shell command: auto-approved (prefix match)"
                );
                return CommandVerdict::AutoApproved;
            }
        }

        // 4. Fallthrough — requires user approval
        tracing::debug!(
            binary = %binary,
            "Classified shell command: requires user approval"
        );
        CommandVerdict::RequiresApproval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_filter() -> ShellFilter {
        ShellFilter::from_config(&SandboxConfig::default())
    }

    #[test]
    fn test_auto_approve_git_status() {
        let filter = make_filter();
        assert_eq!(filter.classify("git status"), CommandVerdict::AutoApproved);
    }

    #[test]
    fn test_auto_approve_cargo_build() {
        let filter = make_filter();
        assert_eq!(
            filter.classify("cargo build --release"),
            CommandVerdict::AutoApproved
        );
    }

    #[test]
    fn test_auto_approve_echo() {
        let filter = make_filter();
        assert_eq!(
            filter.classify("echo hello world"),
            CommandVerdict::AutoApproved
        );
    }

    #[test]
    fn test_block_rm_rf_root() {
        let filter = make_filter();
        match filter.classify("rm -rf /") {
            CommandVerdict::Blocked { reason } => {
                assert!(reason.contains("deny-pattern"));
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn test_block_sudo() {
        let filter = make_filter();
        match filter.classify("sudo rm something") {
            CommandVerdict::Blocked { reason } => {
                assert!(reason.contains("deny-pattern"));
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn test_block_shutdown() {
        let filter = make_filter();
        match filter.classify("shutdown /s") {
            CommandVerdict::Blocked { .. } => {}
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn test_requires_approval_unknown_command() {
        let filter = make_filter();
        assert_eq!(
            filter.classify("curl https://example.com"),
            CommandVerdict::RequiresApproval
        );
    }

    #[test]
    fn test_strict_allowlist_mode() {
        let mut config = SandboxConfig::default();
        config.shell.allowed_commands.binaries = vec!["cargo".into(), "git".into()];
        let filter = ShellFilter::from_config(&config);

        // In allowlist — auto-approved (also in auto_approve list)
        assert_eq!(filter.classify("cargo build"), CommandVerdict::AutoApproved);

        // NOT in allowlist — blocked
        match filter.classify("python script.py") {
            CommandVerdict::Blocked { reason } => {
                assert!(reason.contains("not in the allowed-commands"));
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn test_quoted_binary() {
        let filter = make_filter();
        // A quoted command that isn't auto-approved should require approval
        let verdict = filter.classify("\"some tool\" arg");
        assert_eq!(verdict, CommandVerdict::RequiresApproval);
    }
}
