//! # Generation — "generation methods"
//!
//! In the NVIDIA OO Agents paradigm, a method whose body is `...` (or empty)
//! is **implemented by the LLM at runtime**. The Rust equivalent:
//!
//! ```rust,ignore
//! #[agent_impl]
//! impl FeedbackAgent {
//!     /// Analyze customer feedback for sentiment and key topics in one sentence.
//!     async fn analyze_feedback(&self, text: String) -> String {}
//! }
//! ```
//!
//! The `#[agent_impl]` macro replaces the empty body with a call to
//! [`run_generation`]. This module implements that runtime:
//!
//! - **Predict strategy** — a single non-streaming LLM call, no tools.
//!   Fast and cheap; best for classification, extraction, translation.
//! - **CodeAct strategy** — a full ReAct loop with tool access, driven by
//!   the existing `core::engine` machinery. Best for complex multi-step
//!   tasks.
//!
//! When the return type `T` is not `String`, the output is validated
//! against `T`'s JSON Schema and auto-retried on failure — mirroring
//! NVIDIA's Pydantic return-type validation.

use std::sync::Arc;

use engine::{Agent, AgentError, EngineContext};
use memory::{Memory, SharedMemory};
use provider::{CompletionRequest, LLMClient, Message, Role};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use tools::ToolRegistry;

/// How a generation method should execute.
#[derive(Clone, Debug)]
pub enum Strategy {
    /// Single LLM call, no tools. Best for classification/extraction.
    Predict {
        /// Max parse-and-retry attempts when structured output fails.
        max_retries: usize,
    },
    /// Full ReAct loop with tool access (default, mirrors NVIDIA CodeAct).
    CodeAct {
        /// Maximum loop iterations (NVIDIA's `max_iterations`).
        max_iterations: usize,
        /// Max parse-and-retry attempts for structured output.
        max_retries: usize,
    },
}

impl Default for Strategy {
    fn default() -> Self {
        Self::CodeAct {
            max_iterations: 50,
            max_retries: 2,
        }
    }
}

/// Errors from [`run_generation`].
#[derive(Debug)]
pub enum GenerationError {
    /// The LLM provider call failed.
    Provider(AgentError),
    /// The model returned text that failed to parse as `T` after retries.
    Parse(String),
    /// An agent run (CodeAct) failed.
    Run(AgentError),
}

impl std::fmt::Display for GenerationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(e) => write!(f, "provider error: {e}"),
            Self::Parse(e) => write!(f, "could not parse structured output: {e}"),
            Self::Run(e) => write!(f, "agent run failed: {e}"),
        }
    }
}

impl std::error::Error for GenerationError {}

impl From<AgentError> for GenerationError {
    fn from(e: AgentError) -> Self {
        Self::Run(e)
    }
}

/// Run a generation method.
///
/// `system_prompt` is the agent struct's doc comment; `method_prompt` is
/// the method's doc comment plus its serialized arguments. `tools` are
/// the agent's registered tools (used by the CodeAct strategy).
///
/// The model's answer is parsed into `T` (validated against `T`'s JSON
/// Schema) with auto-retry on failure. When `T == String`, the raw text
/// answer is returned without parsing.
///
/// `C` must be `Clone` because the CodeAct strategy builds a fresh
/// `core::engine::Agent` (which owns its client) per call.
pub async fn run_generation<C: LLMClient + Clone + 'static, T: DeserializeOwned + JsonSchema>(
    client: &C,
    model: &str,
    system_prompt: &str,
    method_prompt: &str,
    tools: Option<&ToolRegistry>,
    strategy: &Strategy,
) -> Result<T, GenerationError> {
    match strategy {
        Strategy::Predict { max_retries } => {
            predict_generate::<C, T>(client, model, system_prompt, method_prompt, *max_retries)
                .await
        }
        Strategy::CodeAct {
            max_iterations,
            max_retries,
        } => {
            codeact_generate::<C, T>(
                client,
                model,
                system_prompt,
                method_prompt,
                tools,
                *max_iterations,
                *max_retries,
            )
            .await
        }
    }
}

// ── Predict: single LLM call, no tools ────────────────────────────────────────

async fn predict_generate<C: LLMClient + Clone + 'static, T: DeserializeOwned + JsonSchema>(
    client: &C,
    model: &str,
    system_prompt: &str,
    method_prompt: &str,
    max_retries: usize,
) -> Result<T, GenerationError> {
    let mut attempt = 0;
    let mut last_error: Option<String> = None;
    loop {
        let prompt = build_prompt::<T>(method_prompt, attempt, last_error.as_deref());
        let request = CompletionRequest::new(
            model,
            vec![
                Message::new(Role::System, system_prompt),
                Message::new(Role::User, prompt),
            ],
        );

        let response = client
            .generate(request)
            .await
            .map_err(|e| GenerationError::Provider(AgentError::Provider(e)))?;

        let raw = response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        match finish::<T>(&raw) {
            Ok(value) => return Ok(value),
            Err(e) if attempt < max_retries => {
                attempt += 1;
                last_error = Some(e);
            }
            Err(e) => return Err(GenerationError::Parse(e)),
        }
    }
}

// ── CodeAct: full ReAct loop with tools ───────────────────────────────────────

