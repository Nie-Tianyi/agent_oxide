//! NoteTakingAgent — the `agent.context["notes"]` equivalent (Phase 4).
//!
//! `#[context(dynamic)]` on a field makes it a context block: rendered
//! into the system prompt before every LLM call. Because the block
//! captures a clone of the field, shared types (`Arc<RwLock<...>>`) stay
//! live — the notes updated by the `add_note` tool are visible to the
//! next `answer()` call, both through the full-agent hook pipeline and
//! through generation methods (which inline the rendered blocks).
//!
//! ```text
//! cargo run -p agent-kit --example note_taking_agent
//! ```
//! (requires `DEEPSEEK_API`; blueprint checks run without it)

use agent_kit::schemars::JsonSchema;
use agent_kit::serde::{Deserialize, Serialize};
use agent_macros::{Agent, agent_impl};
use deepseek::DeepSeekClient;

/// 你是一个笔记 Agent。
///
/// 你的笔记以 `[CONTEXT:notes]` 的形式提供。回答用户问题时，先阅读当前所有笔记，
/// 基于笔记内容回答；不要在笔记之外编造信息。
#[derive(Clone, Agent)]
struct NoteTakingAgent {
    #[agent(client)]
    client: DeepSeekClient,
    /// 动态上下文：每次 LLM 调用前重新渲染（Arc<RwLock> 保持共享）。
    #[context(dynamic)]
    notes: std::sync::Arc<std::sync::RwLock<Vec<String>>>,
}

#[agent_impl]
impl NoteTakingAgent {
    /// 添加一条笔记。
    fn add_note(&self, text: String) {
        self.notes.write().expect("lock").push(text);
    }

    /// 根据当前所有笔记回答用户的问题。
    async fn answer(&self, question: String) -> String {}
}

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(crate = "agent_kit::serde")]
#[schemars(crate = "agent_kit::schemars")]
struct NoteCount {
    count: i32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = match std::env::var("DEEPSEEK_API") {
        Ok(key) => key,
        Err(_) => {
            println!("DEEPSEEK_API not set — blueprint checks only.");
            validate_context(&NoteTakingAgent {
                client: DeepSeekClient::new("sk-test"),
                notes: std::sync::Arc::new(std::sync::RwLock::new(vec![
                    "buy milk".to_string(),
                    "call bob".to_string(),
                ])),
            });
            println!("example finished (set DEEPSEEK_API to run the live note check)");
            return Ok(());
        }
    };

    let agent = NoteTakingAgent {
        client: DeepSeekClient::new(api_key),
        notes: std::sync::Arc::new(std::sync::RwLock::new(Vec::new())),
    };
    // Adds the notes through the `add_note` tool, then checks the
    // hook pipeline / context prompt pick them up.
    validate_context(&agent);

    // Live: ask — the notes added through the tool must be visible.
    let answer = agent
        .answer("我们目前有几条笔记？分别是什么？".to_string())
        .await?;
    println!("[llm] answer: {answer}");
    assert!(
        answer.contains("milk") || answer.contains("牛奶"),
        "answer must reference the notes, got: {answer}"
    );
    assert!(
        answer.contains("bob") || answer.contains("鲍勃") || answer.contains("Bob"),
        "answer must reference both notes, got: {answer}"
    );

    // Structured-output variant of the same question.
    let count = agent
        .answer_count("我们目前有几条笔记？".to_string())
        .await?;
    println!("[llm] answer_count: {count:?}");
    assert_eq!(
        count.count, 2,
        "notes must be visible via the context block"
    );

    println!("[ok] live: dynamic context block kept generation methods in sync");
    println!("example finished");
    Ok(())
}

/// Blueprint-level checks that need no LLM: the context hook renders the
/// notes field, and `agent_context_prompt()` inlines them for generation
/// methods.
fn validate_context(agent: &NoteTakingAgent) {
    // 0. Add notes through the tool — proves the tool writes are visible
    // to both the hook pipeline and `agent_context_prompt()`.
    agent.add_note("buy milk".to_string());
    agent.add_note("call bob".to_string());

    // 1. agent_context_prompt (used by generation methods).
    let prompt = agent.agent_context_prompt();
    assert!(
        prompt.contains("[CONTEXT:notes]"),
        "context marker present, got: {prompt}"
    );

    // 2. The hook pipeline (used by full agent runs) injects the block.
    let hooks = agent_kit::AgentBlueprint::blueprint_context_hooks(agent);
    assert_eq!(hooks.len(), 1, "one hook for the notes field");
    let mem = std::sync::Arc::new(std::sync::RwLock::new(agent_kit::memory::Memory::new()));
    hooks[0].on_llm_start("session", &mem);
    let msgs = mem.read().expect("lock").to_context_vec();
    let injected = msgs
        .iter()
        .find(|m| m.content.starts_with("[CONTEXT:notes]"))
        .expect("context block injected");
    assert!(injected.content.contains("buy milk"), "notes rendered");
    assert!(injected.content.contains("call bob"));

    println!("[ok] blueprint: context block renders in hook pipeline and context prompt");
}

impl NoteTakingAgent {
    /// 数一下当前有多少条笔记，返回数字。
    async fn answer_count(
        &self,
        question: String,
    ) -> Result<NoteCount, agent_kit::GenerationError> {
        // Manual generation-method body (written by hand to prove the
        // runtime call is all the macro emits) — see the #[agent_impl]
        // expansion in the README for the generated equivalent.
        let prompt = format!("{question}\n\nArguments:\n- question: {:?}", question);
        let system = format!(
            "{}{}",
            self.agent_system_prompt(),
            self.agent_context_prompt()
        );
        agent_kit::run_generation::<_, NoteCount>(
            self.agent_client(),
            &self.agent_model(),
            &system,
            &prompt,
            None,
            &agent_kit::Strategy::Predict { max_retries: 2 },
        )
        .await
    }
}
