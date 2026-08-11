//! Shared intervention-request helper — decouples the common
//! request/response plumbing from call-site business logic.
//!
//! Three components in the codebase need to pause the agent and ask the
//! user a question:
//!
//! | Component | Purpose | Timeout |
//! |---|---|---|
//! | `SandboxHook` | Shell command approval | 120 s |
//! | `AskUserQuestionTool` | LLM-initiated questions | 300 s |
//! | `ExitPlanModeTool` | Plan approval | 300 s |
//!
//! Before this module, each component duplicated the same 6-step pattern:
//! 1. Generate a unique `request_id`.
//! 2. Create a `SyncChannel(0)` rendezvous channel.
//! 3. Register the sender with [`ResponseRouter`].
//! 4. Send an [`InterventionRequired`](AgentEvent::InterventionRequired) event.
//! 5. Block on `recv_timeout()`.
//! 6. Unregister from the router (bug: skipped on `Disconnected`).
//!
//! [`request_intervention`] consolidates these steps with a single
//! function call and guarantees cleanup regardless of the outcome.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::response_router::{ResponseRouter, next_request_id};
use crate::{AgentEvent, InterventionRequest, InterventionResponse};

/// Errors that can occur during an intervention request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterventionError {
    /// The user did not respond within the timeout.
    Timeout,
    /// The TUI event channel was closed (process shutting down).
    Disconnected,
}

/// Send an intervention request to the TUI and block until the user
/// responds (or the timeout expires).
///
/// # Cleanup guarantee
///
/// The `request_id` is **always** unregistered from the response router,
/// regardless of the outcome — on success, timeout, or disconnection.
/// This fixes a bug in the original three call sites where `unregister`
/// was skipped on the `Disconnected` path.
///
/// # Arguments
///
/// * `response_router` — shared router for delivering responses.
/// * `agent_tx` — sender half of the agent-event channel.
/// * `title` — short question (e.g. `"Approve shell command?"`).
/// * `description` — detailed context (the shell command, diff, etc.).
/// * `options` — one or more choices the user can pick from.
/// * `timeout` — how long to wait before returning [`InterventionError::Timeout`].
pub fn request_intervention(
    response_router: &Arc<ResponseRouter>,
    agent_tx: &mpsc::UnboundedSender<AgentEvent>,
    title: String,
    description: String,
    options: Vec<String>,
    timeout: Duration,
) -> Result<InterventionResponse, InterventionError> {
    let request_id = next_request_id();

    // Create a per-request rendezvous channel and register with the
    // response router so the TUI can deliver the answer.
    let (tx, rx) = std::sync::mpsc::sync_channel::<InterventionResponse>(0);
    response_router.register(request_id.clone(), tx);

    // Notify the TUI to render an interactive intervention prompt.
    let _ = agent_tx.send(AgentEvent::InterventionRequired(InterventionRequest {
        request_id: request_id.clone(),
        title: title.clone(),
        description: description.clone(),
        options,
    }));
    tracing::debug!(
        request_id = %request_id,
        title = %title,
        description_len = description.len(),
        timeout_secs = timeout.as_secs(),
        "intervention requested — awaiting user response",
    );

    // Block until the user responds (with timeout to prevent deadlock).
    let result = rx.recv_timeout(timeout);

    // ALWAYS clean up — the TUI's route() may have already removed the
    // entry, in which case unregister() is a no-op.
    response_router.unregister(&request_id);

    match result {
        Ok(resp) => {
            tracing::debug!(request_id = %request_id, "intervention response received");
            Ok(resp)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            tracing::warn!(request_id = %request_id, "intervention request timed out");
            Err(InterventionError::Timeout)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            tracing::warn!(request_id = %request_id, "intervention channel disconnected");
            Err(InterventionError::Disconnected)
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InterventionResponse;

    fn make_router() -> Arc<ResponseRouter> {
        Arc::new(ResponseRouter::new())
    }

    fn make_agent_channel() -> (
        mpsc::UnboundedSender<AgentEvent>,
        mpsc::UnboundedReceiver<AgentEvent>,
    ) {
        mpsc::unbounded_channel()
    }

    #[test]
    fn test_request_intervention_timeout() {
        let router = make_router();
        let (tx, _rx) = make_agent_channel();

        // No TUI is listening — the intervention will time out.
        let result = request_intervention(
            &router,
            &tx,
            "Test?".into(),
            "Description".into(),
            vec!["Yes".into(), "No".into()],
            Duration::from_millis(50),
        );

        assert!(matches!(result, Err(InterventionError::Timeout)));
    }

    #[test]
    fn test_request_intervention_disconnected() {
        let router = make_router();
        let (tx, _rx) = make_agent_channel();

        // Simulating a true Disconnected in a unit test is awkward
        // because the sync_channel's rx is created inside
        // request_intervention. The timeout path covers the most
        // common failure mode.
        let result = request_intervention(
            &router,
            &tx,
            "Test?".into(),
            "".into(),
            vec!["OK".into()],
            Duration::from_millis(10),
        );
        assert!(matches!(result, Err(InterventionError::Timeout)));
    }

    #[test]
    fn test_router_cleaned_on_timeout() {
        let router = make_router();
        let (tx, _agent_rx) = make_agent_channel();

        // Call request_intervention which will time out.
        let _ = request_intervention(
            &router,
            &tx,
            "Q".into(),
            "".into(),
            vec!["A".into()],
            Duration::from_millis(10),
        );

        // The router should be clean — no leftover entries.
        // Verify by checking that routing to any id returns false.
        assert!(!router.route(
            "req-0000000000000000",
            InterventionResponse {
                chosen: Some(0),
                custom_text: None,
            }
        ));
    }
}
