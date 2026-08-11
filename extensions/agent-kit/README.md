# agent-kit — NVIDIA OO Agents 范式（映射到 Agent Oxide core）

将 NVIDIA 开源的 OO Agents 编程范式（类文档 = System Prompt、同步方法 = 工具、
异步方法 = LLM 生成、Pydantic 返回 = 结构化输出、`@strategy`、context blocks）
映射到本项目 `core/` 已有的能力上。**`core/` 零改动** — 所有新代码都在
`extensions/` 下，宏展开后等价于手写 `core/` API 调用。

| Python 版 | Rust 版（本 crate） | 语义 |
|-----------|---------------------|------|
| `class X(Agent, llm=llm)` + 类文档 | `#[derive(Agent)] struct X` + `/// doc` | 定义 Agent；doc = System Prompt |
| `def method(self, ...)` | `fn method(&self, ...) { ... }`（`#[agent_impl]` 块内） | 同步方法 → 自动注册为工具 |
| `async def method(...) -> Ret: ...` | `async fn method(&self, ...) -> Ret {}`（空 body） | 生成方法 → LLM 实现 |
| `@strategy(PredictStrategy())` | `#[strategy(predict)]` | 单次 LLM 调用，不暴露工具 |
| `@strategy(CodeActStrategy(...))` | `#[strategy(code_act, max_iterations = 10)]` | 完整 ReAct 循环（默认 50 轮） |
| `-> FeedbackAnalysis`（Pydantic） | `-> FeedbackAnalysis`（Deserialize + JsonSchema） | 结构化输出 + 自动验证重试 |
| `shell = ShellTools()`（外部工具字段） | `#[tool]` 字段（实现 `Tool` + `Clone`） | 外部工具自动注册 |
| `agent.context["notes"] = Context(...)` | `#[context(dynamic)]` 字段 | 动态上下文，每次 LLM 调用前重渲染 |

## 完整示例

```rust
use agent_kit::schemars::JsonSchema;
use agent_kit::serde::{Deserialize, Serialize};
use agent_macros::{Agent, agent_impl};
use deepseek::DeepSeekClient;

/// 你是一个库存管理 Agent。回答订单问题时，必须先调用工具查询实际数据。
#[derive(Clone, Agent)]
struct InventoryAgent {
    #[agent(client)]
    client: DeepSeekClient,
    inventory: HashMap<String, Item>,
}

#[agent_impl]
impl InventoryAgent {
    /// 获取指定物品的当前库存数量。
    fn get_stock(&self, item: String) -> i32 { /* ... */ }

    /// 获取指定物品的当前单价。
    fn get_price(&self, item: String) -> f64 { /* ... */ }

    /// 检查订单是否可以在预算内完成。
    #[strategy(code_act, max_iterations = 15)]
    async fn can_fulfill_order(&self, items: Vec<String>, budget: f64) -> OrderResult {}
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(crate = "agent_kit::serde")]
#[schemars(crate = "agent_kit::schemars")]
struct OrderResult { can_fulfill: bool, total_cost: f64, unavailable_items: Vec<String> }
```

使用（示例见 `examples/`）：

```rust
let agent = InventoryAgent { client: DeepSeekClient::new(api_key), /* ... */ };
let r = agent.can_fulfill_order(vec!["apple".into()], 10.0).await?;  // 生成方法
agent.get_stock("apple".into());          // 原方法保留，Rust 直接调用
// 或组装成 core 引擎的完整 Agent（带 hook 管线）：
let engine_agent: engine::Agent<DeepSeekClient> = agent.clone().into_agent("deepseek-v4-pro")?;
```

## 宏展开（一句话版）

- **`#[derive(Agent)]`**：扫描 struct —— 收集 doc（System Prompt）、
  `#[agent(client)]`/`#[agent(model)]` 字段、`#[tool]` 字段、`#[context(...)]` 字段；
  生成 `agent_client()` / `agent_model()` / `agent_system_prompt()` /
  `agent_context_prompt()` / `into_agent()` 等固有方法，以及唯一一个
  `AgentBlueprint` trait impl（field 半边：System Prompt、字段工具注册、context hook）。
- **`#[agent_impl]`**：处理方法块 —— 同步 `fn` 生成 `Tool` 适配器
  （方法签名即契约：参数列表自动推导出 `__AgentArgs_*` 结构体 + JSON Schema），
  方法体保留原样；空 body 的 `async fn` 替换 body 为
  `agent_kit::run_generation` 调用（返回类型包装为
  `Result<T, GenerationError>`，`#[strategy]` 决定 Predict 还是 CodeAct）；
  同时生成 method 半边的 `blueprint_register_method_tools` 等**固有方法**。

### 为什么两个宏不会产生重复的 trait impl

Rust 不允许同一类型有两个 `impl AgentBlueprint for T`。因此 derive 生成的 trait
impl 是**唯一**的，它按名字调用 `blueprint_register_method_tools(...)`；在
`#[agent_impl]` 的具体 impl 处，**固有方法**同名覆盖 trait 的无操作默认实现，
所以只写其中一个宏也能编译（没写 `#[agent_impl]` 的 Agent 只是不注册方法工具）。

### 生成方法（async + 空 body）

```rust
// 输入
/// 根据当前所有笔记回答用户的问题。
async fn answer(&self, question: String) -> String {}

// 展开（概念）
pub async fn answer(&self, question: String)
    -> Result<String, agent_kit::GenerationError>
{
    let __prompt = format!("根据当前所有笔记回答用户的问题。\n\nArguments:\n- question: {:?}", question);
    let __system = format!("{}{}", self.agent_system_prompt(), self.agent_context_prompt());
    let mut __registry = agent_kit::tools::ToolRegistry::new();  // CodeAct 策略时
    agent_kit::AgentBlueprint::blueprint_register_tools(self, &mut __registry);
    agent_kit::run_generation::<_, String>(
        self.agent_client(), &self.agent_model(), &__system, &__prompt,
        Some(&__registry),                                    // Predict 时传 None
        &agent_kit::Strategy::CodeAct { max_iterations: 50, max_retries: 2 },
    ).await
}
```

生成方法绕过 hook 管线直接调 LLM，所以 `#[context]` 块通过
`agent_context_prompt()` 内联进 System Prompt；完整 Agent 运行（`into_agent`）
则通过 `ContextBlockHook` 注入。

## 约束

- Agent struct 必须 `Clone`（工具适配器持有 `Arc<Self>`）。
- 方法必须是 `&self`（`self`/`&mut self` 报错），不支持泛型方法。
- 生成方法：空 body + 必须声明返回类型（非 `Result` —— 宏自动包装）。
  参数要求 `Debug`（渲染进 prompt）。
- `#[context(dynamic)]` 字段的类型要 `Clone`（渲染时克隆快照；
  典型用法 `Arc<RwLock<Vec<T>>>`，需要 serde 的 `rc` feature ——
  agent-kit 已在 Cargo.toml 中启用）。
- 结构化输出类型要求 `Deserialize + JsonSchema`，建议按示例加
  `#[serde(crate = "agent_kit::serde")]` + `#[schemars(crate = "agent_kit::schemars")]`
  让 derive 使用 re-export 路径。

## 与现有代码共存

不修改 `core/`，不修改下游应用。`into_agent()` 产出标准
`engine::Agent<C>`，可继续叠加现有 hook（SandboxHook、PersistenceHook 等，
通过 `BuildConfig::extra_hooks` 注入）。
