//! FeedbackAgent — the NVIDIA OO Agents quickstart/01 equivalent, written
//! in the same style and compiled onto the Agent Oxide `core/` API by the
//! `agent-macros` proc macros.
//!
//! ```text
//! cargo run -p agent-kit --example feedback_agent
//! ```
//!
//! The blueprint half (system prompt, tool registration, args schema,
//! tool execution) is always validated, no API key needed. The live LLM
//! calls only run when `DEEPSEEK_API` is set.

use agent_kit::schemars::JsonSchema;
use agent_kit::serde::{Deserialize, Serialize};
use agent_macros::{Agent, agent_impl};
use deepseek::DeepSeekClient;

// ── The agent ────────────────────────────────────────────────────────────────

/// 你是一个专门分析客户反馈的 Agent。
///
/// 你擅长从用户的反馈文本中提取关键信息，并用简洁的语言总结。
/// （这个 doc comment 会被 `#[derive(Agent)]` 自动用作 System Prompt。）
#[derive(Clone, Agent)]
struct FeedbackAgent {
    /// LLM 客户端（`#[agent(client)]` 标记，宏自动发现）。
    #[agent(client)]
    client: DeepSeekClient,
}

#[agent_impl]
impl FeedbackAgent {
    /// 分析客户反馈的情感和关键主题，用一句话总结。
    /// （空 body 的 async 方法 = generation method，由 LLM 在运行时实现。）
    async fn analyze_feedback(&self, text: String) -> String {}

    /// 对文本做情感分类：正面、负面或中性，并给出置信度分数。
    /// （`#[strategy(predict)]` = 单次 LLM 调用，不暴露工具。）
    #[strategy(predict)]
    async fn classify_sentiment(&self, text: String) -> Sentiment {}

    /// 记录一条补充反馈（同步方法 → 自动成为 LLM 可调用的工具）。
    fn add_note(&self, note: String) -> String {
        format!("note recorded: {note}")
    }
}

/// 结构化输出 —— 与 Python 版本的 Pydantic 返回类型对应。
/// 自动验证 + 解析失败时自动重试。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "agent_kit::serde")]
#[schemars(crate = "agent_kit::schemars")]
struct Sentiment {
    label: String,
    score: f64,
}

// ── Blueprint validation (no LLM required) ───────────────────────────────────

fn validate_blueprint(agent: &FeedbackAgent) {
    // 1. System prompt comes from the struct doc comment.
    let system = agent.agent_system_prompt();
    assert!(system.contains("反馈"), "system prompt from doc comment");

    // 2. `add_note` was auto-registered as a tool (name = method name,
    //    description = method doc comment).
    let mut registry = agent_kit::tools::ToolRegistry::new();
    agent_kit::AgentBlueprint::blueprint_register_tools(agent, &mut registry);
    assert!(registry.has("add_note"), "sync method registered as tool");
    let tool = registry.get("add_note").expect("tool present");
    assert!(tool.description().contains("补充反馈"));

    // 3. JSON Schema is derived from the parameter list.
    let schema = tool.parameter_schema();
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["note"].is_object());

    // 4. Tool execution delegates to the user's method.
    let stream = tool
        .execute_stream(r#"{"note": "hello"}"#)
        .expect("execution starts");
    let mut stream = Box::pin(stream);
    let first = futures_executor::block_on(futures_util::StreamExt::next(&mut stream));
    match first {
        Some(agent_kit::tools::Progress::Done(out)) => {
            assert!(out.contains("note recorded: hello"));
        }
        other => panic!("expected Progress::Done, got {other:?}"),
    }

    // 5. Generation methods were wrapped in Result and carry the strategy.
    //    (Their bodies run the LLM, so they are only exercised live below.)

    println!("[ok] blueprint: system prompt, tool registration, args schema, execution");
}

// ── Live LLM calls (DEEPSEEK_API required) ───────────────────────────────────

async fn live_calls(agent: &FeedbackAgent) -> Result<(), Box<dyn std::error::Error>> {
    let analysis = agent
        .analyze_feedback("产品很棒，但物流太慢了。".to_string())
        .await?;
    println!("[llm] analyze_feedback: {analysis}");

    let sentiment = agent
        .classify_sentiment("I absolutely love this product!".to_string())
        .await?;
    println!("[llm] classify_sentiment: {sentiment:?}");
    assert!(
        matches!(sentiment.label.as_str(), "positive" | "正面"),
        "label: {}",
        sentiment.label
    );

    Ok(())
}

// ── Entry ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = match std::env::var("DEEPSEEK_API") {
        Ok(key) => key,
        Err(_) => {
            println!("DEEPSEEK_API not set — running blueprint validation only.");
            let agent = FeedbackAgent {
                client: DeepSeekClient::new("sk-test"),
            };
            validate_blueprint(&agent);
            println!("example finished (skip live LLM — set DEEPSEEK_API to run it)");
            return Ok(());
        }
    };

    let agent = FeedbackAgent {
        client: DeepSeekClient::new(api_key),
    };
    validate_blueprint(&agent);
    live_calls(&agent).await?;

    // into_agent: assemble a core::engine::Agent for the existing TUI / tooling.
    let core_agent = agent.clone().into_agent("deepseek-v4-pro")?;
    let mem = core_agent.memory().read().expect("lock");
    assert!(
        mem.to_context_vec()
            .iter()
            .any(|m| m.content.contains("反馈")),
        "core agent seeded with the struct's system prompt"
    );
    println!("[ok] into_agent: core::engine::Agent assembled with system prompt seeded");
    println!("example finished");
    Ok(())
}
