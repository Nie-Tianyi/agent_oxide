//! Sandbox runtime components — 5-layer security system.
//!
//! # Layers
//!
//! | Layer | Component | Role |
//! |---|---|---|
//! | 1 | [`fs`] ([`WorkspaceFs`]) | Path sandbox — canonicalization, file-size caps, extension blocklist, TOCTOU re-check |
//! | 2 | [`shell_filter`] | Command classification — auto-approve / deny / prompt |
//! | 3 | [`sandbox_hook`] ([`SandboxHook`]) | Orchestrator — quotas, user prompts, audit logging |
//! | 4 | [`env_sanitizer`] | Clears dangerous env vars in child processes |
//! | 5 | [`watchdog`] ([`Watchdog`]) | Kills process tree on timeout |
//!
//! Plus supporting infrastructure: [`config`] (policy types),
//! [`resource_tracker`] (quotas), [`audit_logger`] (JSONL audit trail),
//! [`encoding`] (output encoding).

pub mod audit_logger;
pub mod config;
pub mod encoding;
pub mod env_sanitizer;
pub mod fs;
pub mod resource_tracker;
pub mod sandbox_hook;
pub mod shell_filter;
pub mod watchdog;

pub use config::{ConfigError, FilesystemConfig, SandboxConfig};
pub use fs::{DirEntry, EditSpan, EntryType, FsError, GrepMatch, WorkspaceFs};
pub use sandbox_hook::SandboxHook;
pub use watchdog::Watchdog;
