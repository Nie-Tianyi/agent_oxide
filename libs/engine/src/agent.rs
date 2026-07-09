//! # Agent — Core ReAct Loop
//!
//! Drives autonomous tool-using conversations with an LLM provider.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc;

use memory::SharedMemory;
use provider::{
    CompletionRequest, CompletionResponse, LLMClient, Message, ProviderError, Role, StreamChunk,
    ToolCall, ToolCallFunction, ToolCallType,
};
use tools::ToolError;

use crate::context::EngineContext;

// ── AgentError ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum AgentError {
    Provider(ProviderError),
    Tool { name: String, error: ToolError },
    MaxStepsReached(usize),
    NoOutput,
    NoChoices,
    Memory(String),
    ToolRejected { name: String, reason: String },
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(e) => write!(f, "provider error: {e}"),
            Self::Tool { name, error } => write!(f, "tool '{name}' error: {error}"),
            Self::MaxStepsReached(n) => write!(f, "max steps ({n}) reached"),
            Self::NoOutput => write!(f, "model returned empty output"),
            Self::NoChoices => write!(f, "response has no choices"),
            Self::Memory(msg) => write!(f, "memory error: {msg}"),
            Self::ToolRejected { name, reason } => write!(f, "tool '{name}' rejected: {reason}"),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<ProviderError> for AgentError {
    fn from(e: ProviderError) -> Self {
        Self::Provider(e)
    }
}

// ── AgentEvent ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Token(String),
    ReasoningToken(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgsDelta {
        id: String,
        delta: String,
    },
    ToolResult {
        id: String,
        name: String,
        output: String,
    },
    ShellRunning {
        command: String,
    },
    ShellOutput {
        command: String,
        output: String,
    },
    /// A shell command requires user approval before execution.
    /// Sent by [`DangerousCommandApprovalHook`](crate::hooks::DangerousCommandApprovalHook)
    /// to the TUI so it can render a confirmation prompt.
    ShellApprovalRequested {
        tool_call_id: String,
        command: String,
    },
    Done,
}

// ── Agent ─────────────────────────────────────────────────────────────────────

pub struct Agent<C: LLMClient> {
    ctx: EngineContext<C>,
}

impl<C: LLMClient> Agent<C> {
    pub fn new(ctx: EngineContext<C>) -> Self {
        Self { ctx }
    }

    pub async fn run_loop(&self, user_input: &str) -> Result<String, AgentError> {
        if self.ctx.streaming {
            self.run_streaming_loop(user_input, None).await
        } else {
            self.run_non_streaming_loop(user_input, None).await
        }
    }

