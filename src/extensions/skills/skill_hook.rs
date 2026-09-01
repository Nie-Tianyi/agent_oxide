//! SkillHook — injects active skills as System messages before each LLM call.
//!
//! [`SkillTool`](crate::skills::SkillTool) writes activated skills into the
//! shared [`ActiveSkills`](crate::skills::ActiveSkills) state; this hook
//! renders them into the conversation on every `on_llm_start` so the LLM
//! sees the skill's instructions on the very next call.
//!
//! Injection is **stateless remove-then-reinsert**: every `[SKILL]`-prefixed
//! System message is drained first, then the currently active skills are
//! re-injected (sorted by name).  This is self-healing — duplicates, stale
//! content, or skills activated while the hook wasn't registered never
//! accumulate.  It is also the volatile-marker convention shared with
//! [`MacroCompactHook`](crate::compact::MacroCompactHook) and the
//! `ContextBlockHook` pattern: content anchored at the tail of the System
//! block keeps the static system-prompt head byte-identical, preserving the
//! provider's prompt-cache prefix.

use crate::engine::AgentHook;
use crate::memory::SharedMemory;
use crate::provider::{Message, Role};
use crate::util::insert_before_history;

use super::ActiveSkills;

/// Prefix of injected skill System messages — the marker this hook uses to
/// find and drain its own injections (symmetrical with
/// [`COMPACT_SUMMARY_MARKER`](crate::compact::COMPACT_SUMMARY_MARKER)).
pub const SKILL_MARKER: &str = "[SKILL]";

/// Inject the currently active skills as System messages on every LLM call.
///
/// Create one alongside the [`SkillTool`](crate::skills::SkillTool) and pass
/// the same [`ActiveSkills`] `Arc` to both.
///
/// # Volatility order
///
/// Skills change rarely, so this hook should run **early** in the hook
/// chain (before the more volatile injectors like plan-mode or todo-list
/// hooks) — the ordering inside the System tail follows registration order.
#[derive(Debug)]
pub struct SkillHook {
    active: ActiveSkills,
}

impl SkillHook {
    /// Create a new skill-injection hook sharing the tool's
    /// [`ActiveSkills`] state.
    pub fn new(active: ActiveSkills) -> Self {
        Self { active }
    }
}

impl AgentHook for SkillHook {
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        // Snapshot the active set under a read lock; drop it before taking
        // the memory write lock (never hold two locks at once).
        let skills = match self.active.read() {
            Ok(m) => m.clone(),
            Err(_) => {
                tracing::error!("active skills lock poisoned — skipping skill injection");
                return;
            }
        };

        let Ok(mut mem) = memory.write() else {
            tracing::error!("memory lock poisoned — skipping skill injection");
            return;
        };

        // Drain our previous injections.
        mem.messages
            .retain(|m| !(m.role == Role::System && m.content.starts_with(SKILL_MARKER)));

        // Re-inject the active skills in deterministic order.
        let mut names: Vec<&String> = skills.keys().collect();
        names.sort();
        for name in names {
            insert_before_history(
                &mut mem.messages,
                Message::new(Role::System, format!("{SKILL_MARKER}\n\n{}", skills[name])),
            );
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    use super::*;
    use crate::memory::Memory;

    fn active_with(skills: &[(&str, &str)]) -> ActiveSkills {
        let map: HashMap<String, String> = skills
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Arc::new(RwLock::new(map))
    }

    fn memory_with(static_prompt: &str, user_msg: &str) -> SharedMemory {
        let mut mem = Memory::new();
        mem.push(Message::new(Role::System, static_prompt));
        mem.push(Message::new(Role::User, user_msg));
        Arc::new(RwLock::new(mem))
    }

    /// Invoke the hook exactly like the engine does and return the messages.
    fn inject(hook: &SkillHook, memory: &SharedMemory) -> Vec<Message> {
        hook.on_llm_start("test-session", memory);
        memory.read().unwrap().messages().to_vec()
    }

    #[test]
    fn injects_after_static_system_prompt_before_user() {
        let hook = SkillHook::new(active_with(&[("code-review", "Review carefully.")]));
        let memory = memory_with("You are a helpful agent.", "Hello!");

        let messages = inject(&hook, &memory);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].content, "You are a helpful agent.");
        assert!(messages[1].content.starts_with(SKILL_MARKER));
        assert!(messages[1].content.contains("Review carefully."));
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[2].content, "Hello!");
    }

    #[test]
    fn injects_multiple_skills_sorted_by_name() {
        let hook = SkillHook::new(active_with(&[("zebra", "Striped."), ("alpha", "First.")]));
        let memory = memory_with("Static prompt.", "User.");

        let messages = inject(&hook, &memory);
        let injected: Vec<&str> = messages
            .iter()
            .filter(|m| m.content.starts_with(SKILL_MARKER))
            .map(|m| &m.content[SKILL_MARKER.len() + 2..])
            .collect();
        assert_eq!(injected, vec!["First.", "Striped."]);
    }

    #[test]
    fn repeated_injection_is_idempotent() {
        let hook = SkillHook::new(active_with(&[("code-review", "Review carefully.")]));
        let memory = memory_with("Static prompt.", "User.");

        inject(&hook, &memory);
        let messages = inject(&hook, &memory);

        let injected_count = messages
            .iter()
            .filter(|m| m.content.starts_with(SKILL_MARKER))
            .count();
        assert_eq!(injected_count, 1);
    }

    #[test]
    fn empty_active_set_drains_stale_injections() {
        let hook = SkillHook::new(active_with(&[]));
        let memory = memory_with("Static prompt.", "User.");
        // Simulate an injection from an earlier session.
        memory.write().unwrap().push(Message::new(
            Role::System,
            format!("{SKILL_MARKER}\n\nOld skill content."),
        ));

        let messages = inject(&hook, &memory);
        assert_eq!(messages.len(), 2); // static + user; stale [SKILL] drained
        assert!(!messages.iter().any(|m| m.content.starts_with(SKILL_MARKER)));
    }

    #[test]
    fn no_skills_does_not_modify_memory() {
        let hook = SkillHook::new(active_with(&[]));
        let memory = memory_with("Static prompt.", "User.");

        let messages = inject(&hook, &memory);
        assert_eq!(messages.len(), 2);
    }
}
