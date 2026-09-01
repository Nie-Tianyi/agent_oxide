//! SkillTool — lets the LLM load a named skill by injecting its
//! instructions as a System message.
//!
//! Follows the same pattern as [`SubagentTool`](crate::subagent::SubagentTool):
//! define args struct → hand-write the [`Tool`](crate::tools::Tool) impl →
//! implement `execute_stream`.
//!
//! On success, the skill's content is written to the shared
//! [`ActiveSkills`](crate::skills::ActiveSkills) state so
//! [`SkillHook`](crate::skills::SkillHook) picks it up on the next
//! `on_llm_start` and injects it into memory.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::skills::{ActiveSkills, SkillRegistry};
use crate::tools::{ProgressStream, Tool, ToolError, generate_schema};

/// Description shown to the LLM for the `skill` tool.
const SKILL_TOOL_DESCRIPTION: &str = "\
Load a skill by name to inject specialized instructions as a System \
message. Use this when a task matches one of the available skills listed \
in the system prompt.\n\n\
The skill's content provides domain-specific guidance — read it carefully \
and follow its instructions.\n\n\
When NOT to use: for general tasks that don't match any specific skill. \
Skills are for specialized workflows.";

/// Arguments for the `skill` tool.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillArgs {
    /// Name of the skill to load. Must match one of the available skills
    /// listed in the system prompt.
    #[schemars(
        description = "Name of the skill to load. Must match one of the available skills listed in the system prompt."
    )]
    name: String,
}

/// Load a named skill and inject its instructions as a System message.
///
/// When the LLM determines a task matches one of the available skills,
/// it calls this tool to load the skill's specialized instructions.
/// The content is also returned inline so the LLM can act on it immediately.
///
/// This is the [`Tool`] impl the `#[tool(...)]` macro would generate; it is
/// written by hand because the macro's generated code references
/// `agent_oxide::` paths that cannot resolve inside the crate itself.
pub struct SkillTool {
    /// Discovered skills registry — read-only lookup.
    registry: Arc<SkillRegistry>,
    /// Shared active-skills state — written here, read by
    /// [`SkillHook`](crate::skills::SkillHook).
    active: ActiveSkills,
}

impl SkillTool {
    /// Create a new skill-loading tool.
    ///
    /// * `registry` — The discovered [`SkillRegistry`] (read-only).
    /// * `active` — The shared [`ActiveSkills`] state the tool writes to;
    ///   pass the same `Arc` to [`SkillHook`](crate::skills::SkillHook).
    pub fn new(registry: Arc<SkillRegistry>, active: ActiveSkills) -> Self {
        Self { registry, active }
    }

    fn execute_stream(&self, args: SkillArgs) -> Result<ProgressStream, ToolError> {
        let skill = self.registry.by_name(&args.name).ok_or_else(|| {
            let available = self.registry.names().join(", ");
            tracing::warn!(name = %args.name, "Unknown skill requested");
            ToolError::InvalidArgs(format!(
                "Unknown skill '{}'. Available: [{}]",
                args.name, available
            ))
        })?;

        // Write to active-skills state so SkillHook maintains it in memory.
        {
            let mut active = self
                .active
                .write()
                .map_err(|_| ToolError::Execution("active skills lock poisoned".into()))?;
            active.insert(skill.name.clone(), skill.content.clone());
        }

        tracing::info!(name = %skill.name, "Skill activated");
        Ok(ProgressStream::done(skill.content.clone()))
    }
}

impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        SKILL_TOOL_DESCRIPTION
    }

    fn parameter_schema(&self) -> serde_json::Value {
        static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        SCHEMA.get_or_init(generate_schema::<SkillArgs>).clone()
    }

    fn execute_stream(&self, raw_args: &str) -> Result<ProgressStream, ToolError> {
        let args: SkillArgs = serde_json::from_str(raw_args)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid args: {e}")))?;
        SkillTool::execute_stream(self, args)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::RwLock;

    use super::*;

    fn tool_with_empty() -> SkillTool {
        SkillTool::new(
            Arc::new(SkillRegistry::empty()),
            Arc::new(RwLock::new(HashMap::new())),
        )
    }

    #[test]
    fn test_name() {
        let tool = tool_with_empty();
        assert_eq!(tool.name(), "skill");
    }

    #[test]
    fn test_description() {
        let tool = tool_with_empty();
        assert!(tool.description().contains("Load a skill"));
    }

    #[test]
    fn test_schema() {
        let tool = tool_with_empty();
        let schema = tool.parameter_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["name"]["type"] == "string");
    }

    #[test]
    fn test_execute_unknown_skill_returns_error() {
        let tool = tool_with_empty();
        let err = Tool::execute_stream(&tool, r#"{"name":"nonexistent"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
        let msg = format!("{err:?}");
        assert!(msg.contains("nonexistent"));
    }

    #[test]
    fn test_execute_missing_name_field() {
        let tool = tool_with_empty();
        let err = Tool::execute_stream(&tool, r#"{}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_execute_updates_active_skills() {
        // Build a registry with one skill via discovery from a temp dir.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("test-skill.md"),
            "---\nname: test-skill\ndescription: A test skill.\n---\nSkill content here.",
        )
        .unwrap();
        let paths = vec![tmp.path().to_path_buf()];
        let registry = Arc::new(SkillRegistry::discover(&paths));
        let active: ActiveSkills = Arc::new(RwLock::new(HashMap::new()));

        let tool = SkillTool::new(registry, active.clone());
        let result = Tool::execute_stream(&tool, r#"{"name":"test-skill"}"#)
            .unwrap()
            .poll_done();
        assert_eq!(result, "Skill content here.");

        // Check ActiveSkills side effect.
        let active = active.read().unwrap();
        assert_eq!(active.get("test-skill").unwrap(), "Skill content here.");
    }
}
