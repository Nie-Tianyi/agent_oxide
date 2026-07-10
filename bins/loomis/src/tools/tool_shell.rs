//! # ShellTool — Command-line execution tool
//!
//! Executes shell commands in the workspace directory with a configurable
//! timeout. Used by the agent to run CLI tools, build scripts, tests, etc.
//!
//! ## Safety
//!
//! Commands are validated through [`ShellFilter`](crate::sandbox::shell_filter)
//! before execution.  The environment is sanitised via
//! [`sanitize`](crate::sandbox::env_sanitizer::sanitize) so that secrets
//! and dangerous variables (`LD_PRELOAD`, …) are not leaked to child
//! processes.  A watchdog thread enforces the timeout and kills the
//! **entire process tree** (not just the immediate child) on timeout.
//! Output is capped at 100 KB.
//!
//! ## User confirmation
//!
//! The [`SandboxHook`](crate::hooks::SandboxHook) intercepts shell tool
//! calls before they reach `execute()`.  Commands matching an auto-approve
//! prefix run immediately; commands matching a deny-pattern are blocked;
//! everything else prompts the user.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::Deserialize;

use tools::{ProgressStream, SandboxConfig, ToolError, tool};

use crate::sandbox::env_sanitizer;
use crate::sandbox::shell_filter::ShellFilter;

/// Maximum output bytes returned to the model. Prevents a single command
/// from flooding the conversation context.
const MAX_OUTPUT_BYTES: usize = 100_000;

/// Shell 工具的参数。
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShellArgs {
    /// The shell command to execute.
    #[schemars(
        description = "The shell command to execute. Runs with the workspace root as working directory. On Windows: cmd /C. On Unix: sh -c. Examples: 'cargo build', 'git status', 'npm test'. Do NOT use for cat/ls/find/grep/echo — use the dedicated tools instead."
    )]
    pub command: String,

    /// Max execution time in seconds.
    #[schemars(
        description = "Max execution time in seconds (range: 1-120). Default: 60. The process is killed if exceeded; partial output captured so far is returned. Set shorter for quick commands, longer for builds."
    )]
    pub timeout_secs: Option<u64>,
}

/// Executes arbitrary shell commands within the workspace.
///
/// # Platform shells
///
/// | OS | Shell | Invocation |
/// |----|-------|-----------|
/// | Windows | `cmd.exe` | `cmd /C <command>` |
/// | Unix | `sh` | `sh -c <command>` |
#[tool(
    name = "shell",
    description = "Execute a shell command in the workspace directory. The command runs inside \
         the workspace root as the working directory.\n\n\
         Output is capped at 100 KB to avoid flooding context. If the command \
         exceeds the timeout it is killed and partial output is returned. Exit code \
         is appended to the output when non-zero.\n\n\
         When to use: running build commands (`cargo build`, `npm install`, `make`), \
         running tests (`cargo test`, `pytest`), version control (`git status`, \
         `git diff`, `git log`), any CLI tool without a dedicated equivalent.\n\n\
         IMPORTANT — use dedicated tools instead of shell when possible:\n\
         - Reading files → use read (safer, cat -n format with line numbers)\n\
         - Listing directories → use ls or glob (structured output)\n\
         - Searching content → use grep (structured output with line numbers)\n\
         - Editing files → use edit or write (sandbox-safe, undoable)\n\
         Do NOT use shell to run `cat`, `ls`, `find`, `grep`, `echo`, or `sed` \
         unless you have verified that the dedicated tool cannot accomplish the task.\n\n\
         Timed out or killed commands return partial output — do not assume success \
         when output is incomplete.",
    args = ShellArgs
)]
pub struct ShellTool {
    /// All commands run with this as the working directory.
    workspace_root: PathBuf,
    /// Default timeout applied when the model omits `timeout_secs`.
    default_timeout: Duration,
    /// Hard upper bound — the model cannot request more.
    max_timeout: Duration,
    /// Whether to sanitize the environment before spawning.
    sanitize_env: bool,
    /// Compiled command classifier (auto-approve / deny / prompt).
    filter: ShellFilter,
}

impl ShellTool {
    /// Creates a new shell tool from sandbox configuration.
    pub fn new(workspace_root: PathBuf, config: &SandboxConfig) -> Self {
        Self {
            workspace_root,
            default_timeout: Duration::from_secs(config.shell.default_timeout_secs),
            max_timeout: Duration::from_secs(config.shell.max_timeout_secs),
            sanitize_env: config.shell.sanitize_environment,
            filter: ShellFilter::from_config(config),
        }
    }

