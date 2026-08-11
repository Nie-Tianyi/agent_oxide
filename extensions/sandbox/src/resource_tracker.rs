//! Per-session resource quota tracking.
//!
//! Tracks cumulative operation counts and concurrent shell invocations
//! so we can reject tool calls when quotas are exhausted.

use crate::config::SandboxConfig;
use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};

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

        let current_total = stats.total_operations.load(Ordering::Relaxed);
        if current_total >= self.max_total_operations {
            tracing::warn!(
                session_id,
                tool = tool_name,
                metric = "total_operations",
                limit = self.max_total_operations,
                current = current_total,
                "Resource quota exceeded: total operations"
            );
            return Err(format!(
                "session quota exceeded: {} total operations",
                self.max_total_operations
            ));
        }

        if tool_name == "shell" {
            let current = stats.active_shells.load(Ordering::Relaxed);
            if current >= self.max_concurrent_shells {
                tracing::warn!(
                    session_id,
                    tool = tool_name,
                    metric = "concurrent_shells",
                    limit = self.max_concurrent_shells,
                    current,
                    "Resource quota exceeded: concurrent shells"
                );
                return Err(format!(
                    "too many concurrent shells (max {})",
                    self.max_concurrent_shells
                ));
            }
            stats.active_shells.fetch_add(1, Ordering::Relaxed);
        }

        tracing::debug!(session_id, tool = tool_name, "Resource quota check passed");
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tracker() -> ResourceTracker {
        ResourceTracker::new(&SandboxConfig::default())
    }

    fn make_tracker_with_limits(max_total: usize, max_shells: usize) -> ResourceTracker {
        let mut config = SandboxConfig::default();
        config.quotas.max_total_operations = max_total;
        config.quotas.max_concurrent_shells = max_shells;
        ResourceTracker::new(&config)
    }

    #[test]
    fn test_check_passes_within_limits() {
        let tracker = make_tracker();
        assert!(tracker.check("s1", "read").is_ok());
        assert!(tracker.check("s1", "grep").is_ok());
    }

    #[test]
    fn test_check_fails_total_operations() {
        let tracker = make_tracker_with_limits(1, 10);
        // check() passes — no operations recorded yet.
        assert!(tracker.check("s1", "read").is_ok());
        // record() increments the counter.
        tracker.record("s1", "read");
        // Now the next check should fail because total >= max (1).
        let err = tracker.check("s1", "write").unwrap_err();
        assert!(err.contains("quota exceeded"), "got: {err}");
    }

    #[test]
    fn test_check_fails_concurrent_shells() {
        let tracker = make_tracker_with_limits(100, 1);
        assert!(tracker.check("s1", "shell").is_ok());
        let err = tracker.check("s1", "shell").unwrap_err();
        assert!(err.contains("too many concurrent shells"), "got: {err}");
    }

    #[test]
    fn test_record_decrements_active_shells() {
        let tracker = make_tracker_with_limits(100, 1);
        tracker.check("s1", "shell").unwrap();
        // Record the shell as done; concurrent counter should drop.
        tracker.record("s1", "shell");
        // Now another shell should be allowed.
        assert!(tracker.check("s1", "shell").is_ok());
    }

    #[test]
    fn test_cancel_decrements_active_shells() {
        let tracker = make_tracker_with_limits(100, 1);
        tracker.check("s1", "shell").unwrap();
        // Cancel (no execution) — concurrent counter should drop.
        tracker.cancel("s1", "shell");
        // Now another shell should be allowed.
        assert!(tracker.check("s1", "shell").is_ok());
    }

    #[test]
    fn test_cancel_non_shell_noop() {
        let tracker = make_tracker();
        // Cancelling a non-shell tool should not panic.
        tracker.check("s1", "read").unwrap();
        tracker.cancel("s1", "read");
    }

    #[test]
    fn test_multiple_sessions_independent() {
        let tracker = make_tracker_with_limits(100, 1);
        // Session 1 takes the shell slot.
        assert!(tracker.check("s1", "shell").is_ok());
        // Session 2 is independent — should also be allowed.
        assert!(tracker.check("s2", "shell").is_ok());
        // Session 1 should be blocked on second shell.
        assert!(tracker.check("s1", "shell").is_err());
    }

    #[test]
    fn test_record_creates_session_if_missing() {
        let tracker = make_tracker();
        // Recording on a never-seen session should not panic.
        tracker.record("new_session", "read");
    }

    #[test]
    fn test_record_increments_total_operations() {
        let tracker = make_tracker_with_limits(2, 10);
        assert!(tracker.check("s1", "read").is_ok());
        tracker.record("s1", "read");
        assert!(tracker.check("s1", "grep").is_ok());
        tracker.record("s1", "grep");
        // Third operation should fail (limit is 2).
        let err = tracker.check("s1", "ls").unwrap_err();
        assert!(err.contains("quota exceeded"), "got: {err}");
    }

    #[test]
    fn test_total_operations_only_after_record() {
        // check() alone doesn't increment the total counter.
        // A new session starts at 0, so max=0 means the first check
        // sees total_operations (0) >= max_operations (0) — rejected.
        let tracker = make_tracker_with_limits(0, 10);
        let err = tracker.check("s1", "read").unwrap_err();
        assert!(err.contains("quota exceeded"), "got: {err}");
    }
}
