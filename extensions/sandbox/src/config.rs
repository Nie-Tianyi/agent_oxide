//! [`SandboxConfig`] — user-facing security policy.
//!
//! All fields are optional in the TOML file; missing keys use the
//! baked-in safe defaults (equivalent to the `"strict"` profile).

use serde::{Deserialize, Serialize};

// ── Filesystem ─────────────────────────────────────────────────────────────────

/// Safety limits applied to file reads and writes by [`WorkspaceFs`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FilesystemConfig {
    /// Maximum bytes that `read()` will return for a single file.
    pub max_read_bytes: usize,
    /// Maximum bytes that `write()` will accept in a single call.
    pub max_write_bytes: usize,
    /// Reject writes whose content contains a null byte (binary heuristic).
    pub forbid_binary_writes: bool,
    /// Reject writes to dot-files (e.g. `.env`, `.gitignore`).
    pub forbid_hidden_file_writes: bool,
    /// File extensions that cannot be created or modified.
    pub blocked_write_extensions: Vec<String>,
    /// Additional **absolute** directories (outside the workspace) that
    /// read-only operations (`read`, `ls`, `glob`, `grep`) may access.
    /// Writes are always confined to the workspace — these roots are
    /// readable, never writable. Defaults to the cargo registry cache
    /// (from `CARGO_HOME` or `~/.cargo/registry`), commonly needed when
    /// searching dependency sources. Override with an explicit list.
    pub read_only_paths: Vec<String>,
}

impl Default for FilesystemConfig {
    fn default() -> Self {
        Self {
            max_read_bytes: 1_048_576, // 1 MiB
            max_write_bytes: 524_288,  // 512 KiB
            forbid_binary_writes: true,
            forbid_hidden_file_writes: true,
            blocked_write_extensions: vec![
                ".exe".into(),
                ".dll".into(),
                ".so".into(),
                ".dylib".into(),
                ".sys".into(),
                ".bin".into(),
            ],
            read_only_paths: default_cargo_registry().into_iter().collect(),
        }
    }
}

/// Auto-detect the cargo registry cache directory if it exists on disk.
///
/// Resolution order: `CARGO_HOME` → `~/.cargo` (via `HOME`/`USERPROFILE`).
/// Returns `None` when cargo's home cannot be located or has no
/// `registry/` subdirectory yet.
fn default_cargo_registry() -> Option<String> {
    let home = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|p| p.join(".cargo"))
        })
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(std::path::PathBuf::from)
                .map(|p| p.join(".cargo"))
        });
    let registry = home.map(|h| h.join("registry"));
    registry
        .filter(|p| p.is_dir())
        .map(|p| p.to_string_lossy().into_owned())
}

// ── Sandbox ────────────────────────────────────────────────────────────────────

/// Root configuration for the sandbox system.
///
/// The config file path is provided by the caller (see
/// [`SandboxConfig::load`]). If the file is missing, a fresh one is
/// written with safe defaults so the user can inspect and customise it.
/// If any key is absent in an existing file,
/// [`SandboxConfig::default`] provides fallback values.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SandboxConfig {
    pub filesystem: FilesystemConfig,
    pub shell: ShellConfig,
    pub quotas: QuotaConfig,
    pub audit: AuditConfig,
}

impl SandboxConfig {
    /// Load config from `config_path` (a `.toml` file).
    ///
    /// When the file does not exist, its parent directory and the file
    /// itself are created with the default values.  If writing the
    /// default file fails, the error is logged (via `tracing`) and the
    /// in-memory defaults are still returned — the application should
    /// not refuse to start just because it cannot persist the config
    /// template.
    pub fn load(config_path: &std::path::Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(config_path) {
            Ok(contents) => {
                let config = toml::from_str(&contents).map_err(ConfigError::Parse)?;
                tracing::info!(path = %config_path.display(), "Loaded sandbox config");
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    path = %config_path.display(),
                    "Sandbox config not found — using safe defaults"
                );
                let default_config = Self::default();
                Self::try_write_default(config_path, &default_config);
                Ok(default_config)
            }
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    /// Best-effort write of the default config to disk.
    ///
    /// Creates the parent directory if needed.  Failures are traced but
    /// never propagated — the caller always continues with in-memory
    /// defaults.
    fn try_write_default(config_path: &std::path::Path, config: &Self) {
        // Ensure the parent directory exists.
        if let Some(parent) = config_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(
                dir = %parent.display(),
                error = %e,
                "Cannot create config directory; config template not written",
            );
            return;
        }

        // Serialise with pretty formatting so the file is human-editable.
        let toml_str = match toml::to_string_pretty(config) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to serialise default config");
                return;
            }
        };

        if let Err(e) = std::fs::write(config_path, &toml_str) {
            tracing::warn!(
                path = %config_path.display(),
                error = %e,
                "Cannot write default config.toml",
            );
        } else {
            tracing::info!(
                path = %config_path.display(),
                "Created config.toml with safe defaults",
            );
        }
    }
}