    pub async fn run_with_events(
        &self,
        user_input: &str,
        tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<String, AgentError> {
        if self.ctx.streaming {
            self.run_streaming_loop(user_input, Some(tx)).await
        } else {
            self.run_non_streaming_loop(user_input, Some(tx)).await
        }
    }

    pub fn streaming(&self) -> bool {
        self.ctx.streaming
    }

    pub fn model(&self) -> &str {
        &self.ctx.model
    }

    pub fn max_steps(&self) -> usize {
        self.ctx.max_steps
    }

    pub fn memory(&self) -> &SharedMemory {
        &self.ctx.memory
    }
}

// ── Streaming Loop ────────────────────────────────────────────────────────────

impl<C: LLMClient> Agent<C> {
    async fn run_streaming_loop(
        &self,
        user_input: &str,
        tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> Result<String, AgentError> {
        for hook in &self.ctx.hooks {
            hook.on_run_start("default", user_input);
        }

        let mut steps = 0;
        loop {
            if steps >= self.ctx.max_steps {
                return Err(AgentError::MaxStepsReached(self.ctx.max_steps));
            }
            steps += 1;

            // Full LLM compaction when over budget (before fetching messages).
            self.maybe_compact().await?;

            let messages = {
                let mem = self.ctx.memory.read().unwrap();
                if self.ctx.compact_tool_outputs {
                    mem.to_compact_context_vec(
                        self.ctx.keep_recent_tool_outputs,
                        &self.ctx.compactable_tool_names,
                    )
                } else {
                    mem.to_context_vec()
                }
            };

            let tools = self.ctx.tools.to_tool_defs();
            let request = CompletionRequest::new(&self.ctx.model, messages)
                .with_stream(true)
                .with_tools(tools);

            for hook in &self.ctx.hooks {
                hook.on_llm_start("default");
            }

            let mut stream =
                stream_with_retry(&self.ctx.llm, request.clone(), self.ctx.max_retries).await?;

            let mut acc = StreamAccumulator::new();

            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result?;

                if let Some(ref tx) = tx {
                    emit_chunk_events(tx, &chunk, &mut acc);
                } else {
                    acc.ingest(&chunk);
                }
            }

            let assistant_msg = acc.into_assistant_message();

            for hook in &self.ctx.hooks {
                hook.on_llm_end("default", &assistant_msg);
            }

            if let Some(tool_calls) = &assistant_msg.tool_calls {
                if tool_calls.is_empty() {
                    if let Some(ref tx) = tx {
                        let _ = tx.send(AgentEvent::Done);
                    }
                    return Ok(assistant_msg.content);
                }

                {
                    let mut mem = self.ctx.memory.write().unwrap();
                    mem.push(assistant_msg.clone());
                }

                for tc in tool_calls {
                    let mut blocked = false;
                    for hook in &self.ctx.hooks {
                        if let Err(e) = hook.before_tool_call("default", tc) {
                            let msg = Message::tool_result(&tc.id, format!("Tool rejected: {e}"));
                            {
                                let mut mem = self.ctx.memory.write().unwrap();
                                mem.push(msg);
                            }
                            if let Some(ref tx) = tx {
                                let _ = tx.send(AgentEvent::ToolResult {
                                    id: tc.id.clone(),
                                    name: tc.function.name.clone(),
                                    output: format!("[rejected] {e}"),
                                });
                            }
                            blocked = true;
                            break;
                        }
                    }

                    if blocked {
                        continue;
                    }

                    let result = self
                        .ctx
                        .tools
                        .execute(&tc.function.name, &tc.function.arguments);

                    let observation = match result {
                        Some(Ok(output)) => output,
                        Some(Err(e)) => e.to_string(),
                        None => format!("Tool not found: {}", tc.function.name),
                    };

                    for hook in &self.ctx.hooks {
                        hook.after_tool_call("default", tc, &observation);
                    }

                    {
                        let mut mem = self.ctx.memory.write().unwrap();
                        mem.push(Message::tool_result(&tc.id, &observation));
                    }

                    if let Some(ref tx) = tx {
                        let _ = tx.send(AgentEvent::ToolResult {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            output: observation,
                        });
                    }
                }
            } else {
                {
                    let mut mem = self.ctx.memory.write().unwrap();
                    mem.push(assistant_msg.clone());
                }
                if let Some(ref tx) = tx {
                    let _ = tx.send(AgentEvent::Done);
                }
                return Ok(assistant_msg.content);
            }
        }
    }

