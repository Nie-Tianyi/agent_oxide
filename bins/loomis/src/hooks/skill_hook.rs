//! Hook that maintains `[SKILL: ...]` System messages in memory.
//!
//! Fires in [`on_llm_start`](engine::AgentHook::on_llm_start), which runs
//! **after** all tool results have been written to memory. This avoids the
//! API message-ordering constraint: an assistant message with `tool_calls`
//! must be immediately followed by tool-result messages — a System message
//! injected during tool execution would break that ordering.
//!
//! The hook reads the shared [`ActiveSkills`](skills::ActiveSkills) state and
//! ensures exactly one `[SKILL: name]` System message exists per active skill,
//! updating in-place when the set changes.
//!
//! Follows the same pattern as [`TodoListHook`](crate::hooks::TodoListHook)
//! and [`PlanModeHook`](crate::hooks::PlanModeHook).

use engine::AgentHook;
use memory::SharedMemory;
use provider::{Message, Role};
use skills::ActiveSkills;

/// Marker prefix for injected skill System messages.
///
/// Follows the same convention as
/// [`PLAN_MODE_MARKER`](crate::hooks::PlanModeHook) and
/// [`TODO_MARKER`](crate::tools::TODO_MARKER).
const SKILL_MARKER_PREFIX: &str = "[SKILL:";

/// Hook that maintains `[SKILL: ...]` System messages from the shared
/// [`ActiveSkills`] state.
///
/// The hook is stateless — it always derives the System messages from the
/// current active-skills set, so it's safe across `/new`, thread resume,
/// and compaction.
pub struct SkillHook {
    /// Shared active skills — read every `on_llm_start`.
    active: ActiveSkills,
}

impl SkillHook {
    pub fn new(active: ActiveSkills) -> Self {
        Self { active }
    }
}

impl AgentHook for SkillHook {
    /// Synchronise `[SKILL: ...]` System messages with the current active-skills set.
    ///
    /// Called by the agent loop **before** building the context vector for
    /// the next LLM call — all tool results from the previous step are
    /// already committed to memory, so inserting System messages here does
    /// not violate any API ordering constraint.
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        // Clone under lock to minimize contention.
        let active = match self.active.read() {
            Ok(a) => a.clone(),
            Err(_) => return,
        };

        let mut mem = match memory.write() {
            Ok(m) => m,
            Err(_) => return,
        };

        // Remove all existing [SKILL: ...] System messages.
        mem.messages
            .retain(|m| !(m.role == Role::System && m.content.starts_with(SKILL_MARKER_PREFIX)));

        // Insert one System message per active skill at index 0.
        // Iterate in reverse so skills inserted first appear first in memory.
        for (name, content) in active.iter() {
            let msg = format!("{SKILL_MARKER_PREFIX} {name}]\n\n{content}");
            mem.messages.insert(0, Message::new(Role::System, msg));
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    use super::*;
    use memory::Memory;

    fn make_active_skills(map: HashMap<String, String>) -> ActiveSkills {
        Arc::new(RwLock::new(map))
    }

    fn make_memory() -> SharedMemory {
        Arc::new(RwLock::new(Memory::new()))
    }

    #[test]
    fn test_injects_skill_message_when_active() {
        let mut map = HashMap::new();
        map.insert("my-skill".into(), "Skill instructions.".into());
        let active = make_active_skills(map);
        let memory = make_memory();
        let hook = SkillHook::new(active);

        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let skill_msg = mem
            .messages
            .iter()
            .find(|m| m.role == Role::System && m.content.starts_with(SKILL_MARKER_PREFIX));
        assert!(skill_msg.is_some(), "expected [SKILL: ...] System message");
        let content = &skill_msg.unwrap().content;
        assert!(
            content.contains("[SKILL: my-skill]"),
            "should contain marker with skill name"
        );
        assert!(
            content.contains("Skill instructions."),
            "should contain skill content"
        );
    }

    #[test]
    fn test_removes_skill_message_when_empty() {
        let active = make_active_skills(HashMap::new());
        let memory = make_memory();

        // Pre-seed with a [SKILL: ...] message.
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(
                Role::System,
                format!("{SKILL_MARKER_PREFIX} old-skill]\n\nOld content."),
            ));
        }

        let hook = SkillHook::new(active);
        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let skill_count = mem
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.content.starts_with(SKILL_MARKER_PREFIX))
            .count();
        assert_eq!(skill_count, 0, "[SKILL: ...] messages should be removed");
    }

    #[test]
    fn test_replaces_existing_skill_message() {
        let mut map = HashMap::new();
        map.insert("my-skill".into(), "Updated content.".into());
        let active = make_active_skills(map);
        let memory = make_memory();

        // Pre-seed with an old [SKILL: my-skill] message.
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(
                Role::System,
                format!("{SKILL_MARKER_PREFIX} my-skill]\n\nOld content."),
            ));
        }

        let hook = SkillHook::new(active);
        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let skill_count = mem
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.content.starts_with(SKILL_MARKER_PREFIX))
            .count();
        assert_eq!(
            skill_count, 1,
            "should still have exactly one [SKILL: ...] message"
        );

        let skill_msg = mem
            .messages
            .iter()
            .find(|m| m.role == Role::System && m.content.starts_with(SKILL_MARKER_PREFIX))
            .unwrap();
        assert!(
            skill_msg.content.contains("Updated content."),
            "should contain new content, got: {}",
            skill_msg.content
        );
        assert!(
            !skill_msg.content.contains("Old content"),
            "should NOT contain old content"
        );
    }

    #[test]
    fn test_preserves_non_skill_system_messages() {
        let mut map = HashMap::new();
        map.insert("my-skill".into(), "Skill content.".into());
        let active = make_active_skills(map);
        let memory = make_memory();

        // Pre-seed with a regular System message.
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(Role::System, "Normal system prompt"));
        }

        let hook = SkillHook::new(active);
        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        assert!(
            mem.messages
                .iter()
                .any(|m| m.role == Role::System && m.content == "Normal system prompt"),
            "non-skill System message should be preserved"
        );
    }

    #[test]
    fn test_multiple_active_skills() {
        let mut map = HashMap::new();
        map.insert("skill-a".into(), "Content A.".into());
        map.insert("skill-b".into(), "Content B.".into());
        let active = make_active_skills(map);
        let memory = make_memory();
        let hook = SkillHook::new(active);

        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let skill_count = mem
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.content.starts_with(SKILL_MARKER_PREFIX))
            .count();
        assert_eq!(skill_count, 2, "expected two [SKILL: ...] messages");

        assert!(
            mem.messages
                .iter()
                .any(|m| m.content.contains("[SKILL: skill-a]")),
            "should contain skill-a"
        );
        assert!(
            mem.messages
                .iter()
                .any(|m| m.content.contains("[SKILL: skill-b]")),
            "should contain skill-b"
        );
    }
}
