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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::engine::intervention::{self, InterventionError};
use crate::engine::{AgentError, AgentEvent, AgentHook, ResponseRouter, RunOutcome};
use crate::memory::SharedMemory;
use crate::provider::ToolCall;
use tokio::sync::mpsc;

use super::audit_logger::{AuditEntry, AuditLogger};
use super::resource_tracker::{ResourceTracker, ShellSlot};
use super::shell_filter::{CommandVerdict, ShellFilter};

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
    /// Reserved concurrent-shell slots parked in `before_tool_call`,
    /// keyed by tool-call id.  Released by exactly one terminal callback
    /// — `after_tool_call` / `on_tool_failed` (commit) or
    /// `on_tool_rejected` / run start / run finish (cancel) — so the
    /// quota counter converges even when another hook rejects the call
    /// or the tool task panics.
    pending_slots: Mutex<HashMap<String, ShellSlot>>,
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
            pending_slots: Mutex::new(HashMap::new()),
        }
    }

    /// Called by `build_coding_agent` after the agent-event channel
    /// is created.
    pub fn set_agent_tx(&self, tx: mpsc::UnboundedSender<AgentEvent>) {
        let _ = self.agent_tx.set(tx);
    }

    /// Prompt the user and block until they respond.
    ///
    /// Options: `Approve` / `Deny` / `Deny with reason…`.  The third
    /// option's free-form text is the user's denial reason — it is
    /// recorded in the audit trail and returned to the model, and is
    /// **never** treated as approval.  (The tool layer cannot substitute
    /// a command typed here for the original tool call, so the only
    /// fail-closed interpretation of custom input is a denial.)
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
            vec!["Approve".into(), "Deny".into(), "Deny with reason…".into()],
            Duration::from_secs(120),
        );

        match response {
            Ok(resp) => match resp.chosen {
                Some(0) => Ok(()), // "Approve"
                Some(2) => {
                    // "Deny with reason…" — the custom text is the
                    // reason, capped so a huge paste cannot flood the
                    // context or the audit log.
                    let text = resp.custom_text.as_deref().unwrap_or("");
                    let reason = text.trim();
                    let reason = if reason.is_empty() {
                        "User denied shell command execution".to_string()
                    } else {
                        // floor_char_boundary panics when max > len.
                        let boundary =
                            crate::util::floor_char_boundary(reason, reason.len().min(200));
                        format!(
                            "User denied shell command execution: {}",
                            &reason[..boundary]
                        )
                    };
                    Err(AgentError::ToolRejected {
                        name: "shell".into(),
                        reason,
                    })
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
        // Shell calls reserve a concurrent-shell slot (RAII guard)
        // instead of the bare check() — the slot is parked keyed by
        // tool-call id and released by a terminal callback no matter
        // how the call ends.  Non-shell tools only check the total
        // operations quota.
        if tool_call.function.name == "shell" {
            match self.resource_tracker.acquire_shell_slot(session_id) {
                Ok(slot) => {
                    let mut slots = self.pending_slots.lock().unwrap_or_else(|e| e.into_inner());
                    slots.insert(tool_call.id.clone(), slot);
                }
                Err(reason) => {
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
            }
        } else if let Err(reason) = self
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
                    timestamp: crate::util::iso8601_now(),
                    session_id: session_id.to_string(),
                    tool: "shell".into(),
                    command: command.clone(),
                    verdict: "blocked".into(),
                    outcome: reason.clone(),
                });
                // Release the reserved slot — dropping it cancels the
                // reservation made at the top of before_tool_call.
                if let Ok(mut slots) = self.pending_slots.lock() {
                    slots.remove(&tool_call.id);
                }
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
                    timestamp: crate::util::iso8601_now(),
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
                            timestamp: crate::util::iso8601_now(),
                            session_id: session_id.to_string(),
                            tool: "shell".into(),
                            command,
                            verdict: "user_approved".into(),
                            outcome: "allowed".into(),
                        });
                        Ok(())
                    }
                    Err(e) => {
                        // Release the reserved slot — dropping it cancels
                        // the reservation made at the top of
                        // before_tool_call.
                        tracing::warn!(
                            session_id,
                            command = %command,
                            error = %e,
                            "Shell command rejected: user denied or approval timed out"
                        );
                        if let Ok(mut slots) = self.pending_slots.lock() {
                            slots.remove(&tool_call.id);
                        }
                        // The denial reason (including a custom reason
                        // typed via "Deny with reason…") belongs in the
                        // audit trail.
                        let reason_text = e.to_string();
                        let boundary = crate::util::floor_char_boundary(
                            &reason_text,
                            reason_text.len().min(200),
                        );
                        self.audit_logger.log(AuditEntry {
                            timestamp: crate::util::iso8601_now(),
                            session_id: session_id.to_string(),
                            tool: "shell".into(),
                            command,
                            verdict: "user_denied".into(),
                            outcome: reason_text[..boundary].to_string(),
                        });
                        Err(e)
                    }
                }
            }
        }
    }

    fn after_tool_call(&self, session_id: &str, tool_call: &ToolCall, observation: &str) {
        // Release the reserved slot / record the completed operation.
        // Shell calls commit their slot; non-shell tools just record.
        if tool_call.function.name == "shell" {
            if let Ok(mut slots) = self.pending_slots.lock()
                && let Some(slot) = slots.remove(&tool_call.id)
            {
                slot.commit();
            } else {
                tracing::warn!(
                    tool_call_id = %tool_call.id,
                    "shell completed without a pending slot"
                );
            }
        } else {
            self.resource_tracker
                .record(session_id, &tool_call.function.name);
        }
        // Also log non-shell operations so the audit trail is complete.
        // (Shell operations are already logged inline in before_tool_call.)
        if tool_call.function.name != "shell" {
            self.audit_logger.log(AuditEntry {
                timestamp: crate::util::iso8601_now(),
                session_id: session_id.to_string(),
                tool: tool_call.function.name.clone(),
                command: tool_call.function.arguments.clone(),
                verdict: "allowed".into(),
                outcome: if observation.len() > 100 {
                    let boundary = crate::util::floor_char_boundary(observation, 100);
                    format!("{}...", &observation[..boundary])
                } else {
                    observation.to_string()
                },
            });
        }
    }

    fn on_run_start(&self, _session_id: &str, _user_input: &str, _memory: &SharedMemory) {
        // A previous run may have been cancelled — cancellation aborts
        // the agent task and bypasses hooks, so any slots it reserved
        // are still parked here.  Release them so the quota counters
        // converge before the new run starts.
        if let Ok(mut slots) = self.pending_slots.lock()
            && !slots.is_empty()
        {
            tracing::debug!(
                count = slots.len(),
                "releasing stale shell slots from a previous run"
            );
            slots.clear();
        }
    }

    fn on_run_finish(&self, session_id: &str, outcome: &RunOutcome, _memory: &SharedMemory) {
        // Backstop: release any slots that never reached a terminal
        // callback (e.g. a tool task panicked mid-execution, or an
        // external hook rejected the call before our notification ran).
        if let Ok(mut slots) = self.pending_slots.lock()
            && !slots.is_empty()
        {
            tracing::warn!(
                count = slots.len(),
                "releasing uncommitted shell slots at run finish"
            );
            slots.clear();
        }
        self.resource_tracker.finish_session(session_id);

        let verdict = match outcome {
            RunOutcome::Success { .. } => "success",
            RunOutcome::Error { .. } => "error",
            RunOutcome::Cancelled => "cancelled",
        };
        self.audit_logger.log(AuditEntry {
            timestamp: crate::util::iso8601_now(),
            session_id: session_id.to_string(),
            tool: "__run_finish__".into(),
            command: String::new(),
            verdict: verdict.into(),
            outcome: format!("run outcome: {verdict}"),
        });
    }

    fn on_tool_rejected(&self, session_id: &str, tool_call: &ToolCall, error: &str) {
        // A later hook rejected this call after our before_tool_call
        // reserved a slot.  Dropping the slot cancels the reservation —
        // without this, two rejections would permanently exhaust
        // max_concurrent_shells for the session.
        if let Ok(mut slots) = self.pending_slots.lock()
            && let Some(slot) = slots.remove(&tool_call.id)
        {
            drop(slot);
        }
        // Record the rejection in the audit trail (the rejecting hook
        // audits its own decision; this entry records that the sandbox
        // released its reservation).
        let boundary = crate::util::floor_char_boundary(error, error.len().min(200));
        self.audit_logger.log(AuditEntry {
            timestamp: crate::util::iso8601_now(),
            session_id: session_id.to_string(),
            tool: tool_call.function.name.clone(),
            command: tool_call.function.arguments.clone(),
            verdict: "hook_rejected".into(),
            outcome: error[..boundary].to_string(),
        });
    }

    fn on_tool_failed(&self, session_id: &str, tool_call: &ToolCall, error: &str) {
        // Release the reserved slot / record the failed operation.
        if tool_call.function.name == "shell" {
            if let Ok(mut slots) = self.pending_slots.lock()
                && let Some(slot) = slots.remove(&tool_call.id)
            {
                slot.commit();
            }
        } else {
            self.resource_tracker
                .record(session_id, &tool_call.function.name);
        }
        self.audit_logger.log(AuditEntry {
            timestamp: crate::util::iso8601_now(),
            session_id: session_id.to_string(),
            tool: tool_call.function.name.clone(),
            command: tool_call.function.arguments.clone(),
            verdict: "tool_failed".into(),
            outcome: if error.len() > 100 {
                let boundary = crate::util::floor_char_boundary(error, 100);
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
    use crate::engine::InterventionResponse;
    use crate::provider::{ToolCallFunction, ToolCallKind};
    use crate::sandbox::SandboxConfig;

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

    // ── Approval prompt plumbing (end-to-end without a TUI) ──────────
    //
    // A responder thread plays the TUI: it receives the
    // InterventionRequired event, answers through the ResponseRouter,
    // and the hook's rendezvous channel unblocks.  Exercises the real
    // intervention path, not a mocked one.

    /// Wire a fresh hook + event channel + router, answer the prompt
    /// with `response`, and return the approval outcome.
    fn run_approval_case(response: InterventionResponse) -> Result<(), AgentError> {
        let tmp = tempfile::tempdir().unwrap();
        let config = SandboxConfig::default();
        let hook = SandboxHook::new(
            ShellFilter::from_config(&config),
            Arc::new(ResourceTracker::new(&config)),
            Arc::new(AuditLogger::new(&config, tmp.path())),
            Arc::new(ResponseRouter::new()),
        );
        let router = Arc::clone(&hook.response_router);
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        hook.set_agent_tx(tx);

        let responder = std::thread::spawn(move || {
            let ev = rx
                .blocking_recv()
                .expect("expected InterventionRequired event");
            let AgentEvent::InterventionRequired(req) = ev else {
                panic!("expected InterventionRequired, got {ev:?}");
            };
            assert_eq!(
                req.options,
                ["Approve", "Deny", "Deny with reason…"],
                "option labels drive the TUI rendering"
            );
            assert!(
                router.route(&req.request_id, response),
                "router must deliver the response"
            );
        });

        let tool_call = ToolCall {
            index: 0,
            id: "call_1".into(),
            kind: ToolCallKind::Function,
            function: ToolCallFunction {
                name: "shell".into(),
                arguments: r#"{"command": "echo hi"}"#.into(),
            },
        };
        let result = hook.request_user_approval(&tool_call, "echo hi");
        responder.join().unwrap();
        result
    }

    #[test]
    fn test_approve_option_approves() {
        assert!(
            run_approval_case(InterventionResponse {
                chosen: Some(0),
                custom_text: None,
            })
            .is_ok()
        );
    }

    #[test]
    fn test_deny_option_rejects() {
        let err = run_approval_case(InterventionResponse {
            chosen: Some(1),
            custom_text: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("denied"), "got: {err}");
    }

    #[test]
    fn test_deny_with_reason_rejects_and_keeps_text() {
        let err = run_approval_case(InterventionResponse {
            chosen: Some(2),
            custom_text: Some("use pip install instead".into()),
        })
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("denied"), "got: {msg}");
        assert!(
            msg.contains("use pip install instead"),
            "custom denial reason must reach the model: {msg}"
        );
    }

    #[test]
    fn test_deny_with_empty_reason_still_rejects() {
        // Empty custom text must not be mistaken for approval.
        let err = run_approval_case(InterventionResponse {
            chosen: Some(2),
            custom_text: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("denied"), "got: {err}");
    }

    #[test]
    fn test_dismissed_prompt_rejects() {
        let err = run_approval_case(InterventionResponse {
            chosen: None,
            custom_text: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("denied"), "got: {err}");
    }

    // ── Slot lifecycle pairing ─────────────────────────────────────

    /// Hook wired with a 1-slot shell quota — a leaked reservation
    /// makes the very next shell call fail with "too many concurrent
    /// shells" (the pre-fix failure mode).
    fn make_tight_hook(tmp: &tempfile::TempDir) -> SandboxHook {
        let mut config = SandboxConfig::default();
        config.quotas.max_concurrent_shells = 1;
        SandboxHook::new(
            ShellFilter::from_config(&config),
            Arc::new(ResourceTracker::new(&config)),
            Arc::new(AuditLogger::new(&config, tmp.path())),
            Arc::new(ResponseRouter::new()),
        )
    }

    fn shell_tool_call(id: &str) -> ToolCall {
        ToolCall {
            index: 0,
            id: id.into(),
            kind: ToolCallKind::Function,
            function: ToolCallFunction {
                name: "shell".into(),
                arguments: r#"{"command": "echo hi"}"#.into(),
            },
        }
    }

    fn make_memory() -> SharedMemory {
        Arc::new(std::sync::RwLock::new(crate::memory::Memory::new()))
    }

    #[test]
    fn test_rejection_by_later_hook_releases_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_tight_hook(&tmp);

        // The shell passes the sandbox checks (echo is auto-approved)
        // and reserves the only slot.
        hook.before_tool_call("s", &shell_tool_call("call_1"))
            .unwrap();

        // A later hook rejects the call — the engine notifies us via
        // on_tool_rejected.  Without the slot release, every subsequent
        // shell call in this session would fail with "too many
        // concurrent shells" forever (the pre-fix bug).
        hook.on_tool_rejected("s", &shell_tool_call("call_1"), "blocked by policy hook");

        // The slot is free again.
        hook.before_tool_call("s", &shell_tool_call("call_2"))
            .unwrap();
        // Run finish releases the second slot and resets the session.
        hook.on_run_finish(
            "s",
            &RunOutcome::Success {
                answer: "done".into(),
            },
            &make_memory(),
        );
        hook.before_tool_call("s", &shell_tool_call("call_3"))
            .unwrap();
    }

    #[test]
    fn test_own_denial_releases_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_tight_hook(&tmp);

        // `curl` requires approval; without an agent channel the prompt
        // fails → denial path → the reserved slot must be released.
        let tc = ToolCall {
            index: 0,
            id: "call_1".into(),
            kind: ToolCallKind::Function,
            function: ToolCallFunction {
                name: "shell".into(),
                arguments: r#"{"command": "curl example.com"}"#.into(),
            },
        };
        assert!(hook.before_tool_call("s", &tc).is_err());

        // Slot free again — a fresh auto-approved shell passes.
        hook.before_tool_call("s", &shell_tool_call("call_2"))
            .unwrap();
    }

    #[test]
    fn test_run_finish_drains_stale_slots() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_tight_hook(&tmp);

        // Simulate the panic path: the slot is parked but no terminal
        // callback ever fires.
        hook.before_tool_call("s", &shell_tool_call("call_1"))
            .unwrap();

        // Run finish is the backstop.
        hook.on_run_finish(
            "s",
            &RunOutcome::Error {
                error: "boom".into(),
            },
            &make_memory(),
        );

        // A new run starts with a clean quota.
        hook.before_tool_call("s", &shell_tool_call("call_2"))
            .unwrap();
    }
}