    async fn run_non_streaming_loop(
        &self,
        user_input: &str,
        tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> Result<String, AgentError> {
        for hook in &self.ctx.hooks {
            hook.on_run_start("default", user_input);
        }

        let mut steps = 0;
        loop {
            if steps >= self.ctx.max_steps {
                return Err(AgentError::MaxStepsReached(self.ctx.max_steps));
            }
            steps += 1;

            // Full LLM compaction when over budget (before fetching messages).
            self.maybe_compact().await?;

            let messages = {
                let mem = self.ctx.memory.read().unwrap();
                if self.ctx.compact_tool_outputs {
                    mem.to_compact_context_vec(
                        self.ctx.keep_recent_tool_outputs,
                        &self.ctx.compactable_tool_names,
                    )
                } else {
                    mem.to_context_vec()
                }
            };

            let tools = self.ctx.tools.to_tool_defs();
            let request = CompletionRequest::new(&self.ctx.model, messages)
                .with_stream(false)
                .with_tools(tools);

            for hook in &self.ctx.hooks {
                hook.on_llm_start("default");
            }

            let response =
                generate_with_retry(&self.ctx.llm, request, self.ctx.max_retries).await?;

            let choice = response
                .choices
                .into_iter()
                .next()
                .ok_or(AgentError::NoChoices)?;
            let content = choice.message.content.unwrap_or_default();
            let tool_calls = choice.message.tool_calls;
            let reasoning = choice.message.reasoning_content;

            let msg = Message {
                role: Role::Assistant,
                content: content.clone(),
                tool_calls: tool_calls.clone(),
                tool_call_id: None,
                name: None,
            };

            for hook in &self.ctx.hooks {
                hook.on_llm_end("default", &msg);
            }

            if let Some(ref tx) = tx {
                if let Some(ref r) = reasoning {
                    if !r.is_empty() {
                        let _ = tx.send(AgentEvent::ReasoningToken(r.clone()));
                    }
                }
                let _ = tx.send(AgentEvent::Token(content.clone()));
            }

            if let Some(tool_calls) = tool_calls {
                if tool_calls.is_empty() {
                    if let Some(ref tx) = tx {
                        let _ = tx.send(AgentEvent::Done);
                    }
                    {
                        let mut mem = self.ctx.memory.write().unwrap();
                        mem.push(msg);
                    }
                    return Ok(content);
                }

                {
                    let mut mem = self.ctx.memory.write().unwrap();
                    mem.push(msg);
                }

                for tc in &tool_calls {
                    let mut blocked = false;
                    for hook in &self.ctx.hooks {
                        if let Err(e) = hook.before_tool_call("default", tc) {
                            let tool_msg =
                                Message::tool_result(&tc.id, format!("Tool rejected: {e}"));
                            {
                                let mut mem = self.ctx.memory.write().unwrap();
                                mem.push(tool_msg);
                            }
                            blocked = true;
                            break;
                        }
                    }
                    if blocked {
                        continue;
                    }

                    let result = self
                        .ctx
                        .tools
                        .execute(&tc.function.name, &tc.function.arguments);
                    let observation = match result {
                        Some(Ok(output)) => output,
                        Some(Err(e)) => e.to_string(),
                        None => format!("Tool not found: {}", tc.function.name),
                    };

                    for hook in &self.ctx.hooks {
                        hook.after_tool_call("default", tc, &observation);
                    }

                    {
                        let mut mem = self.ctx.memory.write().unwrap();
                        mem.push(Message::tool_result(&tc.id, &observation));
                    }
                }
            } else {
                if let Some(ref tx) = tx {
                    let _ = tx.send(AgentEvent::Done);
                }
                {
                    let mut mem = self.ctx.memory.write().unwrap();
                    mem.push(msg);
                }
                return Ok(content);
            }
        }
    }
}

// ── LLM Compaction ────────────────────────────────────────────────────────────

impl<C: LLMClient> Agent<C> {
    /// Check whether full LLM compaction is needed and run it if so.
    ///
    /// Called at the start of each agent loop iteration.
    /// When [`EngineContext::compact_model`] is `Some`, this checks
    /// [`Memory::needs_compact`] and runs an LLM summarisation pass
    /// (drain → summarise → apply) to bring the conversation back
    /// under the character budget.
    async fn maybe_compact(&self) -> Result<(), AgentError> {
        let compact_model = match &self.ctx.compact_model {
            Some(m) => m.clone(),
            None => return Ok(()),
        };

        let needs = {
            let mem = self.ctx.memory.read().unwrap();
            mem.needs_compact()
        };

        if !needs {
            return Ok(());
        }

        let old = {
            let mut mem = self.ctx.memory.write().unwrap();
            mem.drain_for_compact()
        };

        if old.is_empty() {
            return Ok(());
        }

        let transcript: String = old
            .iter()
            .map(|m| format!("[{}]: {}", role_label(m.role), m.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "Summarise the following conversation history concisely. \
             Preserve key facts, decisions, and context. \
             Output only the summary, no preamble:\n\n{transcript}"
        );

        let request =
            CompletionRequest::new(&compact_model, vec![Message::new(Role::User, prompt)]);

        let response = self.ctx.llm.generate(request).await?;

        let summary = response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        {
            let mut mem = self.ctx.memory.write().unwrap();
            mem.apply_compact(summary);
        }

        Ok(())
    }
}

const fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "System",
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
    }
}

// ── StreamAccumulator ─────────────────────────────────────────────────────────

struct StreamAccumulator {
    content: String,
    reasoning: String,
    tool_calls: BTreeMap<u32, ToolCallAccumulator>,
}

struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

impl StreamAccumulator {
    fn new() -> Self {
        Self {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: BTreeMap::new(),
        }
    }

    fn ingest(&mut self, chunk: &StreamChunk) {
        for choice in &chunk.choices {
            if let Some(ref c) = choice.delta.content {
                self.content.push_str(c);
            }
            if let Some(ref r) = choice.delta.reasoning_content {
                self.reasoning.push_str(r);
            }
            if let Some(ref tool_calls) = choice.delta.tool_calls {
                for tc in tool_calls {
                    let entry =
                        self.tool_calls
                            .entry(tc.index)
                            .or_insert_with(|| ToolCallAccumulator {
                                id: tc.id.clone(),
                                name: String::new(),
                                arguments: String::new(),
                            });
                    if !tc.id.is_empty() {
                        entry.id = tc.id.clone();
                    }
                    if !tc.function.name.is_empty() {
                        entry.name = tc.function.name.clone();
                    }
                    entry.arguments.push_str(&tc.function.arguments);
                }
            }
        }
    }

