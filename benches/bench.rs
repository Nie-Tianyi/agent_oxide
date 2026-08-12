//! Micro-benchmarks for hot paths that do not need an LLM: memory
//! append/rendering and tool-registry operations.
//!
//! Run with `cargo bench` (or `cargo bench --no-run` to only check that
//! they compile).

use std::hint::black_box;
use std::sync::{Arc, RwLock};

use agent_oxide::{
    Memory, Message, ProgressStream, Role, SharedMemory, Tool, ToolError, ToolRegistry,
};
use criterion::{Criterion, criterion_group, criterion_main};

// ── Mock tool (no I/O, pure registry mechanics) ─────────────────────────────

struct MockTool {
    name: &'static str,
}

impl Tool for MockTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "A mock tool for benchmarking"
    }

    fn parameter_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    fn execute_stream(&self, _args: &str) -> Result<ProgressStream, ToolError> {
        Ok(ProgressStream::done("mock output".to_string()))
    }
}

// ── Memory ───────────────────────────────────────────────────────────────────

fn memory_append(c: &mut Criterion) {
    c.bench_function("memory/append_100", |b| {
        b.iter(|| {
            let mut mem = Memory::new();
            for i in 0..100 {
                mem.push(Message::new(Role::User, format!("message {i}")));
            }
            black_box(mem.len());
        });
    });
}

fn memory_render(c: &mut Criterion) {
    let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
    {
        let mut guard = mem.write().expect("lock poisoned");
        for i in 0..50 {
            guard.push(Message::new(
                if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                format!("a conversation message with some content and detail {i}"),
            ));
        }
    }
    c.bench_function("memory/to_context_vec_50", |b| {
        b.iter(|| black_box(mem.read().expect("lock poisoned").to_context_vec()));
    });
}

// ── Tool registry ─────────────────────────────────────────────────────────────

fn tool_registry_ops(c: &mut Criterion) {
    let names: Vec<String> = (0..50).map(|i| format!("tool_{i}")).collect();

    let mut registry = ToolRegistry::new();
    for name in &names {
        let name = Box::leak(name.clone().into_boxed_str());
        registry.register(Arc::new(MockTool { name }));
    }

    c.bench_function("tool_registry/get_50_tools", |b| {
        b.iter(|| {
            for name in &names {
                black_box(registry.get(name));
            }
        });
    });

    c.bench_function("tool_registry/register_50", |b| {
        b.iter(|| {
            let mut reg = ToolRegistry::new();
            for i in 0..50 {
                let name = Box::leak(format!("tool_{i}").into_boxed_str());
                reg.register(Arc::new(MockTool { name }));
            }
            black_box(reg.get("tool_25"));
        });
    });
}

criterion_group!(benches, memory_append, memory_render, tool_registry_ops);
criterion_main!(benches);
