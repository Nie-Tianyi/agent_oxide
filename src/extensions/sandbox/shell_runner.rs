//! [`ShellRunner`] — the sandboxed shell **execution** layer (layers 4–5
//! of the sandbox system).
//!
//! One component composes the full execution chain that used to be
//! hand-wired in downstream crates:
//!
//! ```text
//! command ──► build (cmd /D /S /C | sh -c) ──► env sanitize
//!          ──► spawn ──► tree watchdog (timeout kill)
//!          ──► bounded stdout/stderr capture (budget kill)
//!          ──► decode (UTF-8 / ANSI) + truncate
//! ```
//!
//! **Policy-free by design** — [`ShellRunner`] never classifies commands.
//! Approval policy lives in [`crate::sandbox::ShellTool`] (second-pass
//! [`crate::sandbox::ShellFilter`] check) and
//! [`crate::sandbox::SandboxHook`].  Never call the runner directly on
//! model-supplied commands — register `ShellTool` instead, so the policy
//! layers always run.

use std::fmt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use super::config::ShellConfig;
use super::encoding::{decode_stdout, truncate_output};
use super::env_sanitizer::sanitize;
use super::watchdog::{self, Watchdog};

/// Output of a sandboxed shell execution.
#[derive(Debug, Clone, Default)]
pub struct ShellOutput {
    /// Decoded, budget-truncated stdout.
    pub stdout: String,
    /// Decoded, budget-truncated stderr.
    pub stderr: String,
    /// Exit code; `None` when the process was killed by a signal.
    pub exit_code: Option<i32>,
    /// True when the watchdog killed the process (timeout exceeded).
    pub timed_out: bool,
    /// True when the output exceeded the capture budget and was cut short.
    pub truncated: bool,
}

/// Errors from [`ShellRunner::run`].
#[derive(Debug)]
pub enum ShellRunnerError {
    /// The command string was empty after trimming.
    EmptyCommand,
    /// The child process could not be spawned (bad workspace root,
    /// missing shell binary, …).
    Spawn(std::io::Error),
    /// An I/O error while waiting for or reaping the child.
    Io(std::io::Error),
}

impl fmt::Display for ShellRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand => write!(f, "shell command is empty"),
            Self::Spawn(e) => write!(f, "failed to spawn shell process: {e}"),
            Self::Io(e) => write!(f, "shell process I/O error: {e}"),
        }
    }
}

impl std::error::Error for ShellRunnerError {}

/// Extra capture budget above `max_output_bytes` so the truncation
/// markers still fit after decoding.
const CAPTURE_SLACK: usize = 256;

/// Per-read chunk size for the pipe readers.
const READ_CHUNK: usize = 8192;

/// Hard cap on waiting for pipe readers after the child exits — a
/// grandchild can hold the pipe write end and delay EOF indefinitely.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval while waiting for the child to exit.
const WAIT_POLL: Duration = Duration::from_millis(50);

/// Sandboxed shell executor — env sanitization, tree-kill watchdog,
/// bounded capture, decoding and truncation in one component.
#[derive(Debug, Clone)]
pub struct ShellRunner {
    workspace_root: PathBuf,
    config: ShellConfig,
}

impl ShellRunner {
    pub fn new(workspace_root: PathBuf, config: ShellConfig) -> Self {
        Self {
            workspace_root,
            config,
        }
    }