    fn into_assistant_message(self) -> Message {
        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .into_values()
            .map(|acc| ToolCall {
                index: 0,
                id: acc.id,
                r#type: ToolCallType::Function,
                function: ToolCallFunction {
                    name: acc.name,
                    arguments: acc.arguments,
                },
            })
            .collect();

        let tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        };

        Message {
            role: Role::Assistant,
            content: self.content,
            tool_calls,
            tool_call_id: None,
            name: None,
        }
    }
}

// ── Event emission ────────────────────────────────────────────────────────────

fn emit_chunk_events(
    tx: &mpsc::UnboundedSender<AgentEvent>,
    chunk: &StreamChunk,
    acc: &mut StreamAccumulator,
) {
    for choice in &chunk.choices {
        if let Some(ref c) = choice.delta.content {
            let _ = tx.send(AgentEvent::Token(c.clone()));
        }
        if let Some(ref r) = choice.delta.reasoning_content {
            let _ = tx.send(AgentEvent::ReasoningToken(r.clone()));
        }
        if let Some(ref tool_calls) = choice.delta.tool_calls {
            for tc in tool_calls {
                if !tc.id.is_empty() && !tc.function.name.is_empty() {
                    let _ = tx.send(AgentEvent::ToolCallStart {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                    });
                }
                if !tc.function.arguments.is_empty() {
                    let _ = tx.send(AgentEvent::ToolCallArgsDelta {
                        id: tc.id.clone(),
                        delta: tc.function.arguments.clone(),
                    });
                }
            }
        }
    }
    acc.ingest(chunk);
}

// ── Retry helpers ─────────────────────────────────────────────────────────────

async fn generate_with_retry(
    client: &impl LLMClient,
    request: CompletionRequest,
    max_retries: usize,
) -> Result<CompletionResponse, AgentError> {
    let mut last_err = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_millis(500 * 2u64.pow(attempt as u32 - 1));
            tokio::time::sleep(backoff).await;
        }
        match client.generate(request.clone()).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                if !is_retryable(&e) {
                    return Err(AgentError::Provider(e));
                }
                last_err = Some(e);
            }
        }
    }
    Err(AgentError::Provider(last_err.unwrap()))
}

async fn stream_with_retry(
    client: &impl LLMClient,
    request: CompletionRequest,
    max_retries: usize,
) -> Result<futures_util::stream::BoxStream<'static, Result<StreamChunk, ProviderError>>, AgentError>
{
    let mut last_err = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_millis(500 * 2u64.pow(attempt as u32 - 1));
            tokio::time::sleep(backoff).await;
        }
        match client.stream(request.clone()).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                if !is_retryable(&e) {
                    return Err(AgentError::Provider(e));
                }
                last_err = Some(e);
            }
        }
    }
    Err(AgentError::Provider(last_err.unwrap()))
}

fn is_retryable(err: &ProviderError) -> bool {
    match err {
        ProviderError::Http(_) => true,
        ProviderError::Api { status, .. } => status >= &500u16,
        ProviderError::Parse(_) => false,
        ProviderError::StreamingNotSupported => false,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_accumulator_text_only() {
        let mut acc = StreamAccumulator::new();
        let chunk: StreamChunk = serde_json::from_str(
            r#"{"id":"1","object":"c","created":1,"model":"m","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}],"usage":null}"#,
        )
        .unwrap();
        acc.ingest(&chunk);
        let msg = acc.into_assistant_message();
        assert_eq!(msg.content, "Hello");
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn test_agent_error_display() {
        assert!(AgentError::MaxStepsReached(10).to_string().contains("10"));
        assert!(AgentError::NoOutput.to_string().contains("empty"));
    }

    #[test]
    fn test_is_retryable() {
        assert!(is_retryable(&ProviderError::Http("timeout".into())));
        assert!(is_retryable(&ProviderError::Api {
            status: 503,
            body: "".into()
        }));
        assert!(!is_retryable(&ProviderError::Api {
            status: 400,
            body: "".into()
        }));
        assert!(!is_retryable(&ProviderError::Parse("oops".into())));
    }
}
