//! [`ShellTool`] — the in-library `shell` tool.
//!
//! Completes the sandbox story inside the crate: the policy layers
//! ([`ShellFilter`] via [`SandboxHook`]) and the execution layers
//! ([`ShellRunner`] — env sanitization, watchdog kill, bounded capture,
//! decoding/truncation) are now both shipped and wired here.  Downstream
//! crates register this tool instead of hand-wiring layers 4–5:
//!
//! ```ignore
//! let agent = Agent::builder(client, model)
//!     .tool(ShellTool::from_config(workspace_root, &sandbox_config))
//!     .hook(SandboxHook::new(...))
//!     .build();
//! ```

use std::path::PathBuf;
use std::thread;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::tools::{Progress, ProgressStream, Tool, ToolError};

use super::config::SandboxConfig;
use super::shell_filter::{CommandVerdict, ShellFilter};
use super::shell_runner::{ShellOutput, ShellRunner};

/// How the second-pass [`ShellFilter`] check treats commands the hook
/// layer would have prompted for.
///
/// The tool layer cannot tell "the hook just approved this" from "the
/// hook was never run".  [`BlockOnly`](ToolApprovalMode::BlockOnly)
/// keeps the default (hook + tool) wiring working; `DenyUnapproved`
/// closes the gap when `ShellTool` is used **without**
/// [`SandboxHook`](crate::sandbox::SandboxHook) in the hook chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolApprovalMode {
    /// Block only `Blocked` verdicts — the default, and the only mode
    /// compatible with `SandboxHook` in the hook chain (the hook already
    /// prompted the user for `RequiresApproval` commands).
    #[default]
    BlockOnly,
    /// Also refuse `RequiresApproval` commands.  **Required** for
    /// deployments that register `ShellTool` without `SandboxHook` —
    /// otherwise the approval layer is silently bypassed.
    DenyUnapproved,
}

/// The `shell` tool — policy check (second-pass classify) + sandboxed
/// execution ([`ShellRunner`]) with live progress events.
pub struct ShellTool {
    runner: ShellRunner,
    filter: ShellFilter,
    mode: ToolApprovalMode,
}

impl ShellTool {
    /// Convenience constructor — builds the filter and runner from one
    /// [`SandboxConfig`].  Uses [`ToolApprovalMode::BlockOnly`]; pass
    /// [`new`](Self::new) to change the mode.
    pub fn from_config(workspace_root: PathBuf, config: &SandboxConfig) -> Self {
        Self {
            runner: ShellRunner::new(workspace_root, config.shell.clone()),
            filter: ShellFilter::from_config(config),
            mode: ToolApprovalMode::BlockOnly,
        }
    }

    pub fn new(runner: ShellRunner, filter: ShellFilter, mode: ToolApprovalMode) -> Self {
        Self {
            runner,
            filter,
            mode,
        }
    }
}

/// Strict argument parsing — unlike the hook's lenient
/// [`SandboxHook::parse_command`] (which renders garbage for display),
/// the tool must fail closed on malformed input.
fn parse_command_strict(args: &str) -> Result<(String, Option<u64>), ToolError> {
    let value: Value = serde_json::from_str(args)
        .map_err(|e| ToolError::InvalidArgs(format!("expected a JSON object: {e}")))?;
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArgs("missing required string field 'command'".into()))?;
    if command.trim().is_empty() {
        return Err(ToolError::InvalidArgs("'command' is empty".into()));
    }
    let timeout_secs = value.get("timeout_secs").and_then(Value::as_u64);
    Ok((command.to_string(), timeout_secs))
}

