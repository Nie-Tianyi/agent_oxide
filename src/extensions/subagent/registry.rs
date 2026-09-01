//! Subagent definition registry — discovery and wiring.
//!
//! [`SubagentRegistry`] discovers subagent definitions from Markdown files
//! on disk (same semantics as the skills registry); [`register_subagents`]
//! turns each definition into a [`SubagentTool`](super::tool::SubagentTool)
//! registered on an [`AgentBuilder`](crate::engine::AgentBuilder).

use std::path::PathBuf;

use crate::engine::AgentBuilder;
use crate::memory::SharedMemory;
use crate::provider::LLMClient;
use crate::tools::ToolRegistry;

use super::def::{SubagentDef, parse_subagent_file};
use super::tool::SubagentTool;

/// A read-only registry of discovered subagent definitions.
///
/// Created at startup via [`SubagentRegistry::discover`] and never mutated
/// afterwards. Lookups are O(n) linear search over a `Vec` — acceptable
/// because subagent counts are expected to stay small.
#[derive(Debug, Clone)]
pub struct SubagentRegistry {
    defs: Vec<SubagentDef>,
}

impl SubagentRegistry {
    /// Create an empty registry (no subagents).
    pub fn empty() -> Self {
        Self { defs: Vec::new() }
    }

    /// Discover subagent definitions by scanning `*.md` files in the given
    /// search paths (same semantics as `SkillRegistry::discover`).
    ///
    /// Directories are scanned in order; later paths **override** earlier
    /// ones when a definition with the same `name` is found. Missing
    /// directories are silently skipped; files that fail to parse are
    /// skipped with a `tracing::warn!` log.
    pub fn discover(search_paths: &[PathBuf]) -> Self {
        use std::collections::HashMap;

        let mut by_name: HashMap<String, SubagentDef> = HashMap::new();

        for dir in search_paths {
            let pattern = dir.join("*.md");
            let pattern_str = pattern.display().to_string();

            let paths = match glob::glob(&pattern_str) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        dir = %dir.display(),
                        error = %e,
                        "Invalid glob pattern for subagent discovery"
                    );
                    continue;
                }
            };

            for entry in paths {
                let path = match entry {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, "Glob walk error during subagent discovery");
                        continue;
                    }
                };

                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Cannot read subagent definition file"
                        );
                        continue;
                    }
                };

                match parse_subagent_file(&content) {
                    Ok(def) => {
                        tracing::info!(
                            name = %def.name,
                            path = %path.display(),
                            "Discovered subagent definition"
                        );
                        // Later entries overwrite earlier ones (project over user).
                        by_name.insert(def.name.clone(), def);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Failed to parse subagent definition file"
                        );
                    }
                }
            }
        }

        let mut defs: Vec<SubagentDef> = by_name.into_values().collect();
        // Sort by name for deterministic ordering.
        defs.sort_by(|a, b| a.name.cmp(&b.name));

        tracing::info!(count = defs.len(), "Subagent discovery complete");
        Self { defs }
    }

    /// Look up a definition by name.
    pub fn by_name(&self, name: &str) -> Option<&SubagentDef> {
        self.defs.iter().find(|d| d.name == name)
    }

    /// Return all discovered definitions.
    pub fn list(&self) -> &[SubagentDef] {
        &self.defs
    }

    /// Return just the definition names.
    pub fn names(&self) -> Vec<&str> {
        self.defs.iter().map(|d| d.name.as_str()).collect()
    }

    /// Whether no definitions were discovered.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// Register one [`SubagentTool`] per definition onto `builder` and return it.
///
/// # Recursion-safety contract
///
/// `parent_registry` must be the parent's tool set **without any subagent
/// tools** (the registry you build the parent's other tools in).  Each
/// child's tool set is filtered against exactly this registry, so no child
/// can reach another subagent — recursion is prevented by
/// construction.  Definitions whose name collides with an existing parent
/// tool are skipped with a warning (never clobbered).
pub fn register_subagents<C: LLMClient + Clone + 'static>(
    builder: AgentBuilder<C>,
    llm: C,
    registry: &SubagentRegistry,
    parent_registry: &ToolRegistry,
    parent_memory: SharedMemory,
    parent_model: &str,
) -> AgentBuilder<C> {
    let mut builder = builder;
    for def in select_defs(registry.list(), parent_registry) {
        builder = builder.tool(SubagentTool::new(
            llm.clone(),
            def.clone(),
            parent_registry,
            parent_memory.clone(),
            parent_model,
        ));
    }
    builder
}

