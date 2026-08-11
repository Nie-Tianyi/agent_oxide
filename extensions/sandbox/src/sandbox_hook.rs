//! [`SandboxHook`] — unified security hook that replaces
//! `DangerousCommandApprovalHook`.
//!
//! # Architecture
//!
//! ```text
//! shell command arrives in before_tool_call
//!   │
//!   ├─ ResourceTracker::check → quota exceeded? → reject
//!   ├─ ShellFilter::classify
//!   │   ├─ Blocked  → reject immediately (no prompt)
//!   │   ├─ AutoApproved → allow (no prompt)
//!   │   └─ RequiresApproval → TUI prompt (navigable options)
//!   └─ AuditLogger records every decision
//! ```
//!
//! Non-shell tools pass through without checks (their sandboxing is
//! handled by [`WorkspaceFs`]).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use engine::intervention::{self, InterventionError};
use engine::{AgentError, AgentEvent, AgentHook, ResponseRouter, RunOutcome};
use memory::SharedMemory;
use provider::ToolCall;
use tokio::sync::mpsc;

use crate::audit_logger::{AuditEntry, AuditLogger};
use crate::resource_tracker::ResourceTracker;
use crate::shell_filter::{CommandVerdict, ShellFilter};

pub struct SandboxHook {
    /// Sends agent events to the TUI (intervention requests, etc.).
    agent_tx: OnceLock<mpsc::UnboundedSender<AgentEvent>>,
    /// Shared router for delivering intervention responses to the
    /// correct requester (SandboxHook, AskUserQuestionTool, …).
    response_router: Arc<ResponseRouter>,
    /// Compiled command policy.
    shell_filter: ShellFilter,
    /// Per-session quota tracker.
    resource_tracker: Arc<ResourceTracker>,
    /// Append-only audit log.
    audit_logger: Arc<AuditLogger>,
}

impl SandboxHook {
    /// Creates the hook, sharing the given response router for
    /// intervention prompts.
    pub fn new(
        shell_filter: ShellFilter,
        resource_tracker: Arc<ResourceTracker>,
        audit_logger: Arc<AuditLogger>,
        response_router: Arc<ResponseRouter>,
    ) -> Self {
        Self {
            agent_tx: OnceLock::new(),
            response_router,
            shell_filter,
            resource_tracker,
            audit_logger,
        }
    }

    /// Called by `build_coding_agent` after the agent-event channel
    /// is created.
    pub fn set_agent_tx(&self, tx: mpsc::UnboundedSender<AgentEvent>) {
        let _ = self.agent_tx.set(tx);
    }

    /// Prompt the user and block until they respond.
    fn request_user_approval(
        &self,
        _tool_call: &ToolCall,
        command: &str,
    ) -> Result<(), AgentError> {
        let agent_tx = self
            .agent_tx
            .get()
            .ok_or_else(|| AgentError::ToolRejected {
                name: "shell".into(),
                reason: "Agent event channel not configured".into(),
            })?;

        // Delegate the common request/response plumbing to the shared helper.
        let response = intervention::request_intervention(
            &self.response_router,
            agent_tx,
            "Approve shell command?".into(),
            command.to_string(),
            vec!["Approve".into(), "Deny".into(), "Other…".into()],
            Duration::from_secs(120),
        );

        match response {
            Ok(resp) => match resp.chosen {
                Some(0) => Ok(()), // "Approve"
                Some(2) => {
                    // "Other…" — user provided custom input; approve.
                    let _ = resp.custom_text;
                    Ok(())
                }
                _ => {
                    // Deny, cancel, or unknown.
                    Err(AgentError::ToolRejected {
                        name: "shell".into(),
                        reason: "User denied shell command execution".into(),
                    })
                }
            },
            Err(InterventionError::Timeout) => {
                // Treat timeout as deny.
                Err(AgentError::ToolRejected {
                    name: "shell".into(),
                    reason: "Shell command approval timed out (2 minutes)".into(),
                })
            }
            Err(InterventionError::Disconnected) => Err(AgentError::ToolRejected {
                name: "shell".into(),
                reason: "Intervention channel disconnected (TUI may have exited)".into(),
            }),
        }
    }

    /// Extract the command string from shell tool arguments.
    pub fn parse_command(args: &str) -> String {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(args) {
            v.get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            args.to_string()
        }
    }
}