/// Render a [`ShellOutput`] as the observation text returned to the LLM.
fn render_output(output: &ShellOutput) -> String {
    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&output.stdout);
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    if !output.stderr.is_empty() {
        text.push_str("[stderr]\n");
        text.push_str(&output.stderr);
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    let code = match output.exit_code {
        Some(c) => c.to_string(),
        None => "killed by signal".into(),
    };
    let mut meta = format!("[exit code: {code}]");
    if output.timed_out {
        meta.push_str(" [timed out — killed by watchdog]");
    }
    if output.truncated {
        meta.push_str(" [output truncated]");
    }
    text.push_str(&meta);
    text
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Run a shell command in the workspace. Output is size-capped and the \
         process is killed on timeout; pass 'timeout_secs' for long-running \
         commands (bounded by the sandbox config)."
    }

    fn parameter_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute in the workspace"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Optional timeout in seconds; defaults to the sandbox config value"
                }
            },
            "required": ["command"]
        })
    }

    fn execute_stream(&self, args: &str) -> Result<ProgressStream, ToolError> {
        let (command, timeout_secs) = parse_command_strict(args)?;

        // ── Second-pass classification (sandbox check #13). ──
        // The hook layer already gatekept this call; this pass is the
        // backstop for deployments where the hook chain was bypassed.
        match self.filter.classify(&command) {
            CommandVerdict::Blocked { reason } => {
                tracing::warn!(
                    command = %command,
                    reason = %reason,
                    "Shell command blocked by second-pass filter"
                );
                return Err(ToolError::Execution(reason));
            }
            CommandVerdict::RequiresApproval if self.mode == ToolApprovalMode::DenyUnapproved => {
                tracing::warn!(
                    command = %command,
                    "Shell command requires approval — DenyUnapproved mode refuses"
                );
                return Err(ToolError::Execution(
                    "command requires user approval (ShellTool running without SandboxHook)".into(),
                ));
            }
            _ => {}
        }

        // ── Run on a background thread, stream progress over a channel ──
        // (same pattern as SubagentTool).  The tokio worker driving the
        // tool loop stays free, and InProgress events reach the TUI live.
        //
        // If the stream is dropped (cancellation), the thread keeps
        // running until the command finishes or the watchdog kills it —
        // bounded by the sandbox timeout.
        let (progress_tx, progress_rx) = mpsc::unbounded_channel::<Progress>();
        let runner = self.runner.clone();
        let command_for_thread = command.clone();
        thread::spawn(move || {
            let _ = progress_tx.send(Progress::InProgress(format!(
                "running: {command_for_thread}"
            )));
            match runner.run(&command_for_thread, timeout_secs) {
                Ok(output) => {
                    if output.timed_out {
                        let _ = progress_tx.send(Progress::InProgress(
                            "timed out — killed by watchdog".into(),
                        ));
                    }
                    let _ = progress_tx.send(Progress::Done(render_output(&output)));
                }
                Err(e) => {
                    let _ =
                        progress_tx.send(Progress::Done(format!("shell execution failed: {e}")));
                }
            }
        });

        let stream = futures_util::stream::unfold(progress_rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(ProgressStream::new(Box::pin(stream)))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    /// Test config with tight limits so timeout tests run fast.
    fn test_config() -> SandboxConfig {
        let mut cfg = SandboxConfig::default();
        cfg.shell.default_timeout_secs = 2;
        cfg.shell.max_timeout_secs = 5;
        cfg.shell.max_output_bytes = 256;
        cfg
    }

    fn make_tool(tmp: &tempfile::TempDir, mode: ToolApprovalMode) -> ShellTool {
        let cfg = test_config();
        let runner = ShellRunner::new(tmp.path().to_path_buf(), cfg.shell.clone());
        let filter = ShellFilter::from_config(&cfg);
        ShellTool::new(runner, filter, mode)
    }

    /// Drive a ProgressStream to its final `Done` event, skipping
    /// InProgress updates (which `poll_done` would reject).
    fn drive_to_done(stream: &mut ProgressStream) -> String {
        futures_executor::block_on(async {
            let mut last = None;
            while let Some(ev) = stream.next().await {
                if let Progress::Done(output) = ev {
                    last = Some(output);
                }
            }
            last.expect("stream must end with Progress::Done")
        })
    }

    #[test]
    fn schema_requires_command_and_offers_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp, ToolApprovalMode::BlockOnly);
        let schema = tool.parameter_schema();
        assert_eq!(schema["required"][0], "command");
        assert!(schema["properties"]["timeout_secs"].is_object());
    }

    #[test]
    fn malformed_args_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp, ToolApprovalMode::BlockOnly);
        assert!(matches!(
            tool.execute_stream("not json"),
            Err(ToolError::InvalidArgs(_))
        ));
        assert!(matches!(
            tool.execute_stream(r#"{"other": 1}"#),
            Err(ToolError::InvalidArgs(_))
        ));
        assert!(matches!(
            tool.execute_stream(r#"{"command": "   "}"#),
            Err(ToolError::InvalidArgs(_))
        ));
    }

    #[test]
    fn blocked_command_is_rejected_by_second_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp, ToolApprovalMode::BlockOnly);
        // The default deny patterns block `rm -rf /` — the second-pass
        // check (sandbox check #13) must catch it even without a hook.
        let err = tool
            .execute_stream(r#"{"command": "rm -rf /"}"#)
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "got: {err:?}");
    }

    #[test]
    fn deny_unapproved_mode_refuses_prompts() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp, ToolApprovalMode::DenyUnapproved);
        // `curl` is not in the auto-approve list → RequiresApproval.
        let err = tool
            .execute_stream(r#"{"command": "curl example.com"}"#)
            .unwrap_err();
        assert!(matches!(err, ToolError::Execution(_)), "got: {err:?}");
    }

    #[test]
    fn block_only_mode_allows_unapproved_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp, ToolApprovalMode::BlockOnly);
        // A nonexistent binary is RequiresApproval but harmless to run —
        // BlockOnly lets it through to the runner.
        let mut stream = tool
            .execute_stream(r#"{"command": "definitely_not_a_real_cmd_xyz"}"#)
            .unwrap();
        let done = drive_to_done(&mut stream);
        assert!(
            done.contains("exit code"),
            "observation must carry the exit code: {done}"
        );
    }

    #[test]
    fn echo_produces_done_with_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp, ToolApprovalMode::BlockOnly);
        let mut stream = tool.execute_stream(r#"{"command": "echo hello"}"#).unwrap();
        let done = drive_to_done(&mut stream);
        assert!(done.contains("hello"), "got: {done}");
        assert!(done.contains("[exit code: 0]"), "got: {done}");
    }

    #[test]
    fn timeout_marks_observation() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = make_tool(&tmp, ToolApprovalMode::BlockOnly);
        #[cfg(target_os = "windows")]
        let command = r#"{"command": "ping -n 5 127.0.0.1", "timeout_secs": 1}"#;
        #[cfg(not(target_os = "windows"))]
        let command = r#"{"command": "sleep 5", "timeout_secs": 1}"#;

        let mut stream = tool.execute_stream(command).unwrap();
        let done = drive_to_done(&mut stream);
        assert!(
            done.contains("timed out"),
            "observation must report the timeout: {done}"
        );
    }
}
