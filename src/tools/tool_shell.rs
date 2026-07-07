//! # ShellTool — Command-line execution tool
//!
//! Executes shell commands in the workspace directory with a configurable
//! timeout. Used by the agent to run CLI tools, build scripts, tests, etc.
//!
//! ## Safety
//!
//! Commands run in the workspace root directory. A watchdog thread enforces
//! the timeout by killing the process if it runs too long. Output is capped
//! at 100 KB to avoid flooding the conversation context.
//!
//! ## User confirmation
//!
//! This tool is special: the agent loop (see [`crate::core::agent`]) checks
//! for `name == "shell"` and prompts the user for confirmation via the TUI
//! before calling `execute()`. When no confirmation infrastructure is present
//! (e.g. `--no-tui` mode), the tool executes unconditionally.
//!
//! ## Known limitation: blocking `execute`
//!
//! Because [`Tool::execute`] is synchronous, `ShellTool::execute` blocks
//! the calling tokio worker thread for the entire duration of
//! `wait_with_output()` (up to the configured timeout). This is acceptable
//! for short commands but may stall the runtime for long-running builds.
//! Future versions may migrate to `tokio::task::spawn_blocking` or an
//! async `Tool` trait.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::tools::error::ToolError;
use crate::tools::tool::Tool;
use serde_json::{Value, json};

/// Maximum output bytes returned to the model. Prevents a single command
/// from flooding the conversation context.
const MAX_OUTPUT_BYTES: usize = 100_000;

/// Executes arbitrary shell commands within the workspace.
///
/// # Platform shells
///
/// | OS | Shell | Invocation |
/// |----|-------|-----------|
/// | Windows | `cmd.exe` | `cmd /C <command>` |
/// | Unix | `sh` | `sh -c <command>` |
pub struct ShellTool {
    /// All commands run with this as the working directory.
    workspace_root: PathBuf,
    /// Default timeout applied when the model omits `timeout_secs`.
    default_timeout: Duration,
}

impl ShellTool {
    /// Creates a new shell tool.
    ///
    /// `workspace_root` must be an existing directory — it becomes the CWD
    /// of every command executed through this tool.
    ///
    /// `default_timeout` caps how long a single command may run. The model
    /// can request a shorter timeout via the `timeout_secs` parameter, but
    /// cannot exceed this default (the tool clamps it).
    pub fn new(workspace_root: PathBuf, default_timeout: Duration) -> Self {
        Self {
            workspace_root,
            default_timeout,
        }
    }
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the workspace directory. The command runs inside \
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
         when output is incomplete."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute. Runs with the workspace root as working directory. On Windows: cmd /C. On Unix: sh -c. Examples: 'cargo build', 'git status', 'npm test'. Do NOT use for cat/ls/find/grep/echo — use the dedicated tools instead."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Max execution time in seconds (range: 1-120). Default: 60. The process is killed if exceeded; partial output captured so far is returned. Set shorter for quick commands, longer for builds."
                }
            },
            "required": ["command"]
        })
    }

    fn execute(&self, args: &str) -> Result<String, ToolError> {
        // ── Parse arguments ───────────────────────────────────────
        let parsed: Value = serde_json::from_str(args)
            .map_err(|e| ToolError::InvalidArgs(format!("Invalid JSON: {e}")))?;

        let command = parsed
            .get("command")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ToolError::InvalidArgs("Missing required field: 'command'".into()))?;

        let timeout_secs = parsed
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.default_timeout.as_secs())
            .min(self.default_timeout.as_secs())
            .max(1);

        // ── Platform shell selection ──────────────────────────────
        #[cfg(target_os = "windows")]
        let (shell, shell_arg) = ("cmd", "/C");
        #[cfg(not(target_os = "windows"))]
        let (shell, shell_arg) = ("sh", "-c");

        // ── Spawn child process ───────────────────────────────────
        let child = Command::new(shell)
            .arg(shell_arg)
            .arg(command)
            .current_dir(&self.workspace_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ToolError::Execution(format!("Failed to spawn command: {e}")))?;

        let pid = child.id();

        // ── Watchdog thread ──────────────────────────────────────
        // On timeout, kills the process. Sleeps in a separate OS
        // thread so it doesn't block the caller or the tokio runtime.
        let timeout = Duration::from_secs(timeout_secs);
        let watchdog = thread::spawn(move || {
            thread::sleep(timeout);
            // Best-effort kill — the process may have already exited.
            #[cfg(target_os = "windows")]
            {
                let _ = Command::new("taskkill")
                    .args(["/F", "/PID", &pid.to_string()])
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

        // Join the watchdog (no-op if process exited before timeout)
        let _ = watchdog.join();

        // ── Build result ─────────────────────────────────────────
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
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

        Ok(result)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool() -> ShellTool {
        ShellTool::new(std::env::current_dir().unwrap(), Duration::from_secs(30))
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
    }

    // ── Execution ─────────────────────────────────────────────────

    #[test]
    fn test_execute_echo() {
        let tool = make_tool();
        let result = tool
            .execute(r#"{"command": "echo hello"}"#)
            .expect("echo should succeed");
        assert!(result.contains("hello"), "got: {result}");
    }

    #[test]
    fn test_execute_pwd() {
        let tool = make_tool();
        let result = tool
            .execute(r#"{"command": "echo %cd%"}"#)
            .expect("cmd echo cd should succeed");
        // On Windows cmd, %cd% prints the current directory
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

        let result = tool
            .execute(cmd)
            .expect("should not error on non-zero exit");
        // Should mention the exit code
        assert!(
            result.contains("exit code") || result.contains("42"),
            "got: {result}"
        );
    }

    #[test]
    fn test_execute_missing_command() {
        let tool = make_tool();
        let result = tool.execute(r#"{"timeout_secs": 5}"#);
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
        let result = tool.execute(r#"{"command": "   "}"#);
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
        let result = tool.execute("not json");
        match result {
            Err(ToolError::InvalidArgs(msg)) => {
                assert!(msg.contains("JSON"), "got: {msg}");
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

        let result = tool.execute(cmd).expect("should succeed");
        // Should indicate the command ran even though there's no output
        assert!(
            result.contains("no output") || result.is_empty(),
            "got: {result}"
        );
    }

    #[test]
    fn test_execute_with_timeout_in_args() {
        let tool = make_tool();
        let result = tool
            .execute(r#"{"command": "echo fast", "timeout_secs": 10}"#)
            .expect("should succeed");
        assert!(result.contains("fast"), "got: {result}");
    }

    #[test]
    fn test_execute_stderr_captured() {
        let tool = make_tool();
        // Print to stderr
        #[cfg(target_os = "windows")]
        let cmd = r#"{"command": "cmd /C echo error text >&2"}"#;
        #[cfg(not(target_os = "windows"))]
        let cmd = r#"{"command": "echo error text >&2"}"#;

        let result = tool.execute(cmd).expect("should succeed");
        assert!(result.contains("error text"), "got: {result}");
    }
}
