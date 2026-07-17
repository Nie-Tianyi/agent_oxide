//! [`SkillTool`] — lets the LLM load a named skill by injecting its
//! instructions as a System message.
//!
//! Follows the same pattern as [`EchoTool`](crate::tools::EchoTool):
//! define args struct → annotate with `#[tool()]` → implement `execute_stream`.
//!
//! On success, the skill's content is written to the shared [`ActiveSkills`]
//! state so [`SkillHook`](crate::hooks::SkillHook) picks it up on the next
//! `on_llm_start` and injects it into memory.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use skills::{ActiveSkills, SkillRegistry};

use tools::{ProgressStream, ToolError, tool};

/// Arguments for the `skill` tool.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SkillArgs {
    /// Name of the skill to load. Must match one of the available skills
    /// listed in the system prompt.
    #[schemars(
        description = "Name of the skill to load. Must match one of the available skills listed in the system prompt."
    )]
    pub name: String,
}

/// Load a named skill and inject its instructions as a System message.
///
/// When the LLM determines a task matches one of the available skills,
/// it calls this tool to load the skill's specialized instructions.
/// The content is also returned inline so the LLM can act on it immediately.
///
/// # Arguments
///
/// ```json
/// {"name": "my-skill"}
/// ```
///
/// # Errors
///
/// Returns `ToolError::InvalidArgs` if `name` is not a recognised skill.
#[tool(
    name = "skill",
    description = "Load a skill by name to inject specialized instructions as a \
         System message. Use this when a task matches one of the available \
         skills listed in the system prompt.\n\n\
         The skill's content provides domain-specific guidance — read it \
         carefully and follow its instructions.\n\n\
         When NOT to use: for general tasks that don't match any specific \
         skill. Skills are for specialized workflows.",
    args = SkillArgs
)]
pub struct SkillTool {
    /// Discovered skills registry — read-only lookup.
    registry: Arc<SkillRegistry>,
    /// Shared active-skills state — written here, read by [`SkillHook`].
    active: ActiveSkills,
}

impl SkillTool {
    pub fn new(registry: Arc<SkillRegistry>, active: ActiveSkills) -> Self {
        Self { registry, active }
    }

    fn execute_stream(&self, args: SkillArgs) -> Result<ProgressStream, ToolError> {
        let skill = self.registry.by_name(&args.name).ok_or_else(|| {
            let available = self.registry.names().join(", ");
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

        Ok(ProgressStream::done(skill.content.clone()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::RwLock;

    use super::*;
    use skills::SkillRegistry;
    use tools::Tool;

    #[test]
    fn test_name() {
        let tool = SkillTool::new(
            Arc::new(SkillRegistry::empty()),
            Arc::new(RwLock::new(HashMap::new())),
        );
        assert_eq!(tool.name(), "skill");
    }

    #[test]
    fn test_description() {
        let tool = SkillTool::new(
            Arc::new(SkillRegistry::empty()),
            Arc::new(RwLock::new(HashMap::new())),
        );
        assert!(tool.description().contains("Load a skill"));
    }

    #[test]
    fn test_schema() {
        let tool = SkillTool::new(
            Arc::new(SkillRegistry::empty()),
            Arc::new(RwLock::new(HashMap::new())),
        );
        let schema = tool.parameter_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["name"]["type"] == "string");
    }

    #[test]
    fn test_execute_unknown_skill_returns_error() {
        let tool = SkillTool::new(
            Arc::new(SkillRegistry::empty()),
            Arc::new(RwLock::new(HashMap::new())),
        );
        let err = Tool::execute_stream(&tool, r#"{"name":"nonexistent"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
        let msg = format!("{err:?}");
        assert!(msg.contains("nonexistent"));
    }

    #[test]
    fn test_execute_missing_name_field() {
        let tool = SkillTool::new(
            Arc::new(SkillRegistry::empty()),
            Arc::new(RwLock::new(HashMap::new())),
        );
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
