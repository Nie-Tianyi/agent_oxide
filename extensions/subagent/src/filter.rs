use std::sync::Arc;

use tools::ToolRegistry;

/// Build a subagent-safe tool registry by selecting a subset of tools
/// from the parent's full registry.
///
/// Each allowed tool name must match a tool registered in `source`.
/// Missing names are silently skipped so the parent can add or remove
/// tools without breaking the subagent configuration.
pub fn filter_tools(source: &ToolRegistry, allowed: &[&str]) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for name in allowed {
        if let Some(tool) = source.get(name) {
            registry.register(Arc::clone(tool));
        }
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::Arc;
    use tools::{ProgressStream, Tool, ToolError};

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

    #[test]
    fn filters_to_allowed_names() {
        let mut source = ToolRegistry::new();
        source.register(Arc::new(MockTool { name: "read" }));
        source.register(Arc::new(MockTool { name: "write" }));
        source.register(Arc::new(MockTool { name: "grep" }));

        let filtered = filter_tools(&source, &["read", "grep"]);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.has("read"));
        assert!(filtered.has("grep"));
        assert!(!filtered.has("write"));
    }

    #[test]
    fn missing_names_are_skipped() {
        let mut source = ToolRegistry::new();
        source.register(Arc::new(MockTool { name: "read" }));

        let filtered = filter_tools(&source, &["read", "nonexistent"]);
        assert_eq!(filtered.len(), 1);
        assert!(filtered.has("read"));
    }

    #[test]
    fn empty_allowed_returns_empty_registry() {
        let mut source = ToolRegistry::new();
        source.register(Arc::new(MockTool { name: "read" }));

        let filtered = filter_tools(&source, &[]);
        assert!(filtered.is_empty());
    }
}