    /// Execute `command` through the full sandbox execution chain.
    ///
    /// Blocks the calling thread until the command finishes, times out,
    /// or overflows the output budget (whichever comes first — all
    /// bounded by `max_timeout_secs`).
    ///
    /// Policy-free — the caller must have classified the command (see
    /// module docs).
    pub fn run(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ShellOutput, ShellRunnerError> {
        let command = command.trim();
        if command.is_empty() {
            return Err(ShellRunnerError::EmptyCommand);
        }

        let mut child = self
            .build_command(command)
            .spawn()
            .map_err(ShellRunnerError::Spawn)?;
        let pid = child.id();
        let timeout = self.resolve_timeout(timeout_secs);

        // Start the bounded pipe readers before waiting, so the pipes
        // never fill and block the child.
        let capture = Capture::start(&mut child, self.config.max_output_bytes);
        let watchdog = Watchdog::spawn_tree(pid, timeout);

        // Poll until the child exits, the watchdog kills it, or the
        // output budget overflows (kill early — no point waiting for
        // the timeout while gigabytes stream in).
        let exit_status = loop {
            if capture.truncated() {
                tracing::warn!(
                    pid,
                    command = %command,
                    "Output budget exceeded — killing process tree"
                );
                watchdog::kill_tree(pid);
                break child.wait().map_err(ShellRunnerError::Io)?;
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(WAIT_POLL),
                Err(e) => return Err(ShellRunnerError::Io(e)),
            }
        };

        // Read the fired flag BEFORE disarming — it is the only reliable
        // "was it killed" signal.  Elapsed-time heuristics misreport
        // processes that exit naturally right at the deadline.
        let timed_out = watchdog.fired();
        watchdog.disarm();

        let truncated = capture.truncated();
        let (stdout_bytes, stderr_bytes) = capture.finish(pid);

        let stdout = truncate_output(&decode_stdout(&stdout_bytes), self.config.max_output_bytes);
        let stderr = truncate_output(
            &decode_stdout(&stderr_bytes),
            self.config.max_output_bytes / 4,
        );

        Ok(ShellOutput {
            stdout,
            stderr,
            exit_code: exit_status.code(),
            timed_out,
            truncated,
        })
    }

    /// Resolve the effective timeout: explicit request (clamped into
    /// `1..=max_timeout_secs`) or the configured default.
    fn resolve_timeout(&self, requested: Option<u64>) -> Duration {
        let secs = requested.unwrap_or(self.config.default_timeout_secs);
        Duration::from_secs(secs.clamp(1, self.config.max_timeout_secs))
    }

    /// Apply cwd, stdio wiring, and env sanitization to a shell command.
    fn finish_command(&self, cmd: &mut Command) {
        cmd.current_dir(&self.workspace_root)
            // Null stdin — interactive commands (`more`, REPLs) must not
            // hang on inherited input or steal TUI keystrokes.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        sanitize(cmd, &self.workspace_root, self.config.sanitize_environment);
    }

    #[cfg(target_os = "windows")]
    fn build_command(&self, command: &str) -> Command {
        let mut cmd = Command::new("cmd");
        // `/D` disables the AutoRun registry hook — a command-injection
        // vector the env sanitizer cannot see; `/S` disables cmd's
        // quote-stripping reparse so the single-argument form below is
        // passed through verbatim.
        cmd.args(["/D", "/S", "/C", command]);
        self.finish_command(&mut cmd);
        cmd
    }

    #[cfg(not(target_os = "windows"))]
    fn build_command(&self, command: &str) -> Command {
        use std::os::unix::process::CommandExt;

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        // New process group — the child's pgid equals its pid, so the
        // tree watchdog kills background grandchildren with one group
        // kill instead of leaving them holding the pipe write ends.
        cmd.process_group(0);
        self.finish_command(&mut cmd);
        cmd
    }
}

// ── Bounded capture ──────────────────────────────────────────────────────────

/// Concurrent, budget-bounded stdout/stderr capture.
///
/// Two detached reader threads drain the pipes into channel messages
/// while a shared [`AtomicUsize`] enforces a single output budget across
/// both streams.  Crossing the budget sets `truncated` and stops the
/// reader — the runner reacts by killing the process tree.
struct Capture {
    rx: std::sync::mpsc::Receiver<(bool, Vec<u8>)>,
    truncated: Arc<AtomicBool>,
}

impl Capture {
    fn start(child: &mut Child, max_output_bytes: usize) -> Self {
        let remaining = Arc::new(AtomicUsize::new(max_output_bytes + CAPTURE_SLACK));
        let truncated = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<(bool, Vec<u8>)>();

        // (is_stderr, pipe) — `take()` leaves None behind on failure.
        // The pipes are boxed so `ChildStdout`/`ChildStderr` unify.
        fn boxed(read: impl std::io::Read + Send + 'static) -> Box<dyn std::io::Read + Send> {
            Box::new(read)
        }
        for (is_stderr, pipe) in [
            (false, child.stdout.take().map(boxed)),
            (true, child.stderr.take().map(boxed)),
        ] {
            let tx = tx.clone();
            let remaining = Arc::clone(&remaining);
            let truncated = Arc::clone(&truncated);
            match pipe {
                Some(mut pipe) => {
                    std::thread::spawn(move || {
                        let bytes = read_bounded(&mut pipe, &remaining, &truncated);
                        let _ = tx.send((is_stderr, bytes));
                    });
                }
                None => {
                    std::thread::spawn(move || {
                        let _ = tx.send((is_stderr, Vec::new()));
                    });
                }
            }
        }
        drop(tx); // rx disconnects once both readers exit

        Self { rx, truncated }
    }

