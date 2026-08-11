//! Process watchdog — kills a child process (by PID) if it exceeds a
//! timeout.
//!
//! Shared between `ShellTool` and user `!command` execution in downstream
//! crates (e.g. your binary).

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A watchdog thread that kills a process (by PID) if it exceeds a timeout.
///
/// The watchdog checks a shared [`AtomicBool`] every 100ms. When the process
/// finishes normally, the owner calls [`disarm()`](Watchdog::disarm) which
/// sets the flag and joins the thread. This guarantees `disarm()` returns
/// within 100ms regardless of the timeout duration.
///
/// # Platform behaviour
///
/// | OS | Kill command |
/// |----|-------------|
/// | Windows | `taskkill /F /T /PID <pid>` (tree kill) |
/// | Unix | `kill -9 <pid>` |
///
/// # Example
///
/// ```ignore
/// let child = Command::new("long-running-process").spawn()?;
/// let watchdog = Watchdog::spawn(child.id(), Duration::from_secs(30));
/// let output = child.wait_with_output()?;
/// watchdog.disarm(); // returns within 100ms
/// ```
pub struct Watchdog {
    /// Shared flag — set to `true` by `disarm()` to signal the watchdog
    /// thread that the process finished normally.
    done: Arc<AtomicBool>,
    /// The watchdog thread handle. `None` only during the `disarm` drop.
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    /// Spawn a watchdog for the given PID with the given timeout.
    ///
    /// The watchdog polls `done` every 100ms. If `done` is still `false`
    /// when the timeout expires, it kills the process identified by `pid`.
    /// The kill is best-effort — if the PID is invalid (process already
    /// exited), the kill command is a no-op.
    pub fn spawn(pid: u32, timeout: Duration) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let done_signal = Arc::clone(&done);

        let thread = std::thread::spawn(move || {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if done_signal.load(Ordering::Relaxed) {
                    tracing::debug!(pid, "Process finished normally before watchdog timeout");
                    return; // process finished normally
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // Timeout reached — best-effort kill.
            tracing::warn!(
                pid,
                timeout_secs = timeout.as_secs(),
                "Watchdog timeout reached — killing process"
            );
            #[cfg(target_os = "windows")]
            {
                // /T = tree kill (child processes too)
                if let Err(e) = Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    tracing::error!(pid, error = %e, "Failed to spawn taskkill for watchdog kill");
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                if let Err(e) = Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    tracing::error!(pid, error = %e, "Failed to spawn kill for watchdog kill");
                }
            }
        });

        Self {
            done,
            thread: Some(thread),
        }
    }

    /// Signal that the process finished normally and wait for the watchdog
    /// thread to exit.
    ///
    /// Sets the shared flag and joins the watchdog thread. Because the
    /// watchdog polls every 100ms, this method returns within 100ms
    /// regardless of the original timeout duration.
    ///
    /// # Panics
    ///
    /// Panics if the watchdog thread panicked (unexpected — the thread
    /// body never unwraps or panics).
    pub fn disarm(mut self) {
        // Signal the watchdog to exit.
        self.done.store(true, Ordering::Relaxed);
        // Join the thread — returns within 100ms.
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        // Prevent the Drop impl from trying to join again.
    }
}

impl Drop for Watchdog {
    /// If `disarm()` was not called (e.g. early return due to error),
    /// signal the watchdog and join the thread. This prevents orphan
    /// watchdog threads from accumulating.
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disarm_returns_quickly() {
        // Spawn a watchdog with a very long timeout, then disarm immediately.
        let watchdog = Watchdog::spawn(99999, Duration::from_secs(3600));
        let start = Instant::now();
        watchdog.disarm();
        let elapsed = start.elapsed();
        // Should return within 200ms (the 100ms poll + join overhead).
        assert!(
            elapsed < Duration::from_millis(200),
            "disarm took too long: {elapsed:?}"
        );
    }

    #[test]
    fn test_disarm_idempotent_via_drop() {
        // Verify that dropping a watchdog without calling disarm() doesn't
        // panic (the Drop impl handles it).
        let watchdog = Watchdog::spawn(99999, Duration::from_secs(3600));
        drop(watchdog); // should not panic
    }

    #[test]
    fn test_watchdog_kills_nonexistent_pid_without_panic() {
        // Use a very short timeout on a non-existent PID.
        // The kill command will fail silently — no panic.
        let watchdog = Watchdog::spawn(99999, Duration::from_millis(50));
        // Wait for the watchdog to fire.
        std::thread::sleep(Duration::from_millis(200));
        // Disarming after timeout should still work (done flag already set
        // by the Drop prevention — actually the thread already returned).
        watchdog.disarm();
    }

    #[test]
    fn test_disarm_after_timeout_does_not_panic() {
        // Spawn with a very short timeout so the watchdog fires.
        let watchdog = Watchdog::spawn(99999, Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(100));
        // Disarming after the watchdog has already returned should be fine.
        watchdog.disarm();
    }
}