impl AgentHook for SandboxHook {
    fn before_tool_call(&self, session_id: &str, tool_call: &ToolCall) -> Result<(), AgentError> {
        // ── Resource quota check (all tools) ────────────────────────
        if let Err(reason) = self
            .resource_tracker
            .check(session_id, &tool_call.function.name)
        {
            tracing::warn!(
                session_id,
                tool = %tool_call.function.name,
                reason = %reason,
                "Tool call rejected: resource quota exceeded"
            );
            return Err(AgentError::ToolRejected {
                name: tool_call.function.name.clone(),
                reason,
            });
        }

        // ── Shell-specific checks ────────────────────────────────────
        if tool_call.function.name != "shell" {
            return Ok(());
        }

        let command = Self::parse_command(&tool_call.function.arguments);

        match self.shell_filter.classify(&command) {
            CommandVerdict::Blocked { reason } => {
                // Log the block and reject immediately — no prompt.
                tracing::warn!(
                    session_id,
                    command = %command,
                    reason = %reason,
                    "Shell command blocked by sandbox policy"
                );
                self.audit_logger.log(AuditEntry {
                    timestamp: util::iso8601_now(),
                    session_id: session_id.to_string(),
                    tool: "shell".into(),
                    command: command.clone(),
                    verdict: "blocked".into(),
                    outcome: reason.clone(),
                });
                // Cancel the active_shells increment from check() —
                // the tool was rejected before execution.
                self.resource_tracker.cancel(session_id, "shell");
                Err(AgentError::ToolRejected {
                    name: "shell".into(),
                    reason: format!("Blocked by sandbox: {reason}"),
                })
            }

            CommandVerdict::AutoApproved => {
                // Log and allow — no prompt.
                tracing::debug!(
                    session_id,
                    command = %command,
                    "Shell command auto-approved"
                );
                self.audit_logger.log(AuditEntry {
                    timestamp: util::iso8601_now(),
                    session_id: session_id.to_string(),
                    tool: "shell".into(),
                    command: command.clone(),
                    verdict: "auto_approved".into(),
                    outcome: "allowed".into(),
                });
                Ok(())
            }

            CommandVerdict::RequiresApproval => {
                // Prompt the user.
                tracing::debug!(
                    session_id,
                    command = %command,
                    "Shell command requires user approval — prompting"
                );
                match self.request_user_approval(tool_call, &command) {
                    Ok(()) => {
                        tracing::info!(
                            session_id,
                            command = %command,
                            "Shell command approved by user"
                        );
                        self.audit_logger.log(AuditEntry {
                            timestamp: util::iso8601_now(),
                            session_id: session_id.to_string(),
                            tool: "shell".into(),
                            command,
                            verdict: "user_approved".into(),
                            outcome: "allowed".into(),
                        });
                        Ok(())
                    }
                    Err(e) => {
                        // Cancel the active_shells increment from check() —
                        // the tool was rejected by the user before execution.
                        tracing::warn!(
                            session_id,
                            command = %command,
                            error = %e,
                            "Shell command rejected: user denied or approval timed out"
                        );
                        self.resource_tracker.cancel(session_id, "shell");
                        self.audit_logger.log(AuditEntry {
                            timestamp: util::iso8601_now(),
                            session_id: session_id.to_string(),
                            tool: "shell".into(),
                            command,
                            verdict: "user_denied".into(),
                            outcome: "denied".into(),
                        });
                        Err(e)
                    }
                }
            }
        }
    }

    fn after_tool_call(&self, session_id: &str, tool_call: &ToolCall, observation: &str) {
        // Record the operation in the resource tracker.
        self.resource_tracker
            .record(session_id, &tool_call.function.name);
        // Also log non-shell operations so the audit trail is complete.
        // (Shell operations are already logged inline in before_tool_call.)
        if tool_call.function.name != "shell" {
            self.audit_logger.log(AuditEntry {
                timestamp: util::iso8601_now(),
                session_id: session_id.to_string(),
                tool: tool_call.function.name.clone(),
                command: tool_call.function.arguments.clone(),
                verdict: "allowed".into(),
                outcome: if observation.len() > 100 {
                    let boundary = observation.floor_char_boundary(100);
                    format!("{}...", &observation[..boundary])
                } else {
                    observation.to_string()
                },
            });
        }
    }

    fn on_run_finish(&self, session_id: &str, outcome: &RunOutcome, _memory: &SharedMemory) {
        let verdict = match outcome {
            RunOutcome::Success { .. } => "success",
            RunOutcome::Error { .. } => "error",
            RunOutcome::Cancelled => "cancelled",
        };
        self.audit_logger.log(AuditEntry {
            timestamp: util::iso8601_now(),
            session_id: session_id.to_string(),
            tool: "__run_finish__".into(),
            command: String::new(),
            verdict: verdict.into(),
            outcome: format!("run outcome: {verdict}"),
        });
    }

    fn on_tool_failed(&self, session_id: &str, tool_call: &ToolCall, error: &str) {
        // Record the failure in the resource tracker and audit log.
        self.resource_tracker
            .record(session_id, &tool_call.function.name);
        self.audit_logger.log(AuditEntry {
            timestamp: util::iso8601_now(),
            session_id: session_id.to_string(),
            tool: tool_call.function.name.clone(),
            command: tool_call.function.arguments.clone(),
            verdict: "tool_failed".into(),
            outcome: if error.len() > 100 {
                let boundary = error.floor_char_boundary(100);
                format!("{}...", &error[..boundary])
            } else {
                error.to_string()
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_command_extracts_shell_arg() {
        let cmd = SandboxHook::parse_command(r#"{"command": "echo hello"}"#);
        assert_eq!(cmd, "echo hello");
    }

    #[test]
    fn test_parse_command_empty_args() {
        let cmd = SandboxHook::parse_command("");
        assert_eq!(cmd, "");
    }

    #[test]
    fn test_parse_command_missing_command_field() {
        let cmd = SandboxHook::parse_command(r#"{"other": "value"}"#);
        assert_eq!(cmd, "");
    }

    #[test]
    fn test_parse_command_raw_string_fallback() {
        let cmd = SandboxHook::parse_command("not json at all");
        assert_eq!(cmd, "not json at all");
    }
}
