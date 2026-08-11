//! # Context blocks — dynamic system-prompt injection
//!
//! Mirrors NVIDIA OO Agents' "context blocks" (`agent.context["notes"] =
//! Context(expr="self.render_notes()")`): named blocks of content that
//! appear in the agent's system prompt, re-evaluated before every LLM
//! call when dynamic.
//!
//! Implemented as a standard [`AgentHook`] that writes `[CONTEXT:key]`
//! System messages via `insert_before_history` — the same mechanism the
//! existing SkillHook / PlanModeHook / ProfileHook / TodoListHook use.
//! No core changes required.

use engine::AgentHook;
use memory::SharedMemory;
use provider::{Message, Role};

/// A named block of context injected into the system prompt.
///
/// `render()` is called on every LLM call for dynamic blocks; static
/// blocks hold fixed content. Returning `None` omits the block
/// (e.g. when a skill is not active).
pub struct ContextBlock {
    key: String,
    priority: i32,
    render: Box<dyn Fn() -> Option<String> + Send + Sync>,
}

impl ContextBlock {
    /// A fixed-content block.
    pub fn static_block(key: impl Into<String>, priority: i32, content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            key: key.into(),
            priority,
            render: Box::new(move || Some(content.clone())),
        }
    }

    /// A block whose content is recomputed before every LLM call.
    pub fn dynamic_block(
        key: impl Into<String>,
        priority: i32,
        f: impl Fn() -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            key: key.into(),
            priority,
            render: Box::new(f),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn priority(&self) -> i32 {
        self.priority
    }

    pub fn render(&self) -> Option<String> {
        (self.render)()
    }
}

/// An [`AgentHook`] that injects [`ContextBlock`]s into the system prompt.
///
/// Blocks are rendered in priority order (lower first) into
/// `[CONTEXT:key]`-prefixed System messages, placed at the tail of the
/// System block via `insert_before_history` (prompt-cache friendly).
pub struct ContextBlockHook {
    blocks: Vec<ContextBlock>,
}

impl ContextBlockHook {
    pub fn new() -> Self {
        Self { blocks: Vec::new() }
    }

    pub fn add(&mut self, block: ContextBlock) {
        self.blocks.push(block);
        self.blocks.sort_by_key(|b| b.priority());
    }

    pub fn add_static(
        &mut self,
        key: impl Into<String>,
        priority: i32,
        content: impl Into<String>,
    ) {
        self.add(ContextBlock::static_block(key, priority, content));
    }

    pub fn add_dynamic(
        &mut self,
        key: impl Into<String>,
        priority: i32,
        f: impl Fn() -> Option<String> + Send + Sync + 'static,
    ) {
        self.add(ContextBlock::dynamic_block(key, priority, f));
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Render all blocks to `(key, content)` pairs, omitting blocks that
    /// render `None`. Used by generation methods to inline their context
    /// into the system prompt (their LLM calls bypass the hook pipeline).
    pub fn render_all(&self) -> Vec<(String, String)> {
        self.blocks
            .iter()
            .filter_map(|b| b.render().map(|content| (b.key().to_string(), content)))
            .collect()
    }
}

impl Default for ContextBlockHook {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentHook for ContextBlockHook {
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let mut mem = memory.write().expect("memory lock poisoned");
        for block in &self.blocks {
            let marker = format!("[CONTEXT:{}]", block.key());
            // Remove-then-reinsert (same pattern as SkillHook/ProfileHook).
            mem.messages.retain(|m| !m.content.starts_with(&marker));
            if let Some(content) = block.render() {
                let msg = Message::new(Role::System, format!("{marker}\n{content}"));
                let idx = mem
                    .messages
                    .iter()
                    .position(|m| m.role != Role::System)
                    .unwrap_or(mem.messages.len());
                mem.messages.insert(idx, msg);
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use memory::Memory;

    fn shared_memory() -> SharedMemory {
        std::sync::Arc::new(std::sync::RwLock::new(Memory::new()))
    }

    #[test]
    fn test_static_block_renders() {
        let hook = ContextBlockHook::new_with(|h| {
            h.add_static("project", 10, "You are working on a Rust project.");
        });
        let mem = shared_memory();
        hook.on_llm_start("s", &mem);
        let msgs = mem.read().unwrap().to_context_vec();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.starts_with("[CONTEXT:project]"));
    }

    #[test]
    fn test_dynamic_block_rereads() {
        let value = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let value2 = value.clone();
        let hook = ContextBlockHook::new_with(|h| {
            h.add_dynamic("count", 10, move || {
                Some(format!(
                    "count={}",
                    value2.load(std::sync::atomic::Ordering::Relaxed)
                ))
            });
        });
        let mem = shared_memory();
        hook.on_llm_start("s", &mem);
        value.store(42, std::sync::atomic::Ordering::Relaxed);
        hook.on_llm_start("s", &mem);
        let msgs = mem.read().unwrap().to_context_vec();
        assert_eq!(msgs.len(), 1, "remove-then-reinsert keeps exactly one");
        assert!(
            msgs[0].content.contains("count=42"),
            "dynamic re-evaluation"
        );
    }

    #[test]
    fn test_none_omits_block() {
        let hook = ContextBlockHook::new_with(|h| {
            h.add_dynamic("maybe", 10, || None);
        });
        let mem = shared_memory();
        hook.on_llm_start("s", &mem);
        assert!(mem.read().unwrap().to_context_vec().is_empty());
    }

    #[test]
    fn test_priority_order() {
        let hook = ContextBlockHook::new_with(|h| {
            h.add_static("late", 100, "L");
            h.add_static("early", 10, "E");
        });
        let mem = shared_memory();
        hook.on_llm_start("s", &mem);
        let msgs = mem.read().unwrap().to_context_vec();
        assert!(msgs[0].content.contains("early"));
        assert!(msgs[1].content.contains("late"));
    }

    trait NewWith {
        fn new_with<F: FnOnce(&mut ContextBlockHook)>(f: F) -> Self;
    }
    impl NewWith for ContextBlockHook {
        fn new_with<F: FnOnce(&mut ContextBlockHook)>(f: F) -> Self {
            let mut h = ContextBlockHook::new();
            f(&mut h);
            h
        }
    }
}