async fn codeact_generate<C: LLMClient + Clone + 'static, T: DeserializeOwned + JsonSchema>(
    client: &C,
    model: &str,
    system_prompt: &str,
    method_prompt: &str,
    tools: Option<&ToolRegistry>,
    max_iterations: usize,
    max_retries: usize,
) -> Result<T, GenerationError> {
    let mut attempt = 0;
    loop {
        let registry = Arc::new(match tools {
            Some(src) => clone_registry(src),
            None => ToolRegistry::new(),
        });

        let memory: SharedMemory = std::sync::Arc::new(std::sync::RwLock::new(Memory::new()));
        let ctx = EngineContext::builder(client.clone(), memory, registry, model)
            .max_steps(max_iterations)
            .build();
        let agent = Agent::new(ctx);

        // Seed the system prompt before the first user turn.
        {
            let mut mem = agent.memory().write().expect("memory lock poisoned");
            mem.push(Message::new(Role::System, system_prompt));
        }

        let prompt = build_prompt::<T>(method_prompt, attempt, None);
        let answer = match agent.run(&prompt).await {
            Ok(a) => a,
            Err(_e) if attempt < max_retries => {
                attempt += 1;
                continue;
            }
            Err(e) => return Err(GenerationError::Run(e)),
        };

        match finish::<T>(&answer) {
            Ok(value) => return Ok(value),
            Err(_e) if attempt < max_retries => {
                attempt += 1;
                continue;
            }
            Err(e) => return Err(GenerationError::Parse(e)),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Deep-copy a `ToolRegistry` (it does not implement `Clone`; the `Arc`s
/// inside it are cheap to share).
fn clone_registry(src: &ToolRegistry) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    for (_, tool) in src.iter() {
        reg.register(tool.clone());
    }
    reg
}

/// Assemble the user prompt: optional JSON-Schema instruction, the method
/// docstring + arguments, and (on retry) the previous parse error.
fn build_prompt<T: JsonSchema>(
    method_prompt: &str,
    attempt: usize,
    last_error: Option<&str>,
) -> String {
    let mut prompt = String::new();

    let is_structured = std::any::type_name::<T>() != std::any::type_name::<String>();
    if is_structured {
        prompt.push_str("You MUST respond with valid JSON conforming to this JSON Schema:\n");
        prompt.push_str(&schema_pretty::<T>());
        prompt.push_str("\n\n");
    }

    prompt.push_str(method_prompt);

    if let Some(err) = last_error {
        prompt.push_str(&format!(
            "\n\nYour previous response failed to parse:\n{err}\nPlease respond again with valid JSON only."
        ));
    } else if attempt > 0 {
        prompt.push_str("\n\nYour previous response failed to parse. Please respond again with valid JSON only.");
    }

    prompt
}

/// Finalize the model's raw answer into `T`.
///
/// `String` returns pass through verbatim; everything else is parsed
/// as JSON (fences and leading prose tolerated).
fn finish<T: DeserializeOwned + JsonSchema>(raw: &str) -> Result<T, String> {
    if std::any::type_name::<T>() == std::any::type_name::<String>() {
        // T is String — wrap in a JSON string literal.
        return serde_json::from_str::<T>(&json_escape(raw)).map_err(|e| e.to_string());
    }
    parse_structured::<T>(raw)
}

/// Escape `s` as a JSON string literal (with surrounding quotes).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Pretty-printed JSON Schema for `T`.
pub fn schema_pretty<T: JsonSchema>() -> String {
    let schema = tools::generate_schema::<T>();
    serde_json::to_string_pretty(&schema).unwrap_or_default()
}

/// Extract a JSON value from model output, tolerating markdown code fences
/// and leading prose.
pub fn extract_json(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Code fence: ```json ... ```
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(newline) = after.find('\n') {
            let body = &after[newline + 1..];
            if let Some(end) = body.find("```") {
                return Some(body[..end].trim().to_owned());
            }
        }
        return None;
    }

    // Bare JSON: find the first { or [ and the matching last } or ]
    let start = trimmed.find(['{', '['])?;
    let end = trimmed.rfind(['}', ']'])?;
    if end > start {
        Some(trimmed[start..=end].to_owned())
    } else {
        None
    }
}

/// Parse `raw` into `T`, returning a human-readable error on failure.
pub fn parse_structured<T: DeserializeOwned + JsonSchema>(raw: &str) -> Result<T, String> {
    let json = extract_json(raw).ok_or_else(|| {
        format!(
            "no JSON found in response. Expected schema:\n{}",
            schema_pretty::<T>()
        )
    })?;
    serde_json::from_str::<T>(&json).map_err(|e| {
        format!(
            "JSON parse error: {e}\nExpected schema:\n{}",
            schema_pretty::<T>()
        )
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
    struct Mock {
        value: u32,
    }

    #[test]
    fn test_extract_json_bare() {
        assert_eq!(
            extract_json(r#"Here is the result: {"value": 42}"#).as_deref(),
            Some(r#"{"value": 42}"#)
        );
    }

    #[test]
    fn test_extract_json_fence() {
        let raw = "```json\n{\"value\": 42}\n```";
        assert_eq!(extract_json(raw).as_deref(), Some(r#"{"value": 42}"#));
    }

    #[test]
    fn test_extract_json_none() {
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn test_parse_structured_ok() {
        let parsed: Mock = parse_structured(r#"{"value": 7}"#).unwrap();
        assert_eq!(parsed, Mock { value: 7 });
    }

    #[test]
    fn test_parse_structured_fence() {
        let parsed: Mock = parse_structured("```json\n{\"value\": 8}\n```").unwrap();
        assert_eq!(parsed, Mock { value: 8 });
    }

    #[test]
    fn test_parse_structured_fail() {
        let err = parse_structured::<Mock>("not json").unwrap_err();
        assert!(err.contains("no JSON found"));
    }

    #[test]
    fn test_finish_string_passthrough() {
        let out: String = finish("hello world").unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn test_finish_structured() {
        let out: Mock = finish(r#"{"value": 3}"#).unwrap();
        assert_eq!(out, Mock { value: 3 });
    }
}
