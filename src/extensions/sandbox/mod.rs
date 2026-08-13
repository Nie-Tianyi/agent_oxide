//! Sandbox runtime components — 5-layer security system.
//!
//! The Windows encoding shim ([`encoding`]) uses raw Win32 FFI, so this
//! module opts out of the crate-wide `#![deny(unsafe_code)]`.
#![allow(unsafe_code)]
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
//! Layers 4–5, output truncation, and the second-pass policy check are
//! composed into the in-library [`shell_tool`] ([`ShellTool`]) +
//! [`shell_runner`] ([`ShellRunner`]) — downstream crates register
//! `ShellTool` instead of hand-wiring the execution chain.
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
pub mod shell_runner;
pub mod shell_tool;
pub mod watchdog;

pub use config::{ConfigError, FilesystemConfig, SandboxConfig};
pub use fs::{DirEntry, EditSpan, EntryType, FsError, GrepMatch, WorkspaceFs};
pub use sandbox_hook::SandboxHook;
pub use shell_filter::{CommandVerdict, ShellFilter};
pub use shell_runner::{ShellOutput, ShellRunner, ShellRunnerError};
pub use shell_tool::{ShellTool, ToolApprovalMode};
pub use watchdog::Watchdog;
