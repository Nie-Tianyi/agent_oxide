//! Per-session resource quota tracking.
//!
//! Tracks cumulative operation counts and concurrent shell invocations
//! so we can reject tool calls when quotas are exhausted.

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use tools::SandboxConfig;

/// Tracks resource consumption per session.
///
/// Shared between hooks and tools via `Arc<ResourceTracker>`.
pub struct ResourceTracker {
    sessions: RwLock<HashMap<String, SessionStats>>,
    max_total_operations: usize,
    max_concurrent_shells: usize,
}

struct SessionStats {
    total_operations: AtomicUsize,
    active_shells: AtomicUsize,
}

impl ResourceTracker {
    pub fn new(config: &SandboxConfig) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            max_total_operations: config.quotas.max_total_operations,
            max_concurrent_shells: config.quotas.max_concurrent_shells,
        }
    }

    /// Check quotas before an operation. Returns `Ok(())` if within limits.
    pub fn check(&self, session_id: &str, tool_name: &str) -> Result<(), String> {
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        let stats = sessions.entry(session_id.to_string()).or_default();

        if stats.total_operations.load(Ordering::Relaxed) >= self.max_total_operations {
            return Err(format!(
                "session quota exceeded: {} total operations",
                self.max_total_operations
            ));
        }

        if tool_name == "shell" {
            let current = stats.active_shells.load(Ordering::Relaxed);
            if current >= self.max_concurrent_shells {
                return Err(format!(
                    "too many concurrent shells (max {})",
                    self.max_concurrent_shells
                ));
            }
            stats.active_shells.fetch_add(1, Ordering::Relaxed);
        }

        Ok(())
    }

    /// Cancel a previously-checked operation — decrements the concurrent
    /// counter without recording a completed operation.  Call this when a
    /// tool passes `check()` but is subsequently rejected (e.g. by
    /// ShellFilter or user denial in `before_tool_call`).
    pub fn cancel(&self, session_id: &str, tool_name: &str) {
        if tool_name != "shell" {
            return;
        }
        let sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        if let Some(stats) = sessions.get(session_id) {
            stats.active_shells.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Record that an operation completed (must be paired with `check`).
    pub fn record(&self, session_id: &str, tool_name: &str) {
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        // Use entry().or_default() so missing sessions are created rather
        // than silently ignored — ensures counters always converge.
        let stats = sessions.entry(session_id.to_string()).or_default();
        stats.total_operations.fetch_add(1, Ordering::Relaxed);
        if tool_name == "shell" {
            stats.active_shells.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Default for SessionStats {
    fn default() -> Self {
        Self {
            total_operations: AtomicUsize::new(0),
            active_shells: AtomicUsize::new(0),
        }
    }
}