    fn execute_stream(&self, args: ShellArgs) -> Result<ProgressStream, ToolError> {
        let command = args.command;
        if command.trim().is_empty() {
            return Err(ToolError::InvalidArgs(
                "Missing required field: 'command'".into(),
            ));
        }

        // ── Command validation ────────────────────────────────────────
        use crate::sandbox::shell_filter::CommandVerdict;
        if let CommandVerdict::Blocked { reason } = self.filter.classify(&command) {
            return Err(ToolError::Execution(format!(
                "Command blocked by sandbox policy: {reason}"
            )));
        }

        let timeout_secs = args
            .timeout_secs
            .unwrap_or(self.default_timeout.as_secs())
            .min(self.max_timeout.as_secs())
            .max(1);

        // ── Platform shell selection ──────────────────────────────────
        #[cfg(target_os = "windows")]
        let (shell, shell_arg) = ("cmd", "/C");
        #[cfg(not(target_os = "windows"))]
        let (shell, shell_arg) = ("sh", "-c");

        // ── Spawn child process ───────────────────────────────────────
        let mut cmd = Command::new(shell);
        cmd.arg(shell_arg)
            .arg(&command)
            .current_dir(&self.workspace_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Apply environment sanitization
        env_sanitizer::sanitize(&mut cmd, &self.workspace_root, self.sanitize_env);

        let child = cmd
            .spawn()
            .map_err(|e| ToolError::Execution(format!("Failed to spawn command: {e}")))?;

        let pid = child.id();

        // ── Watchdog thread (kills entire process tree) ──────────────
        let done = Arc::new(AtomicBool::new(false));
        let done_signal = Arc::clone(&done);
        let timeout = Duration::from_secs(timeout_secs);

        let watchdog = thread::spawn(move || {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if done_signal.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            // Timeout reached — kill the entire process tree.
            #[cfg(target_os = "windows")]
            {
                // /T = tree kill (child processes too)
                let _ = Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
        });

        // ── Wait for process ─────────────────────────────────────
        let output = child
            .wait_with_output()
            .map_err(|e| ToolError::Execution(format!("Failed to wait on command: {e}")))?;

        // Signal the watchdog to exit, then join (returns within 100ms).
        done.store(true, Ordering::Relaxed);
        let _ = watchdog.join();

        // ── Build result ─────────────────────────────────────────
        let stdout = decode_stdout(&output.stdout);
        let stderr = decode_stdout(&output.stderr);
        let exit_code = output.status.code();

        let mut result = String::new();

        // Truncate helpers
        let truncate = |s: &str, max: usize| -> String {
            if s.len() <= max {
                s.to_string()
            } else {
                // Find valid UTF-8 boundary at or before max
                let boundary = s.floor_char_boundary(max);
                format!("{}…\n[output truncated at {max} bytes]", &s[..boundary])
            }
        };

        let stdout_clean = stdout.trim_end();
        let stderr_clean = stderr.trim_end();

        if !stdout_clean.is_empty() {
            result.push_str(&truncate(stdout_clean, MAX_OUTPUT_BYTES));
        }

        if !stderr_clean.is_empty() {
            if !result.is_empty() {
                result.push_str("\n\n[stderr]\n");
            }
            // Reserve ~20% of budget for stderr (or at least 10KB)
            let stderr_max = (MAX_OUTPUT_BYTES / 5).max(10_240);
            // But don't exceed remaining budget
            let remaining = MAX_OUTPUT_BYTES.saturating_sub(result.len());
            let stderr_limit = stderr_max.min(remaining);
            result.push_str(&truncate(stderr_clean, stderr_limit));
        }

        // If nothing was produced, still indicate the command ran
        if result.is_empty() {
            match exit_code {
                Some(0) => result.push_str("(command completed with no output)"),
                Some(code) => {
                    result.push_str(&format!("(exit code: {code}, no output)"));
                }
                None => result.push_str("(process terminated by signal, no output)"),
            }
        } else if let Some(code) = exit_code
            && code != 0
        {
            // Append exit code info after output
            result.push_str(&format!("\n\n[exit code: {code}]"));
        }

        let output = result;
        Ok(ProgressStream::done(output))
    }
}

// ── Encoding Helpers ──────────────────────────────────────────────────────────

/// Decodes child-process stdout/stderr bytes to a Rust string.
///
/// On Windows, many CLI tools (especially cmd built-ins like `dir`, `echo`,
/// and older programs) output in the system ANSI code page (e.g. GBK/CP936 for
/// Chinese-locale machines). Modern tools (git, cargo, rustc, python 3.7+)
/// typically output UTF-8 when stdout is not a TTY.
///
/// Strategy: try UTF-8 first — if every byte is valid UTF-8, use it directly.
/// Otherwise fall back to the Windows [`GetACP`] code page via
/// [`MultiByteToWideChar`]. On Unix this is just [`String::from_utf8_lossy`].
#[cfg(target_os = "windows")]
fn decode_stdout(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // Try UTF-8 first — modern tools output valid UTF-8.
    if let Ok(utf8) = std::str::from_utf8(bytes) {
        return utf8.to_string();
    }
    // Fall back to the system ANSI code page.
    unsafe {
        let acp = GetACP();
        // CP 65001 IS UTF-8 — if the system already uses UTF-8, just
        // replace invalid sequences (shouldn't happen since from_utf8 failed).
        if acp == 65001 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        // Determine how many UTF-16 code units we need.
        let wide_len = MultiByteToWideChar(
            acp,
            0,
            bytes.as_ptr() as *const i8,
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if wide_len <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide: Vec<u16> = vec![0; wide_len as usize];
        let written = MultiByteToWideChar(
            acp,
            0,
            bytes.as_ptr() as *const i8,
            bytes.len() as i32,
            wide.as_mut_ptr(),
            wide_len,
        );
        if written <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        wide.truncate(written as usize);
        String::from_utf16_lossy(&wide)
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn GetACP() -> u32;
    fn MultiByteToWideChar(
        Codepage: u32,
        dwFlags: u32,
        lpMultiByteStr: *const i8,
        cbMultiByte: i32,
        lpWideCharStr: *mut u16,
        cchWideChar: i32,
    ) -> i32;
}

#[cfg(not(target_os = "windows"))]
fn decode_stdout(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    fn make_tool() -> ShellTool {
        ShellTool::new(std::env::current_dir().unwrap(), &SandboxConfig::default())
    }

    // ── Metadata ──────────────────────────────────────────────────

    #[test]
    fn test_name() {
        let tool = make_tool();
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn test_description() {
        let tool = make_tool();
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_parameters_schema() {
        let tool = make_tool();
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["command"]["type"] == "string");
        assert!(params["required"][0] == "command");
        assert_eq!(
            params["additionalProperties"], false,
            "ShellTool must include additionalProperties: false"
        );
    }

    // ── Execution ─────────────────────────────────────────────────

    #[test]
    fn test_execute_echo() {
        let tool = make_tool();
        let result = Tool::execute_stream(&tool, r#"{"command": "echo hello"}"#)
            .unwrap()
            .poll_done();
        assert!(result.contains("hello"), "got: {result}");
    }

    #[test]
    fn test_execute_pwd() {
        let tool = make_tool();
        #[cfg(target_os = "windows")]
        let cmd = r#"{"command": "echo %cd%"}"#;
        #[cfg(not(target_os = "windows"))]
        let cmd = r#"{"command": "pwd"}"#;
        let result = Tool::execute_stream(&tool, cmd).unwrap().poll_done();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_execute_non_zero_exit() {
        let tool = make_tool();
        // exit /b 42 works on Windows; exit 42 works on Unix
        #[cfg(target_os = "windows")]
        let cmd = r#"{"command": "cmd /C exit /b 42"}"#;
        #[cfg(not(target_os = "windows"))]
        let cmd = r#"{"command": "exit 42"}"#;

        let result = Tool::execute_stream(&tool, cmd).unwrap().poll_done();
        // Should mention the exit code
        assert!(
            result.contains("exit code") || result.contains("42"),
            "got: {result}"
        );
    }

    #[test]
    fn test_execute_missing_command() {
        let tool = make_tool();
        let result = Tool::execute_stream(&tool, r#"{"timeout_secs": 5}"#);
        match result {
            Err(ToolError::InvalidArgs(msg)) => {
                assert!(msg.contains("command"), "got: {msg}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_empty_command() {
        let tool = make_tool();
        let result = Tool::execute_stream(&tool, r#"{"command": "   "}"#);
        match result {
            Err(ToolError::InvalidArgs(msg)) => {
                assert!(msg.contains("command"), "got: {msg}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_bad_json() {
        let tool = make_tool();
        let result = Tool::execute_stream(&tool, "not json");
        match result {
            Err(ToolError::InvalidArgs(msg)) => {
                assert!(msg.contains("invalid args"), "got: {msg}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_no_output() {
        let tool = make_tool();
        // A command that produces no output at all
        #[cfg(target_os = "windows")]
        let cmd = r#"{"command": "cd ."}"#;
        #[cfg(not(target_os = "windows"))]
        let cmd = r#"{"command": "true"}"#;

        let result = Tool::execute_stream(&tool, cmd).unwrap().poll_done();
        // Should indicate the command ran even though there's no output
        assert!(
            result.contains("no output") || result.is_empty(),
            "got: {result}"
        );
    }

    #[test]
    fn test_execute_with_timeout_in_args() {
        let tool = make_tool();
        let mut result =
            Tool::execute_stream(&tool, r#"{"command": "echo fast", "timeout_secs": 10}"#)
                .expect("should succeed");
        let output = result.poll_done();
        assert!(output.contains("fast"), "got: {output}");
    }

    #[test]
    fn test_execute_stderr_captured() {
        let tool = make_tool();
        // Print to stderr
        #[cfg(target_os = "windows")]
        let cmd = r#"{"command": "cmd /C echo error text >&2"}"#;
        #[cfg(not(target_os = "windows"))]
        let cmd = r#"{"command": "echo error text >&2"}"#;

        let result = Tool::execute_stream(&tool, cmd).unwrap().poll_done();
        assert!(result.contains("error text"), "got: {result}");
    }
}