// ── Shell ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ShellConfig {
    /// Default timeout in seconds when the model omits `timeout_secs`.
    pub default_timeout_secs: u64,
    /// Hard cap on timeout (model cannot request more).
    pub max_timeout_secs: u64,
    /// Maximum bytes returned to the model from a single command.
    pub max_output_bytes: usize,
    /// When true, clear all environment variables and only pass a safe
    /// allowlist before spawning child processes.
    pub sanitize_environment: bool,
    pub auto_approve: AutoApproveConfig,
    pub deny_patterns: DenyPatternsConfig,
    pub allowed_commands: AllowedCommandsConfig,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            default_timeout_secs: 30,
            max_timeout_secs: 120,
            max_output_bytes: 100_000,
            sanitize_environment: true,
            auto_approve: AutoApproveConfig::default(),
            deny_patterns: DenyPatternsConfig::default(),
            allowed_commands: AllowedCommandsConfig::default(),
        }
    }
}

/// Commands whose first word matches one of these prefixes are allowed
/// to run without user confirmation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AutoApproveConfig {
    pub prefixes: Vec<String>,
}

impl Default for AutoApproveConfig {
    fn default() -> Self {
        Self {
            prefixes: vec![
                "cargo".into(),
                "git".into(),
                "npm".into(),
                "node".into(),
                "python".into(),
                "python3".into(),
                "dir".into(),
                "echo".into(),
                "type".into(),
                "ls".into(),
                "cat".into(),
                "head".into(),
                "tail".into(),
                "wc".into(),
                "pwd".into(),
                "date".into(),
                "which".into(),
                "where".into(),
                "printenv".into(),
            ],
        }
    }
}

/// Regex patterns that, when matched against the full command string,
/// cause immediate rejection (no user prompt).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DenyPatternsConfig {
    pub patterns: Vec<String>,
}

impl Default for DenyPatternsConfig {
    fn default() -> Self {
        Self {
            patterns: vec![
                r"rm\s+-rf\s+(/|~)".into(),
                r"sudo\s+".into(),
                r"chmod\s+[0-7]{3,4}\s+/".into(),
                r"dd\s+if=".into(),
                r"mkfs\.".into(),
                "shutdown".into(),
                "reboot".into(),
                r">\s*/dev/".into(),
                r"\|\s*sudo".into(),
            ],
        }
    }
}

/// When non-empty, ONLY these exact binary names are allowed.
/// Empty vec = permissive mode (deny_patterns + auto_approve apply).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AllowedCommandsConfig {
    pub binaries: Vec<String>,
}

// ── Quotas ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct QuotaConfig {
    /// Maximum tool-calling steps per session (already enforced by the
    /// engine, mirrored here for completeness).
    pub max_steps_per_session: usize,
    /// Maximum number of shell commands running concurrently.
    pub max_concurrent_shells: usize,
    /// Hard cap on total tool operations per session.
    pub max_total_operations: usize,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            max_steps_per_session: 50,
            max_concurrent_shells: 2,
            max_total_operations: 10_000,
        }
    }
}

// ── Audit ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AuditConfig {
    /// Master switch for audit logging.
    pub enabled: bool,
    /// Path relative to workspace root for the JSONL audit file.
    pub log_file: String,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            log_file: ".agent/audit.jsonl".into(),
        }
    }
}

// ── Config Error ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error reading config: {e}"),
            Self::Parse(e) => write!(f, "TOML parse error in config: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_only_paths_round_trip() {
        let mut cfg = FilesystemConfig::default();
        cfg.read_only_paths = vec!["C:/some/read/root".into()];
        let s = toml::to_string(&cfg).unwrap();
        assert!(s.contains("read_only_paths"), "got: {s}");
        let back: FilesystemConfig = toml::from_str(&s).unwrap();
        assert_eq!(back.read_only_paths, cfg.read_only_paths);
    }

    #[test]
    fn test_read_only_paths_defaults_from_env() {
        // The default must never be empty-strings or non-absolute entries;
        // whatever cargo home resolves to, entries are real paths.
        let cfg = FilesystemConfig::default();
        assert!(
            cfg.read_only_paths
                .iter()
                .all(|p| !p.trim().is_empty() && std::path::Path::new(p).is_absolute()),
            "got: {:?}",
            cfg.read_only_paths
        );
    }
}
