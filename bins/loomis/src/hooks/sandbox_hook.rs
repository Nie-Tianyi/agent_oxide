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
//! handled by `WorkspaceFs` in the tool implementation).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use engine::{
    AgentError, AgentEvent, AgentHook, InterventionRequest, InterventionResponse, RunOutcome,
};
use provider::ToolCall;
use tokio::sync::mpsc;

use crate::hooks::{ResponseRouter, next_request_id};
use crate::sandbox::audit_logger::{AuditEntry, AuditLogger};
use crate::sandbox::resource_tracker::ResourceTracker;
use crate::sandbox::shell_filter::{CommandVerdict, ShellFilter};

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
        let request_id = next_request_id();

        // Create per-request rendezvous channel and register with the
        // response router so the TUI can deliver the answer.
        let (tx, rx) = std::sync::mpsc::sync_channel::<InterventionResponse>(0);
        self.response_router.register(request_id.clone(), tx);

        // Notify the TUI to render an interactive intervention prompt.
        if let Some(agent_tx) = self.agent_tx.get() {
            let _ = agent_tx.send(AgentEvent::InterventionRequired(InterventionRequest {
                request_id: request_id.clone(),
                title: "Approve shell command?".into(),
                description: command.to_string(),
                options: vec!["Approve".into(), "Deny".into(), "Other…".into()],
            }));
        }

        // Block until the TUI responds (with timeout to prevent deadlock).
        let response = match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(resp) => resp,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Timeout — treat as deny.
                self.response_router.unregister(&request_id);
                InterventionResponse {
                    chosen: Some(1), // "Deny"
                    custom_text: None,
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Channel closed — treat as deny.
                InterventionResponse {
                    chosen: Some(1), // "Deny"
                    custom_text: None,
                }
            }
        };

        // Cleanup (no-op if the TUI's route() already removed the entry).
        self.response_router.unregister(&request_id);

        match response.chosen {
            Some(0) => Ok(()), // "Approve"
            Some(2) => {
                // "Other…" — user provided custom input; approve with
                // the custom text (which can be logged / used later).
                let _ = response.custom_text;
                Ok(())
            }
            _ => {
                // Deny, cancel, or unknown option.
                Err(AgentError::ToolRejected {
                    name: "shell".into(),
                    reason: "User denied shell command execution".into(),
                })
            }
        }
    }

    /// Extract the command string from shell tool arguments.
    fn parse_command(args: &str) -> String {
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
                self.audit_logger.log(AuditEntry {
                    timestamp: memory::iso8601_now(),
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
                self.audit_logger.log(AuditEntry {
                    timestamp: memory::iso8601_now(),
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
                match self.request_user_approval(tool_call, &command) {
                    Ok(()) => {
                        self.audit_logger.log(AuditEntry {
                            timestamp: memory::iso8601_now(),
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
                        self.resource_tracker.cancel(session_id, "shell");
                        self.audit_logger.log(AuditEntry {
                            timestamp: memory::iso8601_now(),
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
                timestamp: memory::iso8601_now(),
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

    fn on_run_finish(&self, session_id: &str, outcome: &RunOutcome) {
        let verdict = match outcome {
            RunOutcome::Success { .. } => "success",
            RunOutcome::Error { .. } => "error",
            RunOutcome::Cancelled => "cancelled",
        };
        self.audit_logger.log(AuditEntry {
            timestamp: memory::iso8601_now(),
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
            timestamp: memory::iso8601_now(),
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
    #[test]
    fn test_iso8601_now_produces_correct_format() {
        let ts = memory::iso8601_now();
        // Should look like "2026-07-09T12:34:56Z"
        assert!(ts.ends_with('Z'), "got {ts}");
        assert_eq!(ts.len(), 20, "got {ts}");
        assert!(ts.starts_with("20"), "got {ts}");
        let parts: Vec<&str> = ts[..19].split('T').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 10);
        assert_eq!(parts[1].len(), 8);
    }
}
