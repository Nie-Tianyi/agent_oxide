//! [`ResponseRouter`] — maps `request_id` to the correct intervention responder.
//!
//! # Why a router?
//!
//! Multiple components can need user intervention simultaneously:
//!
//! - [`SandboxHook`] — shell command approval in `before_tool_call`.
//! - [`AskUserQuestionTool`] — LLM-initiated questions during tool execution.
//!
//! Each requester creates its own `sync_channel(0)` pair, registers the
//! sender under a unique [`request_id`], sends an
//! [`InterventionRequired`](crate::AgentEvent::InterventionRequired) event,
//! and blocks on its own receiver.  The TUI's agent handler routes responses
//! through this router instead of a single hard-coded channel.
//!
//! # Deadlock safety
//!
//! [`route()`](ResponseRouter::route) removes the sender from the map
//! **under the lock** but calls `send()` **outside the lock**.  This avoids
//! holding the mutex while blocked on a rendezvous channel (capacity 0).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::InterventionResponse;

/// Routes [`InterventionResponse`] values to the correct blocking requester.
///
/// Each requester registers a [`SyncSender`](std::sync::mpsc::SyncSender)
/// under its unique `request_id`.  When the TUI sends back a response,
/// [`route()`](Self::route) looks up the sender and delivers the response.
pub struct ResponseRouter {
    senders: Mutex<HashMap<String, std::sync::mpsc::SyncSender<InterventionResponse>>>,
}

impl ResponseRouter {
    /// Creates an empty router.
    pub fn new() -> Self {
        Self {
            senders: Mutex::new(HashMap::new()),
        }
    }

    /// Register a sender for the given `request_id`.
    ///
    /// Panics if a sender is already registered under this id (each
    /// `request_id` must be unique within a session).
    pub fn register(
        &self,
        request_id: String,
        tx: std::sync::mpsc::SyncSender<InterventionResponse>,
    ) {
        let mut senders = self.senders.lock().expect("lock poisoned");
        let prev = senders.insert(request_id, tx);
        assert!(prev.is_none(), "duplicate request_id in ResponseRouter");
    }

    /// Remove the sender for `request_id` without delivering a response.
    ///
    /// Called by the requester on timeout or cancellation.  No-op if no
    /// sender is registered (e.g. the TUI already routed the response).
    pub fn unregister(&self, request_id: &str) {
        let mut senders = self.senders.lock().expect("lock poisoned");
        senders.remove(request_id);
    }

    /// Route a response to the requester identified by `request_id`.
    ///
    /// Returns `true` if the response was successfully delivered, `false`
    /// if no sender was registered or the receiver has been dropped.
    ///
    /// The sender is removed from the map **under the lock**, then
    /// `send()` is called **outside the lock** — this prevents deadlock
    /// when the receiver is a rendezvous channel (capacity 0).
    pub fn route(&self, request_id: &str, response: InterventionResponse) -> bool {
        let sender = {
            let mut senders = self.senders.lock().expect("lock poisoned");
            senders.remove(request_id)
        };

        match sender {
            Some(tx) => tx.send(response).is_ok(),
            None => false,
        }
    }
}

impl Default for ResponseRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Request ID generation ─────────────────────────────────────────────────

/// Generate a unique request identifier for intervention prompts.
///
/// Uses an atomic counter for uniqueness within a session — not a UUID v4
/// (which would require randomness per RFC 4122), but sufficient for
/// correlating intervention requests and responses.
pub fn next_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("req-{id:016x}")
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_response(chosen: Option<usize>) -> InterventionResponse {
        InterventionResponse {
            chosen,
            custom_text: None,
        }
    }

    #[test]
    fn test_new_router_is_empty() {
        let router = ResponseRouter::new();
        // routing to unknown id should return false
        assert!(!router.route("nonexistent", make_response(Some(0))));
    }

    #[test]
    fn test_register_and_route() {
        let router = ResponseRouter::new();
        // Use capacity 1 so route() doesn't block — in practice the
        // receiver thread (tool/hook) is already waiting on recv()
        // when the TUI calls route() on a different thread.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);

        router.register("test-1".into(), tx);
        let delivered = router.route("test-1", make_response(Some(2)));

        assert!(delivered);
        let resp = rx.recv().unwrap();
        assert_eq!(resp.chosen, Some(2));
    }

    #[test]
    fn test_route_removes_entry() {
        let router = ResponseRouter::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);

        router.register("test-2".into(), tx);
        let _ = router.route("test-2", make_response(Some(0)));

        // second route to same id should return false
        assert!(!router.route("test-2", make_response(Some(1))));
    }

    #[test]
    fn test_unregister_before_route_prevents_delivery() {
        let router = ResponseRouter::new();
        let (tx, _rx) = std::sync::mpsc::sync_channel(0);

        router.register("test-3".into(), tx);
        router.unregister("test-3");

        assert!(!router.route("test-3", make_response(Some(0))));
    }

    #[test]
    fn test_route_on_dropped_receiver() {
        let router = ResponseRouter::new();
        let (tx, rx) = std::sync::mpsc::sync_channel::<InterventionResponse>(0);

        router.register("test-4".into(), tx);
        drop(rx); // receiver gone

        let delivered = router.route("test-4", make_response(Some(0)));
        assert!(!delivered);
    }

    #[test]
    #[should_panic(expected = "duplicate request_id")]
    fn test_duplicate_register_panics() {
        let router = ResponseRouter::new();
        let (tx1, _rx1) = std::sync::mpsc::sync_channel(0);
        let (tx2, _rx2) = std::sync::mpsc::sync_channel(0);

        router.register("dup".into(), tx1);
        router.register("dup".into(), tx2); // panics
    }

    #[test]
    fn test_next_request_id_is_unique() {
        let a = next_request_id();
        let b = next_request_id();
        assert_ne!(a, b);
    }
}
