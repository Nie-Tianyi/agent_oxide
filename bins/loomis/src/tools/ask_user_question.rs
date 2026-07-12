//! [`AskUserQuestionTool`] — lets the LLM ask the user a question.
//!
//! # How it works
//!
//! The tool pauses the agent and shows an interactive prompt in the TUI
//! via the existing [`InterventionRequired`](engine::AgentEvent::InterventionRequired)
//! mechanism.  The user navigates options (or types free-form text) and
//! their response is returned as the tool output.
//!
//! # Comparison with SandboxHook
//!
//! | Aspect | SandboxHook | AskUserQuestionTool |
//! |--------|-------------|---------------------|
//! | Trigger point | `before_tool_call` hook | `execute_stream` (during tool exec) |
//! | Who initiates | Shell tool call by LLM | LLM calls this tool directly |
//! | Purpose | Security approval | Information gathering |
//! | Options | Fixed (Approve/Deny/Other) | LLM-defined |
//! | Timeout | 2 minutes | 5 minutes |

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use engine::{AgentEvent, InterventionRequest, InterventionResponse};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc;
use tools::{ProgressStream, ToolError, tool};

use engine::{ResponseRouter, next_request_id};

// ── Args ────────────────────────────────────────────────────────────────────

/// Arguments for the ask_user_question tool.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AskUserQuestionArgs {
    /// The question or prompt to show the user. Displayed prominently.
    #[schemars(
        description = "The question or prompt to show the user. Be clear and specific about what you need them to answer."
    )]
    pub question: String,

    /// Optional additional context, explanation, or background for the
    /// question. Shown below the question in a dimmer style.
    #[schemars(
        description = "Optional additional context, explanation, or background for the question."
    )]
    pub description: Option<String>,

    /// Predefined choices for the user. If omitted or empty, the user
    /// types a free-form text response. An option whose label ends with
    /// "…" (like "Other…") lets the user type custom text.
    #[schemars(
        description = "Predefined choices for the user to pick from. If omitted, the user types a free-form response. End an option with \"…\" to allow custom text input."
    )]
    pub options: Option<Vec<String>>,
}

// ── Tool ────────────────────────────────────────────────────────────────────

/// Lets the LLM ask the user a question and wait for their response.
///
/// Use this when you need the user to make a choice, provide input, or
/// answer a question that only they can answer — preferences, design
/// decisions, clarification of ambiguous requirements, confirmation of
/// actions, information only the user knows, etc.
///
/// # Parameters
///
/// ```json
/// {
///   "question": "Which approach should I use?",
///   "description": "Option A is faster, Option B is more maintainable.",
///   "options": ["Option A", "Option B", "Other…"]
/// }
/// ```
///
/// # Response
///
/// - Predefined option selected → the label text (e.g. `"Option A"`)
/// - "…" option with custom text → the custom text only
/// - User cancelled (Esc) → error
/// - Timeout after 5 minutes → error
#[tool(
    name = "ask_user_question",
    description = "Ask the user a question and wait for their response. Use this when you \
         need the user to make a choice, provide input, or answer a question that only \
         they can answer.\n\n\
         You can provide predefined options for the user to choose from, or leave \
         options empty for a free-form text response.\n\n\
         When to use:\n\
         - Asking for user preferences or design decisions\n\
         - Requesting clarification on ambiguous requirements\n\
         - Confirming potentially destructive actions\n\
         - Gathering information only the user knows\n\n\
         When NOT to use:\n\
         - Asking questions you can answer from the codebase or tools\n\
         - Asking rhetorical questions\n\
         - Asking for information that doesn't affect your next action",
    args = AskUserQuestionArgs
)]
pub struct AskUserQuestionTool {
    /// Sender for agent events — used to emit InterventionRequired.
    agent_tx: OnceLock<mpsc::UnboundedSender<AgentEvent>>,
    /// Shared router for receiving the user's response.
    response_router: Arc<ResponseRouter>,
}

