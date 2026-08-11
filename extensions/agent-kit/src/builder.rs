//! # Builder — assemble a `core::engine::Agent` from a user-defined agent struct
//!
//! The `#[derive(Agent)]` macro generates a `into_agent` / `into_agent_with`
//! method on the user's struct. This module provides the [`BuildConfig`]
//! those methods accept, plus a manual [`AgentAssembler`] for cases where
//! the derive macro is not used.

use std::sync::Arc;

use engine::{Agent, AgentError, AgentHook, EngineContext};
use memory::{Memory, PendingHints, SharedMemory};
use provider::{LLMClient, Message, Role};
use tools::ToolRegistry;

/// Configuration for assembling an agent from an agent struct.
///
/// Mirrors the per-method configuration in NVIDIA OO Agents
/// (`max_iterations`, `max_steps`, ...) plus the Agent Oxide-specific knobs.
pub struct BuildConfig {
    /// Max ReAct loop iterations (default 50).
    pub max_steps: usize,
    /// Max retries for transient provider failures (default 3).
    pub max_retries: usize,
    /// Enable SSE streaming (default true).
    pub streaming: bool,
    /// Shared memory override — auto-created (with the system prompt
    /// seeded) when `None`.
    pub memory: Option<SharedMemory>,
    /// Extra lifecycle hooks appended after the derive-generated ones.
    pub extra_hooks: Vec<Box<dyn AgentHook>>,
    /// Queue for user hints injected during active runs.
    pub pending_hints: Option<PendingHints>,
}

impl std::fmt::Debug for BuildConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildConfig")
            .field("max_steps", &self.max_steps)
            .field("max_retries", &self.max_retries)
            .field("streaming", &self.streaming)
            .field("memory", &self.memory.is_some())
            .field("extra_hooks", &self.extra_hooks.len())
            .field("pending_hints", &self.pending_hints.is_some())
            .finish()
    }
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            max_steps: 50,
            max_retries: 3,
            streaming: true,
            memory: None,
            extra_hooks: Vec::new(),
            pending_hints: None,
        }
    }
}

impl BuildConfig {
    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }

    pub fn max_retries(mut self, n: usize) -> Self {
        self.max_retries = n;
        self
    }

    pub fn streaming(mut self, on: bool) -> Self {
        self.streaming = on;
        self
    }

    pub fn memory(mut self, memory: SharedMemory) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn hook(mut self, hook: impl AgentHook + 'static) -> Self {
        self.extra_hooks.push(Box::new(hook));
        self
    }
}

/// Manual assembler — the runtime half of `#[derive(Agent)]`'s
/// `into_agent_with`, exposed for hand-written agents that don't use
/// the macro.
///
/// ```ignore
/// let agent = AgentAssembler::new(client, "deepseek-v4-pro")
///     .system_prompt("You are a helpful assistant.")
///     .tools(|reg| { reg.register(Arc::new(MyTool)); })
///     .config(BuildConfig::default().max_steps(100))
///     .build()?;
/// ```
pub struct AgentAssembler<C: LLMClient> {
    client: C,
    model: String,
    system_prompt: Option<String>,
    tools: ToolRegistry,
    config: BuildConfig,
}

impl<C: LLMClient + 'static> AgentAssembler<C> {
    pub fn new(client: C, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            system_prompt: None,
            tools: ToolRegistry::new(),
            config: BuildConfig::default(),
        }
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Register tools via a closure (called with the registry).
    pub fn tools<F: FnOnce(&mut ToolRegistry)>(mut self, f: F) -> Self {
        f(&mut self.tools);
        self
    }

    pub fn config(mut self, config: BuildConfig) -> Self {
        self.config = config;
        self
    }

    pub fn build(self) -> Result<Agent<C>, AgentError> {
        let memory = self.config.memory.unwrap_or_else(|| {
            let mem: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));
            if let Some(prompt) = &self.system_prompt {
                let mut w = mem.write().expect("memory lock poisoned");
                w.push(Message::new(Role::System, prompt.clone()));
            }
            mem
        });

        let hooks: Vec<Box<dyn AgentHook>> = self.config.extra_hooks;
        let mut builder =
            EngineContext::builder(self.client, memory, Arc::new(self.tools), self.model)
                .hooks(hooks)
                .max_steps(self.config.max_steps)
                .max_retries(self.config.max_retries)
                .streaming(self.config.streaming);
        if let Some(hints) = self.config.pending_hints {
            builder = builder.pending_hints(hints);
        }
        Ok(Agent::new(builder.build()))
    }
}
