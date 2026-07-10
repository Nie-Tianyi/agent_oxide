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

use std::sync::{Arc, Mutex, OnceLock};

use engine::{AgentError, AgentEvent, AgentHook, InterveneRequest, InterveneResponse};
use provider::ToolCall;
use tokio::sync::mpsc;

use crate::sandbox::audit_logger::{AuditEntry, AuditLogger};
use crate::sandbox::resource_tracker::ResourceTracker;
use crate::sandbox::shell_filter::{CommandVerdict, ShellFilter};

pub struct SandboxHook {
    /// Sends agent events to the TUI (intervention requests, etc.).
    agent_tx: OnceLock<mpsc::UnboundedSender<AgentEvent>>,
    /// Blocking receive for the user's intervention response
    /// (same rendez-vous pattern as the original hook).
    intervene_rx: Mutex<std::sync::mpsc::Receiver<InterveneResponse>>,
    /// Compiled command policy.
    shell_filter: ShellFilter,
    /// Per-session quota tracker.
    resource_tracker: Arc<ResourceTracker>,
    /// Append-only audit log.
    audit_logger: Arc<AuditLogger>,
}

impl SandboxHook {
    /// Creates the hook and returns the sender half through which the
    /// agent handler signals the user's intervention response.
    pub fn new(
        shell_filter: ShellFilter,
        resource_tracker: Arc<ResourceTracker>,
        audit_logger: Arc<AuditLogger>,
    ) -> (Self, std::sync::mpsc::SyncSender<InterveneResponse>) {
        let (tx, rx) = std::sync::mpsc::sync_channel::<InterveneResponse>(0);
        (
            Self {
                agent_tx: OnceLock::new(),
                intervene_rx: Mutex::new(rx),
                shell_filter,
                resource_tracker,
                audit_logger,
            },
            tx,
        )
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
        let request_id = uuid_v4();

        // Notify the TUI to render an interactive intervention prompt.
        if let Some(tx) = self.agent_tx.get() {
            let _ = tx.send(AgentEvent::NeedUserIntervene(InterveneRequest {
                request_id: request_id.clone(),
                title: "Approve shell command?".into(),
                description: command.to_string(),
                options: vec!["Approve".into(), "Deny".into(), "Other…".into()],
            }));
        }

        // Block until the TUI responds.
        let response = self.intervene_rx.lock().unwrap().recv().unwrap_or({
            // Channel closed — treat as deny.
            InterveneResponse {
                chosen: Some(1), // "Deny"
                custom_text: None,
            }
        });

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
                    timestamp: chrono_now(),
                    session_id: session_id.to_string(),
                    tool: "shell".into(),
                    command: command.clone(),
                    verdict: "blocked".into(),
                    outcome: reason.clone(),
                });
                Err(AgentError::ToolRejected {
                    name: "shell".into(),
                    reason: format!("Blocked by sandbox: {reason}"),
                })
            }

            CommandVerdict::AutoApproved => {
                // Log and allow — no prompt.
                self.audit_logger.log(AuditEntry {
                    timestamp: chrono_now(),
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
                            timestamp: chrono_now(),
                            session_id: session_id.to_string(),
                            tool: "shell".into(),
                            command,
                            verdict: "user_approved".into(),
                            outcome: "allowed".into(),
                        });
                        Ok(())
                    }
                    Err(e) => {
                        self.audit_logger.log(AuditEntry {
                            timestamp: chrono_now(),
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
                timestamp: chrono_now(),
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
}

/// Returns an ISO-8601 timestamp string for the current UTC time.
fn chrono_now() -> String {
    // Avoid adding a chrono dependency — use std only.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Manual ISO-8601 formatting: YYYY-MM-DDTHH:MM:SSZ
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Compute year/month/day from days since Unix epoch.
    // This is correct for all dates from 1970 to 2100.
    let mut year = 1970i64;
    let mut remaining = days_since_epoch as i64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }
    let month_lengths = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1usize;
    for &ml in &month_lengths {
        if remaining < ml {
            break;
        }
        remaining -= ml;
        month += 1;
    }
    let day = remaining + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Generate a simple UUID v4-like string for request identification.
fn uuid_v4() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Simple pseudo-UUID — sufficient for unique request ids within a session.
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (now >> 64) as u32,
        (now >> 48) as u16,
        (now >> 36) as u16 & 0x0fff,
        (0x8000 | (now as u16 & 0x3fff)),
        now as u64 & 0xffffffffffff,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chrono_now_produces_iso8601() {
        let ts = chrono_now();
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