impl AskUserQuestionTool {
    /// Creates a new tool that shares the given response router.
    pub fn new(response_router: Arc<ResponseRouter>) -> Self {
        Self {
            agent_tx: OnceLock::new(),
            response_router,
        }
    }

    /// Called by `build_coding_agent` after the agent-event channel is
    /// created.  Must be set before the tool can be used.
    pub fn set_agent_tx(&self, tx: mpsc::UnboundedSender<AgentEvent>) {
        let _ = self.agent_tx.set(tx);
    }

    /// Core logic — blocks the agent task until the user responds.
    fn execute_stream(&self, args: AskUserQuestionArgs) -> Result<ProgressStream, ToolError> {
        let request_id = next_request_id();

        // Default to a single free-text option if none provided.
        // Clone options now — we'll need them again later to resolve
        // the chosen index to a label.
        let options: Vec<String> = args
            .options
            .clone()
            .filter(|opts| !opts.is_empty())
            .unwrap_or_else(|| vec!["Answer…".into()]);

        // Create per-request rendezvous channel and register with the
        // response router so the TUI can deliver the answer.
        let (tx, rx) = std::sync::mpsc::sync_channel::<InterventionResponse>(0);
        self.response_router.register(request_id.clone(), tx);

        // Send intervention request to the TUI.
        if let Some(agent_tx) = self.agent_tx.get() {
            let _ = agent_tx.send(AgentEvent::InterventionRequired(InterventionRequest {
                request_id: request_id.clone(),
                title: args.question,
                description: args.description.unwrap_or_default(),
                options: options.clone(),
            }));
        }

        // Block until the user responds (5-minute timeout).
        let response = match rx.recv_timeout(Duration::from_secs(300)) {
            Ok(resp) => resp,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.response_router.unregister(&request_id);
                return Err(ToolError::Execution(
                    "Timed out waiting for user response (5 minutes)".into(),
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ToolError::Execution(
                    "Intervention channel disconnected (TUI may have exited)".into(),
                ));
            }
        };

        // Cleanup (no-op if the TUI's route() already removed the entry).
        self.response_router.unregister(&request_id);

        // Build output from the user's response.
        // The `options` local is still valid — we cloned it into the
        // InterventionRequest above.

        match (response.chosen, response.custom_text) {
            (None, _) => Err(ToolError::Execution("User cancelled the question".into())),
            (Some(_idx), Some(custom)) => {
                // User selected "…" option and typed custom text.
                // Return just the custom text — the option label is
                // boilerplate like "Other…".
                Ok(ProgressStream::done(custom))
            }
            (Some(idx), None) => {
                // User selected a specific option.
                let label = options
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| format!("Option {idx}"));
                Ok(ProgressStream::done(label))
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    #[test]
    fn test_name() {
        let router = Arc::new(ResponseRouter::new());
        assert_eq!(AskUserQuestionTool::new(router).name(), "ask_user_question");
    }

    #[test]
    fn test_description() {
        let router = Arc::new(ResponseRouter::new());
        assert!(
            AskUserQuestionTool::new(router)
                .description()
                .contains("Ask the user")
        );
    }

    #[test]
    fn test_parameters_schema() {
        let router = Arc::new(ResponseRouter::new());
        let params = AskUserQuestionTool::new(router).parameter_schema();
        assert_eq!(params["type"], "object");
        assert!(
            params["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("question"))
        );
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_invalid_json() {
        let router = Arc::new(ResponseRouter::new());
        let tool = AskUserQuestionTool::new(router);
        let err = Tool::execute_stream(&tool, "garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_missing_question_field() {
        let router = Arc::new(ResponseRouter::new());
        let tool = AskUserQuestionTool::new(router);
        let err = Tool::execute_stream(&tool, r#"{"wrong": "field"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_extra_field_rejected() {
        let router = Arc::new(ResponseRouter::new());
        let tool = AskUserQuestionTool::new(router);
        let err =
            Tool::execute_stream(&tool, r#"{"question": "hello", "extra": true}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