    fn truncated(&self) -> bool {
        self.truncated.load(Ordering::Relaxed)
    }

    /// Collect both readers' bytes.  If a reader stalls (a grandchild
    /// still holds the pipe write end after the child exited), kill the
    /// process tree to force EOF.  Bounded by [`REAP_TIMEOUT`].
    fn finish(self, pid: u32) -> (Vec<u8>, Vec<u8>) {
        let deadline = std::time::Instant::now() + REAP_TIMEOUT;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut got = [false; 2];

        while !(got[0] && got[1]) {
            let now = std::time::Instant::now();
            if now >= deadline {
                tracing::warn!(
                    pid,
                    "Timed out waiting for shell output readers — dropping remaining output"
                );
                break;
            }
            match self.rx.recv_timeout(deadline - now) {
                Ok((is_stderr, bytes)) => {
                    if is_stderr {
                        stderr = bytes;
                        got[1] = true;
                    } else {
                        stdout = bytes;
                        got[0] = true;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Reader stalled — kill the tree so the write ends
                    // close and the reader hits EOF.  Idempotent.
                    watchdog::kill_tree(pid);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        (stdout, stderr)
    }
}

/// Drain `pipe` into a buffer, stopping as soon as the shared budget is
/// exhausted (setting `truncated`).  Never blocks on a full pipe.
fn read_bounded(
    pipe: &mut impl std::io::Read,
    remaining: &Arc<AtomicUsize>,
    truncated: &Arc<AtomicBool>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; READ_CHUNK];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                // fetch_sub returns the budget before this read.
                let prev = remaining.fetch_sub(n, Ordering::Relaxed);
                if prev < n {
                    // This read crosses the budget — keep only the part
                    // that fits and stop draining this pipe.
                    truncated.store(true, Ordering::Relaxed);
                    if prev > 0 {
                        buf.extend_from_slice(&chunk[..prev]);
                    }
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(_) => break,
        }
    }
    buf
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Small test config: tight timeout and tiny output budget so
    /// timeout/truncation tests run fast.
    fn test_config() -> ShellConfig {
        ShellConfig {
            default_timeout_secs: 2,
            max_timeout_secs: 5,
            max_output_bytes: 64,
            sanitize_environment: true,
            ..ShellConfig::default()
        }
    }

    fn make_runner(tmp: &tempfile::TempDir) -> ShellRunner {
        ShellRunner::new(tmp.path().to_path_buf(), test_config())
    }

    #[test]
    fn echo_hello() {
        let tmp = tempfile::tempdir().unwrap();
        let out = make_runner(&tmp).run("echo hello", None).unwrap();
        assert!(out.stdout.contains("hello"), "stdout: {:?}", out.stdout);
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert!(!out.truncated);
    }

    #[test]
    fn multiple_commands_pass_through_verbatim() {
        // Verifies the single-argument form is not split or re-parsed —
        // both parts of the chained command must run.
        let tmp = tempfile::tempdir().unwrap();
        #[cfg(target_os = "windows")]
        let out = make_runner(&tmp).run("echo a & echo b", None).unwrap();
        #[cfg(not(target_os = "windows"))]
        let out = make_runner(&tmp).run("echo a; echo b", None).unwrap();
        assert!(out.stdout.contains('a'), "stdout: {:?}", out.stdout);
        assert!(out.stdout.contains('b'), "stdout: {:?}", out.stdout);
    }

    #[test]
    fn exit_code_propagates() {
        // `exit 7` works identically in cmd and sh.
        let tmp = tempfile::tempdir().unwrap();
        let out = make_runner(&tmp).run("exit 7", None).unwrap();
        assert_eq!(out.exit_code, Some(7));
    }

    #[test]
    fn stderr_with_exit_zero_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        #[cfg(target_os = "windows")]
        let out = make_runner(&tmp).run("echo err 1>&2", None).unwrap();
        #[cfg(not(target_os = "windows"))]
        let out = make_runner(&tmp).run("echo err >&2", None).unwrap();
        assert_eq!(out.exit_code, Some(0), "stderr alone must not fail");
        assert!(out.stderr.contains("err"), "stderr: {:?}", out.stderr);
    }

    #[test]
    fn timeout_kills_process() {
        let tmp = tempfile::tempdir().unwrap();
        // ping -n 5 sleeps ~4s on Windows; sleep 5 on Unix.  Both must
        // be killed by the 1-second watchdog.
        #[cfg(target_os = "windows")]
        let command = "ping -n 5 127.0.0.1";
        #[cfg(not(target_os = "windows"))]
        let command = "sleep 5";

        let out = make_runner(&tmp).run(command, Some(1)).unwrap();
        assert!(out.timed_out, "process must have been killed by watchdog");
        #[cfg(not(target_os = "windows"))]
        assert!(
            out.exit_code.is_none(),
            "SIGKILL must report no exit code, got {:?}",
            out.exit_code
        );
        #[cfg(target_os = "windows")]
        assert_ne!(out.exit_code, Some(0));
    }

    #[test]
    fn large_output_is_truncated_and_killed() {
        let tmp = tempfile::tempdir().unwrap();
        // ~550 bytes (Windows) / 400 bytes (Unix) — far beyond the
        // 64+256 capture budget.
        #[cfg(target_os = "windows")]
        let command = "for /L %i in (1,1,50) do @echo 0123456789";
        #[cfg(not(target_os = "windows"))]
        let command = "dd if=/dev/zero bs=200 count=2 | tr '\\0' a";

        let out = make_runner(&tmp).run(command, None).unwrap();
        assert!(out.truncated, "output must be marked truncated");
        assert!(
            out.stdout.len() <= test_config().max_output_bytes + CAPTURE_SLACK,
            "captured stdout must respect the budget: {} bytes",
            out.stdout.len()
        );
    }

    /// Echo the secret variable through a sandboxed shell.
    fn echo_secret(tmp: &tempfile::TempDir) -> ShellOutput {
        #[cfg(target_os = "windows")]
        let command = "echo %AGENT_OXIDE_SANDBOX_TEST_SECRET%";
        #[cfg(not(target_os = "windows"))]
        let command = "echo $AGENT_OXIDE_SANDBOX_TEST_SECRET";
        make_runner(tmp).run(command, None).unwrap()
    }

    #[test]
    fn environment_is_sanitized() {
        // Setting process-global env from a test is inherently racy —
        // use a name unique to this test and restore it afterwards.
        let var = "AGENT_OXIDE_SANDBOX_TEST_SECRET";
        // SAFETY: sandbox module opts out of `deny(unsafe_code)`; the
        // variable is removed right after the run.
        unsafe {
            std::env::set_var(var, "s3cr3t-value");
        }
        let tmp = tempfile::tempdir().unwrap();
        let result = echo_secret(&tmp);
        unsafe {
            std::env::remove_var(var);
        }
        assert!(
            !result.stdout.contains("s3cr3t-value"),
            "secret leaked into sandboxed child: {:?}",
            result.stdout
        );
    }

    #[test]
    fn path_is_preserved() {
        let tmp = tempfile::tempdir().unwrap();
        #[cfg(target_os = "windows")]
        let out = make_runner(&tmp).run("echo %PATH%", None).unwrap();
        #[cfg(not(target_os = "windows"))]
        let out = make_runner(&tmp).run("echo $PATH", None).unwrap();
        assert!(
            !out.stdout.trim().is_empty(),
            "PATH must survive sanitization"
        );
    }

    #[test]
    fn working_directory_is_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        #[cfg(target_os = "windows")]
        let out = make_runner(&tmp).run("cd", None).unwrap();
        #[cfg(not(target_os = "windows"))]
        let out = make_runner(&tmp).run("pwd", None).unwrap();
        let ws = tmp.path().to_string_lossy().to_lowercase();
        assert!(
            out.stdout.to_lowercase().contains(&ws),
            "cwd must be the workspace root: {:?}",
            out.stdout
        );
    }

    #[test]
    fn empty_command_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            make_runner(&tmp).run("   ", None),
            Err(ShellRunnerError::EmptyCommand)
        ));
    }

    #[test]
    fn missing_workspace_fails_to_spawn() {
        let runner = ShellRunner::new(
            PathBuf::from("this/workspace/does/not/exist"),
            test_config(),
        );
        assert!(matches!(
            runner.run("echo hi", None),
            Err(ShellRunnerError::Spawn(_))
        ));
    }

    #[test]
    fn resolve_timeout_clamps_to_max() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = make_runner(&tmp);
        assert_eq!(runner.resolve_timeout(Some(999)), Duration::from_secs(5));
        assert_eq!(runner.resolve_timeout(Some(0)), Duration::from_secs(1));
        assert_eq!(runner.resolve_timeout(None), Duration::from_secs(2));
    }
}