/// Pure decision logic for which definitions to register — extracted from
/// [`register_subagents`] so it is testable without an LLM client.
///
/// - Definitions are deduplicated by name (first wins; `AgentBuilder::tool`
///   is last-wins, so this pre-dedupe keeps discovery's override semantics).
/// - A definition whose name collides with an existing parent tool is
///   **skipped** with a warning (never clobber a real tool).
/// - A definition whose `tools` list references another definition's name
///   gets a recursion warning (filtered away anyway by construction).
/// - A definition whose `tools` list references a name in neither the
///   parent registry nor the definition set gets a typo warning.
fn select_defs<'a>(
    defs: &'a [SubagentDef],
    parent_registry: &ToolRegistry,
) -> Vec<&'a SubagentDef> {
    use std::collections::HashSet;

    let def_names: HashSet<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut selected: Vec<&SubagentDef> = Vec::new();

    for def in defs {
        // Dedupe (first wins).
        if !seen.insert(def.name.as_str()) {
            continue;
        }
        // Never clobber an existing parent tool.
        if parent_registry.has(&def.name) {
            tracing::warn!(
                name = %def.name,
                "subagent definition skipped — a parent tool with this name already exists"
            );
            continue;
        }
        // Reference checks on def.tools.
        for t in &def.tools {
            if def_names.contains(t.as_str()) {
                tracing::warn!(
                    subagent = %def.name,
                    tool = %t,
                    "subagent definition references another subagent in tools — dropped by filtering"
                );
            } else if !parent_registry.has(t) {
                tracing::warn!(
                    subagent = %def.name,
                    tool = %t,
                    "subagent definition references a tool not present in the parent registry"
                );
            }
        }
        selected.push(def);
    }
    selected
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Agent;
    use crate::memory::Memory;
    use crate::provider::{CompletionRequest, CompletionResponse, LLMClient};
    use crate::tools::{ProgressStream, Tool, ToolError};
    use serde_json::Value;
    use std::sync::{Arc, Mutex, RwLock};

    fn def(name: &str, tools: &[&str]) -> SubagentDef {
        SubagentDef {
            name: name.into(),
            description: format!("Description of {name}."),
            model: None,
            tools: tools.iter().map(|s| s.to_string()).collect(),
            max_steps: None,
            max_retries: None,
            streaming: None,
            timeout_secs: None,
            inherit_context_messages: None,
            system_prompt: format!("You are {name}."),
        }
    }

    struct MockTool {
        name: &'static str,
    }

    impl Tool for MockTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "mock"
        }
        fn parameter_schema(&self) -> Value {
            serde_json::json!({})
        }
        fn execute_stream(&self, _args: &str) -> Result<ProgressStream, ToolError> {
            Ok(ProgressStream::done("ok".into()))
        }
    }

    fn registry_with(tools: &[&'static str]) -> ToolRegistry {
        let mut r = ToolRegistry::new();
        for name in tools {
            r.register(Arc::new(MockTool { name }));
        }
        r
    }

    /// Mock LLM client mirroring the engine's test style — calls are never
    /// reached in these tests, the agent is only built.
    #[derive(Clone)]
    struct MockClient {
        _calls: Arc<Mutex<Vec<String>>>,
    }

    impl LLMClient for MockClient {
        async fn generate(
            &self,
            _req: CompletionRequest,
        ) -> Result<CompletionResponse, crate::provider::ProviderError> {
            unreachable!("generate must not be called in registry tests")
        }

        async fn stream(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            futures_util::stream::BoxStream<
                'static,
                Result<crate::provider::StreamChunk, crate::provider::ProviderError>,
            >,
            crate::provider::ProviderError,
        > {
            unreachable!("stream must not be called in registry tests")
        }
    }

    // ── Registry ─────────────────────────────────────────────────────────────

    #[test]
    fn registry_empty() {
        let r = SubagentRegistry::empty();
        assert!(r.is_empty());
        assert!(r.list().is_empty());
        assert!(r.names().is_empty());
        assert!(r.by_name("anything").is_none());
    }

    #[test]
    fn discover_from_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("researcher.md"),
            "---\nname: researcher\ndescription: A research subagent.\n---\nResearch body.",
        )
        .unwrap();

        let r = SubagentRegistry::discover(&[tmp.path().to_path_buf()]);
        assert_eq!(r.list().len(), 1);
        let def = r.by_name("researcher").unwrap();
        assert_eq!(def.description, "A research subagent.");
        assert_eq!(def.system_prompt, "Research body.");
        assert_eq!(r.names(), vec!["researcher"]);
        assert!(!r.is_empty());
    }

    #[test]
    fn discover_missing_directory_ok() {
        let r = SubagentRegistry::discover(&[PathBuf::from("/nonexistent/dir/for/subagents")]);
        assert!(r.is_empty());
    }

    #[test]
    fn discover_invalid_file_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("valid.md"),
            "---\nname: valid\ndescription: OK.\n---\nContent.",
        )
        .unwrap();
        std::fs::write(tmp.path().join("invalid.md"), "No frontmatter here.").unwrap();

        let r = SubagentRegistry::discover(&[tmp.path().to_path_buf()]);
        assert_eq!(r.list().len(), 1);
        assert!(r.by_name("valid").is_some());
        assert!(r.by_name("invalid").is_none());
    }

    #[test]
    fn discover_later_path_overrides() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        std::fs::write(
            dir1.path().join("a.md"),
            "---\nname: a\ndescription: From dir1 (project).\n---\nContent 1.",
        )
        .unwrap();
        std::fs::write(
            dir2.path().join("a.md"),
            "---\nname: a\ndescription: From dir2 (user) — should win.\n---\nContent 2.",
        )
        .unwrap();

        let paths = vec![dir1.path().to_path_buf(), dir2.path().to_path_buf()];
        let r = SubagentRegistry::discover(&paths);
        assert_eq!(r.list().len(), 1);
        let def = r.by_name("a").unwrap();
        assert_eq!(def.description, "From dir2 (user) — should win.");
        assert_eq!(def.system_prompt, "Content 2.");
    }

    #[test]
    fn discover_sorted_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["zebra", "alpha", "mike"] {
            std::fs::write(
                tmp.path().join(format!("{name}.md")),
                format!("---\nname: {name}\ndescription: Desc {name}.\n---\nBody {name}."),
            )
            .unwrap();
        }

        let r = SubagentRegistry::discover(&[tmp.path().to_path_buf()]);
        let names: Vec<&str> = r.list().iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mike", "zebra"]);
    }

    // ── select_defs ──────────────────────────────────────────────────────────

    #[test]
    fn select_defs_keeps_non_colliding() {
        let parent = registry_with(&["read", "grep"]);
        let defs = vec![def("researcher", &["read", "grep"])];
        let selected = select_defs(&defs, &parent);
        assert_eq!(selected.len(), 1);
    }

    #[test]
    fn select_defs_skips_collision_with_parent_tool() {
        let parent = registry_with(&["read", "grep", "researcher"]);
        let defs = vec![def("researcher", &["read"])];
        let selected = select_defs(&defs, &parent);
        assert!(selected.is_empty());
    }

    #[test]
    fn select_defs_deduplicates_duplicate_names() {
        let parent = registry_with(&["read"]);
        let defs = vec![def("a", &["read"]), def("a", &["read"])];
        let selected = select_defs(&defs, &parent);
        assert_eq!(selected.len(), 1);
    }

    #[test]
    fn select_defs_warns_on_unknown_tool_reference_but_keeps_def() {
        // Referencing a missing tool warns but does not exclude the def —
        // filtering drops the missing name at construction time.
        let parent = registry_with(&["read"]);
        let defs = vec![def("a", &["read", "missing"])];
        let selected = select_defs(&defs, &parent);
        assert_eq!(selected.len(), 1);
    }

    // ── register_subagents ───────────────────────────────────────────────────

    #[test]
    fn register_subagents_builds_agent_smoke() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("researcher.md"),
            "---\nname: researcher\ndescription: A research subagent.\ntools: [read]\n---\nResearch body.",
        )
        .unwrap();

        let registry = SubagentRegistry::discover(&[tmp.path().to_path_buf()]);
        let parent_registry = registry_with(&["read", "grep"]);
        let memory = Arc::new(RwLock::new(Memory::new()));

        // Building must succeed: each def is wired as a tool on the
        // parent builder without clobbering existing tools.
        let _agent = register_subagents(
            Agent::builder(
                MockClient {
                    _calls: Arc::new(Mutex::new(vec![])),
                },
                "parent-model",
            ),
            MockClient {
                _calls: Arc::new(Mutex::new(vec![])),
            },
            &registry,
            &parent_registry,
            memory,
            "parent-model",
        )
        .build();
    }
}
