//! Audit trail — records every shell execution attempt.
//!
//! Writes newline-delimited JSON to the configured audit log path
//! (relative to the workspace root).  A small in-memory ring buffer
//! holds the most recent entries so the UI can display them without
//! re-reading the file.

use crate::config::SandboxConfig;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// Maximum number of entries kept in the in-memory ring buffer.
/// Capacity hint and eviction threshold use the same constant to
/// prevent drift between allocation and logic.
const RING_CAPACITY: usize = 256;

/// Append-only JSONL audit log.
pub struct AuditLogger {
    enabled: bool,
    file: Option<Mutex<File>>,
    ring: Mutex<VecDeque<AuditEntry>>,
    /// Resolved audit file path (for error reporting).
    path: std::path::PathBuf,
}

/// A single audit record.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub session_id: String,
    pub tool: String,
    pub command: String,
    pub verdict: String,
    pub outcome: String,
}

impl AuditLogger {
    pub fn new(config: &SandboxConfig, workspace_root: &Path) -> Self {
        let log_path = workspace_root.join(&config.audit.log_file);
        let file = if config.audit.enabled {
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match OpenOptions::new().create(true).append(true).open(&log_path) {
                Ok(f) => Some(Mutex::new(f)),
                Err(e) => {
                    tracing::error!(
                        path = %log_path.display(),
                        error = %e,
                        "Failed to open audit log file — audit entries will not be persisted"
                    );
                    None
                }
            }
        } else {
            None
        };

        Self {
            enabled: config.audit.enabled,
            file,
            ring: Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
            path: log_path,
        }
    }

    pub fn log(&self, entry: AuditEntry) {
        if !self.enabled {
            return;
        }

        // In-memory ring buffer
        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() >= RING_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(entry.clone());
        }

        // Append to file
        if let Some(file_mutex) = self.file.as_ref()
            && let Ok(mut file) = file_mutex.lock()
        {
            match serde_json::to_string(&entry) {
                Ok(json) => {
                    if let Err(e) = writeln!(file, "{json}") {
                        tracing::error!(
                            path = %self.path.display(),
                            error = %e,
                            "Failed to write audit log entry"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to serialize audit log entry");
                }
            }
        }
    }

    /// Return recent audit entries (for display via `/audit`).
    pub fn recent(&self, count: usize) -> Vec<AuditEntry> {
        if let Ok(ring) = self.ring.lock() {
            ring.iter().rev().take(count).cloned().collect()
        } else {
            vec![]
        }
    }
}
