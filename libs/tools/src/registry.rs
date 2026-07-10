use std::collections::HashMap;
use std::sync::Arc;

use provider::ToolDef;

use super::ToolError;
use super::tool::Tool;

/// Registry of named tools. Thread-safe via `Arc<dyn Tool>`.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool, keyed by [`Tool::name()`].
    /// Replaces any existing tool with the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_owned(), tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Check whether a tool with the given name is registered.
    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Iterate over all registered tools as `(name, tool)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<dyn Tool>)> {
        self.tools.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Convert all registered tools to [`ToolDef`]s for API requests.
    pub fn to_tool_defs(&self) -> Vec<ToolDef> {
        self.tools.values().map(|t| t.as_ref().to_def()).collect()
    }

    /// Dispatch execution by name, returning a progress stream.
    ///
    /// - `None` — no tool found with that name.
    /// - `Some(Ok(stream))` — tool is executing; pull [`Progress`] events from the stream.
    /// - `Some(Err(e))` — tool failed to start.
    pub fn execute_stream(
        &self,
        name: &str,
        args: &str,
    ) -> Option<Result<crate::ProgressStream, ToolError>> {
        self.tools.get(name).map(|tool| tool.execute_stream(args))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert any `&dyn Tool` to a [`ToolDef`].
pub fn tool_to_def(tool: &dyn Tool) -> ToolDef {
    tool.to_def()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Progress;
    use crate::ProgressStream;
    use futures_util::StreamExt;
    use serde_json::Value;

    /// Minimal mock tool for testing the registry.
    struct MockTool {
        name: &'static str,
    }

    impl Tool for MockTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "mock tool for testing"
        }

        fn parameters(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        fn execute_stream(&self, _args: &str) -> Result<ProgressStream, ToolError> {
            Ok(ProgressStream::done("mock result".into()))
        }
    }

    #[test]
    fn test_new_is_empty() {
        let r = ToolRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn test_default_is_empty() {
        let r = ToolRegistry::default();
        assert!(r.is_empty());
    }

    #[test]
    fn test_register_and_get() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(MockTool { name: "mock" }));
        assert!(r.has("mock"));
        assert!(r.get("mock").is_some());
        assert!(r.get("nonexistent").is_none());
    }

    #[test]
    fn test_register_replaces_existing() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(MockTool { name: "mock" }));
        r.register(Arc::new(MockTool { name: "mock" }));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn test_len_tracks_registrations() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(MockTool { name: "a" }));
        r.register(Arc::new(MockTool { name: "b" }));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn test_execute_success() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(MockTool { name: "mock" }));
        let Some(Ok(mut stream)) = r.execute_stream("mock", "{}") else {
            panic!("expected Some(Ok(_))");
        };
        // Pull the single Done event.
        let progress = futures_executor::block_on(stream.next());
        match progress {
            Some(Progress::Done(output)) => assert_eq!(output, "mock result"),
            other => panic!("expected Progress::Done, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_missing_tool() {
        let r = ToolRegistry::new();
        assert!(r.execute_stream("nonexistent", "{}").is_none());
    }

    #[test]
    fn test_to_tool_defs() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(MockTool { name: "a" }));
        r.register(Arc::new(MockTool { name: "b" }));
        let defs = r.to_tool_defs();
        assert_eq!(defs.len(), 2);
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn test_tool_to_def_free_function() {
        let tool = MockTool { name: "mock" };
        let def = tool_to_def(&tool as &dyn Tool);
        assert_eq!(def.function.name, "mock");
    }

    #[test]
    fn test_iter_yields_all() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(MockTool { name: "a" }));
        r.register(Arc::new(MockTool { name: "b" }));
        let names: Vec<&str> = r.iter().map(|(n, _)| n).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }
}
