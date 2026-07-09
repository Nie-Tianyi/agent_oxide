# Agent Oxide 开发者指南

Agent Oxide 是一个模块化的 Rust Agent 框架（Rust 2024 edition，Tokio 异步运行时）。本指南教你如何使用这个框架构建自己的 AI Agent。

## 目录

1. [架构概览](#1-架构概览)
2. [快速开始：构建一个最小 Agent](#2-快速开始构建一个最小-agent)
3. [实现 LLM Provider](#3-实现-llm-provider)
4. [实现 Tool（工具）](#4-实现-tool工具)
5. [实现 Hook（生命周期钩子）](#5-实现-hook生命周期钩子)
6. [Memory（会话记忆）与会话压缩](#6-memory会话记忆与会话压缩)
7. [组装：把所有组件连接在一起](#7-组装把所有组件连接在一起)
8. [Streaming Events 与 TUI 集成](#8-streaming-events-与-tui-集成)
9. [WorkspaceFs 文件沙箱](#9-workspacefs-文件沙箱)
10. [对话持久化](#10-对话持久化)
11. [完整示例：代码审查 Agent](#11-完整示例代码审查-agent)

---

## 1. 架构概览

### 工作空间结构

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["libs/*", "bins/*"]
├── libs/
│   ├── provider/           # LLMClient trait + 共享类型 (Message, ToolCall, ToolDef 等)
│   ├── deepseek/           # DeepSeekClient — 实现 LLMClient (SSE 流式)
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs 沙箱, generate_schema
│   ├── memory/             # Memory (内存缓冲), 压缩, 持久化
│   └── engine/             # Agent (ReAct 循环), AgentHook trait, AgentEvent 流
├── bins/
│   └── loomis/             # 二进制 — 具体工具、钩子、TUI、组装入口
└── docs/
    └── developer-guide.md  # 你正在读的文档
```

### Crate 依赖图

```
provider (无内部依赖)
    ↑
    ├── deepseek ──────── (实现 provider::LLMClient)
    ├── tools ─────────── (使用 provider::ToolDef)
    ├── memory ────────── (使用 provider::Message)
    ↑
    └── engine ────────── (使用 provider + tools + memory)
            ↑
        loomis (bin) ──── (使用全部五个 lib)
```

### 核心抽象一览

| Crate | 核心 Trait / 类型 | 作用 |
|-------|-------------------|------|
| `provider` | `LLMClient` | LLM 提供商抽象（generate / stream） |
| `provider` | `Message`, `Role`, `ToolCall`, `ToolDef` | 对话消息和工具定义 |
| `tools` | `Tool` | 工具抽象（name, description, parameters, execute） |
| `tools` | `ToolRegistry` | 工具注册表（按名称查找和分发执行） |
| `tools` | `WorkspaceFs` | 沙箱文件系统（所有路径限制在 workspace 内） |
| `memory` | `Memory`, `SharedMemory` | 对话记忆缓冲区 + 两阶段压缩 |
| `engine` | `Agent` | ReAct 循环：推理 → 行动 → 观察 → 推理... |
| `engine` | `AgentHook` | 生命周期回调（可拦截工具执行） |
| `engine` | `AgentEvent` | 流式事件（Token, ToolCallStart, ToolResult 等） |
| `engine` | `EngineContext` | Agent 配置和依赖注入容器 |

---

## 2. 快速开始：构建一个最小 Agent

### 2.1 创建新项目

在你的工作空间 `Cargo.toml` 中添加一个二进制 crate：

```toml
# bins/my-agent/Cargo.toml
[package]
name = "my-agent"
version = "0.1.0"
edition = "2024"

[dependencies]
tokio = { workspace = true }
provider = { path = "../../libs/provider" }
deepseek = { path = "../../libs/deepseek" }
tools = { path = "../../libs/tools" }
memory = { path = "../../libs/memory" }
engine = { path = "../../libs/engine" }
serde = { workspace = true }
serde_json = { workspace = true }
schemars = { workspace = true }
```

并在根 `Cargo.toml` 的 `members` 中添加 `"bins/my-agent"`。

### 2.2 最小 Agent 的五分钟版本

```rust
// bins/my-agent/src/main.rs
use std::sync::Arc;
use deepseek::DeepSeekClient;
use engine::{Agent, EngineContext};
use memory::{Memory, SharedMemory};
use provider::{Message, Role};
use tools::ToolRegistry;

#[tokio::main]
async fn main() {
    // 1. 加载 API Key
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API").expect("DEEPSEEK_API not set");

    // 2. 创建 LLM 客户端
    let client = DeepSeekClient::new(&api_key);

    // 3. 创建 Memory 并注入 System Prompt
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));
    {
        let mut mem = memory.write().unwrap();
        mem.push(Message::new(Role::System, "你是一个有帮助的助手。"));
    }

    // 4. 创建空的 ToolRegistry（无工具 Agent）
    let registry = Arc::new(ToolRegistry::new());

    // 5. 组装 EngineContext
    let ctx = EngineContext {
        llm: client,
        memory: memory.clone(),
        tools: registry,
        hooks: vec![],       // 无钩子
        model: "deepseek-v4-pro".into(),
        max_steps: 20,
        max_retries: 3,
        streaming: false,    // 非流式，简单输出
    };

    // 6. 创建 Agent 并运行
    let agent = Agent::new(ctx);

    println!("Agent 已启动。输入你的问题：");
    loop {
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        let input = input.trim().to_string();

        if input.is_empty() || input == "exit" {
            break;
        }

        // 将用户输入推入 memory
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(Role::User, &input));
        }

        match agent.run_loop(&input).await {
            Ok(response) => println!("Agent: {response}"),
            Err(e) => eprintln!("错误: {e}"),
        }
    }
}
```

这就是一个可运行的最小 Agent。它没有工具，只会进行纯文本对话。下面我们将逐步添加更多能力。

---

## 3. 实现 LLM Provider

`LLMClient` trait 是框架与 LLM 服务交互的唯一接口。它定义在 [`libs/provider/src/client.rs`](../libs/provider/src/client.rs)：

```rust
pub trait LLMClient: Send + Sync {
    /// 发送非流式补全请求
    fn generate(
        &self,
        req: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, ProviderError>> + Send;

    /// 发送流式补全请求，返回 BoxStream
    fn stream(
        &self,
        req: CompletionRequest,
    ) -> impl Future<
        Output = Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError>,
    > + Send;
}
```

### 3.1 核心请求/响应类型

**请求** — [`CompletionRequest`](../libs/provider/src/request.rs)：

```rust
pub struct CompletionRequest {
    pub messages: Vec<Message>,       // 对话历史
    pub model: String,                // 模型名称
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stop: Option<Vec<String>>,
    pub stream: bool,                  // 是否流式
    pub tools: Option<Vec<ToolDef>>,  // 工具定义
    pub tool_choice: Option<ToolChoice>,
}
```

使用 Builder 模式构建：

```rust
let request = CompletionRequest::new("deepseek-v4-pro", messages)
    .with_stream(true)
    .with_tools(tools)
    .with_max_tokens(4096);
```

**消息** — [`Message`](../libs/provider/src/message.rs)：

```rust
pub struct Message {
    pub role: Role,                          // System | User | Assistant | Tool
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,   // Assistant 消息中的工具调用
    pub tool_call_id: Option<String>,        // Tool 消息中的调用 ID
    pub name: Option<String>,
}

// 便捷构造方法
Message::new(Role::User, "Hello")
Message::assistant_with_tools("", tool_calls)
Message::tool_result("call_123", "output text")
```

### 3.2 实现新的 Provider（以 Anthropic 为例）

```rust
// libs/anthropic/src/lib.rs
use futures_util::stream::BoxStream;
use provider::{CompletionRequest, CompletionResponse, LLMClient, ProviderError, StreamChunk};

pub struct AnthropicClient {
    api_key: String,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            http: reqwest::Client::new(),
        }
    }
}

impl LLMClient for AnthropicClient {
    async fn generate(
        &self,
        req: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        // 1. 将 CompletionRequest 转换为 Anthropic Messages API 格式
        // 2. 发送 HTTP POST 到 https://api.anthropic.com/v1/messages
        // 3. 将 Anthropic 响应转换回 CompletionResponse
        todo!("实现 Anthropic API 调用")
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        // 1. 同上，但设置 stream: true
        // 2. 解析 SSE 事件流
        // 3. 将每个 SSE 事件转换为 StreamChunk
        todo!("实现 Anthropic 流式 API")
    }
}
```

### 3.3 ProviderError 错误类型

```rust
pub enum ProviderError {
    Http(String),              // HTTP 传输错误（可重试）
    Api { status: u16, body: String },  // API 错误（5xx 可重试）
    Parse(String),             // 解析错误（不可重试）
    StreamingNotSupported,     // Provider 不支持流式
}
```

参考实现：[`libs/deepseek/`](../libs/deepseek/) — 包含完整的 SSE 流式解析管道。

---

## 4. 实现 Tool（工具）

### 4.1 Tool Trait 定义

[`Tool`](../libs/tools/src/tool.rs) trait 是同步的（`Send + Sync`），不需要 `async_trait`：

```rust
pub trait Tool: Send + Sync {
    /// 工具名称 — 映射到 API 请求中的 function.name
    fn name(&self) -> &str;

    /// 给模型看的人类可读描述
    fn description(&self) -> &str;

    /// 工具参数的 JSON Schema
    fn parameters(&self) -> Value;

    /// 执行工具，接收 JSON 字符串参数，返回结果或错误
    fn execute(&self, args: &str) -> Result<String, ToolError>;

    /// 自动生成的：转换为 provider::ToolDef
    fn to_def(&self) -> provider::ToolDef { ... }
}
```

### 4.2 使用 `#[tool]` 宏（推荐）

`#[tool]` 属性宏自动生成 `Tool` trait 的实现，你只需要：

1. 定义一个 `Deserialize + JsonSchema` 参数结构体
2. 用 `#[tool(...)]` 标注结构体
3. 实现一个 inherent `execute` 方法

**示例：Echo 工具**

```rust
use schemars::JsonSchema;
use serde::Deserialize;
use tools::{ToolError, tool};

/// 参数结构体 — 同时用于 JSON Schema 生成和反序列化
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    #[schemars(description = "The text to echo back.")]
    pub text: String,
}

#[tool(
    name = "echo",
    description = "Echo the input text back unchanged. Use for testing.",
    args = EchoArgs        // ← 必须与上面定义的结构体匹配
)]
pub struct EchoTool;

impl EchoTool {
    fn execute(&self, args: EchoArgs) -> Result<String, ToolError> {
        Ok(args.text)
    }
}
```

**`#[tool]` 宏会自动生成：**
- `Tool::name()` → `"echo"`
- `Tool::description()` → description 字符串
- `Tool::parameters()` → 从 `EchoArgs` 自动生成 JSON Schema（通过 `OnceLock` 缓存）
- `Tool::execute()` → 反序列化 JSON 为 `EchoArgs`，然后调用 `EchoTool::execute`

**关键规则：**
- `args` 参数结构体必须 `#[derive(JsonSchema, Deserialize)]`
- `#[serde(deny_unknown_fields)]` 强烈推荐（拒绝未知字段，避免 LLM 产生的幻觉参数被静默忽略）
- inherent `execute` 方法的签名必须是 `fn execute(&self, args: ArgsType) -> Result<String, ToolError>`
- 结构体通常是一个 Unit struct（`pub struct EchoTool;`），依赖通过 `Arc<WorkspaceFs>` 等字段注入

### 4.3 手动实现 Tool trait

如果不想用 proc macro，也可以手动实现：

```rust
use serde_json::Value;
use tools::{Tool, ToolError, generate_schema};

pub struct MyTool;

impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }

    fn description(&self) -> &str { "Does something useful." }

    fn parameters(&self) -> Value {
        generate_schema::<MyArgs>()  // 自动从类型生成 JSON Schema
    }

    fn execute(&self, raw_args: &str) -> Result<String, ToolError> {
        let args: MyArgs = serde_json::from_str(raw_args)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid args: {e}")))?;
        // ... 执行业务逻辑 ...
        Ok("result".into())
    }
}
```

### 4.4 带 WorkspaceFs 依赖的工具

大多数文件操作工具需要 `WorkspaceFs` — 通过 `Arc<WorkspaceFs>` 注入：

```rust
#[tool(
    name = "read",
    description = "Read a file from the workspace...",
    args = ReadArgs
)]
pub struct ReadTool {
    fs: Arc<WorkspaceFs>,  // ← 通过 Arc 共享
}

impl ReadTool {
    pub fn new(fs: Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }

    fn execute(&self, args: ReadArgs) -> Result<String, ToolError> {
        self.fs
            .read(&args.file_path, args.offset, args.limit)
            .map_err(|e| match e {
                FsError::NotFound(_) => ToolError::InvalidArgs(e.to_string()),
                _ => ToolError::Execution(e.to_string()),
            })
    }
}
```

### 4.5 ToolError 错误类型

```rust
pub enum ToolError {
    InvalidArgs(String),   // 参数格式/验证错误 → 告诉 LLM 修正参数
    Execution(String),     // 工具执行失败 → 告诉 LLM 操作失败了
    Internal(String),      // 不应该暴露给 LLM 的内部错误
}
```

### 4.6 ToolRegistry（工具注册表）

[`ToolRegistry`](../libs/tools/src/registry.rs) 管理所有注册的工具：

```rust
let mut registry = ToolRegistry::new();

// 注册工具（key = Tool::name()）
registry.register(Arc::new(EchoTool));
registry.register(Arc::new(ReadTool::new(workspace.clone())));

// 查询
assert!(registry.has("echo"));
let tool: Option<&Arc<dyn Tool>> = registry.get("echo");

// 生成 LLM API 请求所需的 ToolDef 列表
let tool_defs: Vec<ToolDef> = registry.to_tool_defs();

// 按名称分发执行
let result: Option<Result<String, ToolError>> = registry.execute("echo", r#"{"text":"hi"}"#);

// 列出所有工具名
let names: Vec<&str> = registry.iter().map(|(n, _)| n).collect();
```

---

## 5. 实现 Hook（生命周期钩子）

[`AgentHook`](../libs/engine/src/hooks.rs) trait 让你在 Agent 执行的各个阶段插入自定义逻辑。所有方法都有默认空实现 — 你只需要实现你关心的事件。

```rust
pub trait AgentHook: Send + Sync {
    /// 新用户输入开始一个完整的任务运行
    fn on_run_start(&self, session_id: &str, user_input: &str) {}

    /// 发送请求给 LLM 之前
    fn on_llm_start(&self, session_id: &str) {}

    /// 收到 LLM 响应之后
    fn on_llm_end(&self, session_id: &str, response: &Message) {}

    /// 执行工具之前 — 返回 Err 可以**阻止**工具执行
    fn before_tool_call(
        &self,
        session_id: &str,
        tool_call: &ToolCall,
    ) -> Result<(), AgentError> {
        Ok(())
    }

    /// 工具执行之后，参数中包含执行结果
    fn after_tool_call(
        &self,
        session_id: &str,
        tool_call: &ToolCall,
        observation: &str,
    ) {}
}
```

### 5.1 日志钩子示例

```rust
use engine::{AgentError, AgentHook};
use provider::ToolCall;

pub struct CliLoggerHook;

impl AgentHook for CliLoggerHook {
    fn on_llm_start(&self, _session_id: &str) {
        eprintln!("⏳ Agent thinking...");
    }

    fn before_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
    ) -> Result<(), AgentError> {
        eprintln!("🔧 Executing: {} | args: {}", tool.function.name, tool.function.arguments);
        Ok(())  // 总是允许
    }

    fn after_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
        observation: &str,
    ) {
        let preview: String = observation.chars().take(80).collect();
        eprintln!("✅ {} → {}", tool.function.name, preview);
    }
}
```

### 5.2 工具审批钩子（拦截危险操作）

这是 `before_tool_call` 的核心用例 — 在工具执行前拦截并决定是否放行：

```rust
pub struct DangerousCommandApprovalHook {
    agent_tx: OnceLock<mpsc::UnboundedSender<AgentEvent>>,
    approval_rx: Mutex<std::sync::mpsc::Receiver<bool>>,
}

impl AgentHook for DangerousCommandApprovalHook {
    fn before_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
    ) -> Result<(), AgentError> {
        // 只拦截 shell 工具
        if tool.function.name != "shell" {
            return Ok(());
        }

        // 解析命令
        let command = parse_shell_command(&tool.function.arguments);

        // 通知 TUI 显示确认提示
        if let Some(tx) = self.agent_tx.get() {
            let _ = tx.send(AgentEvent::ShellApprovalRequested {
                tool_call_id: tool.id.clone(),
                command: command.clone(),
            });
        }

        // 阻塞等待用户响应
        let approved = self.approval_rx.lock().unwrap().recv().unwrap_or(false);

        if !approved {
            return Err(AgentError::ToolRejected {
                name: "shell".into(),
                reason: "User denied shell command execution".into(),
            });
        }

        Ok(())
    }
}
```

**关键机制：** 当 `before_tool_call` 返回 `Err(AgentError::ToolRejected)` 时，Agent 循环会：
1. 跳过工具执行
2. 将拒绝消息作为 Tool Result 推入 memory
3. LLM 会看到拒接消息并可以调整策略

### 5.3 组合多个 Hook

Hook 可以组合使用 — 它们按注册顺序依次执行：

```rust
let hooks: Vec<Box<dyn engine::AgentHook>> = vec![
    Box::new(CliLoggerHook),
    Box::new(DangerousCommandApprovalHook::new().0),
    Box::new(my_custom_hook),
];
```

---

## 6. Memory（会话记忆）与会话压缩

### 6.1 Memory 基础

[`Memory`](../libs/memory/src/memory.rs) 是一个消息缓冲区，线程安全地通过 `SharedMemory`（即 `Arc<RwLock<Memory>>`）共享：

```rust
use memory::{Memory, SharedMemory};
use provider::{Message, Role};

let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));

// 写入
{
    let mut mem = memory.write().unwrap();
    mem.push(Message::new(Role::System, "你是一个编码助手。"));
    mem.push(Message::new(Role::User, "帮我写一个函数。"));
}

// 读取
{
    let mem = memory.read().unwrap();
    let messages: &[Message] = mem.messages();
    let context: Vec<Message> = mem.to_context_vec(); // clone
}
```

### 6.2 MemoryBuilder

```rust
let memory = Memory::builder()
    .threshold(500_000)  // 自定义压缩阈值（字符数，默认 2,000,000）
    .keep_last(15)        // 压缩时保留最近 N 条消息（默认 10）
    .with_messages(preloaded_history)
    .build();
```

### 6.3 两阶段压缩

当会话历史超过 `compact_threshold` 字符时，`push()` 返回 `CompactSignal::NeedsCompact`。压缩分两步：

```rust
// 阶段 1：排出旧消息（System 消息永不被排出）
let drained: Vec<Message> = {
    let mut mem = memory.write().unwrap();
    mem.drain_for_compact()
};

// 阶段 2：通过 LLM 生成摘要（在 loomis 中是 compact_with_deepseek）
let summary = generate_summary(&client, &drained).await;

// 阶段 3：将摘要作为新 System 消息插入
{
    let mut mem = memory.write().unwrap();
    mem.apply_compact(summary);
}
```

参考实现：`loomis::compact::compact_with_deepseek`。

---

## 7. 组装：把所有组件连接在一起

### 7.1 EngineContext

[`EngineContext`](../libs/engine/src/context.rs) 是所有依赖的容器：

```rust
pub struct EngineContext<C: LLMClient> {
    pub llm: C,                          // LLM provider
    pub memory: SharedMemory,            // 共享记忆
    pub tools: Arc<ToolRegistry>,        // 工具注册表
    pub hooks: Vec<Box<dyn AgentHook>>,  // 生命周期钩子
    pub model: String,                   // 模型名
    pub max_steps: usize,                // 最大循环步数 (防止无限循环)
    pub max_retries: usize,              // 网络错误最大重试次数
    pub streaming: bool,                 // 是否启用 SSE 流式
}
```

### 7.2 标准组装模式

参考 [`loomis::app::build_coding_agent`](../bins/loomis/src/app.rs)，标准组装遵循以下步骤：

```rust
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use deepseek::DeepSeekClient;
use engine::{Agent, AgentEvent, EngineContext};
use memory::{Memory, SharedMemory};
use provider::{Message, Role};
use tokio::sync::mpsc;
use tools::{ToolRegistry, WorkspaceFs};

pub fn build_coding_agent(api_key: &str, workspace_root: &Path, model: &str, flash_model: &str) -> Agent<DeepSeekClient> {
    // 1. 创建事件通道（用于 TUI/CLI 获取实时进度）
    let (agent_tx, agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // 2. 创建文件沙箱
    let workspace = WorkspaceFs::new(workspace_root)
        .expect("无法创建 workspace");
    let workspace = Arc::new(workspace);

    // 3. 创建并注册工具
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CalculatorTool));
    registry.register(Arc::new(ReadTool::new(workspace.clone())));
    registry.register(Arc::new(WriteTool::new(workspace.clone())));
    registry.register(Arc::new(ShellTool::new(
        workspace_root.to_path_buf(),
        Duration::from_secs(30),
    )));
    let registry = Arc::new(registry);

    // 4. 创建 Memory
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));

    // 5. 创建 LLM 客户端
    let client = DeepSeekClient::new(api_key);

    // 6. 创建 Hooks
    let (approval_hook, approval_tx) = DangerousCommandApprovalHook::new();
    approval_hook.set_agent_tx(agent_tx.clone());

    let hooks: Vec<Box<dyn engine::AgentHook>> = vec![
        Box::new(approval_hook),
        Box::new(CliLoggerHook),
    ];

    // 7. 组装 EngineContext
    let ctx = EngineContext {
        llm: client,
        memory: memory.clone(),
        tools: registry,
        hooks,
        model: model.to_string(),
        max_steps: 50,
        max_retries: 3,
        streaming: true,
    };

    // 8. 创建 Agent
    let agent = Agent::new(ctx);

    // 9. 注入 System Prompt
    {
        let mut mem = memory.write().unwrap();
        mem.push(Message::new(Role::System, SYSTEM_PROMPT));
    }

    agent
}
```

### 7.3 Agent 启动方法

```rust
// 方式 1：简单运行（无事件流）
let result: String = agent.run_loop("你好，帮我写个排序函数").await?;

// 方式 2：带事件流（用于 TUI 实时渲染）
let (tx, rx) = mpsc::unbounded_channel();
let result: String = agent.run_with_events("用户输入", tx).await?;

// Rx 端在另一个 task 中消费事件
while let Some(event) = rx.recv().await {
    match event {
        AgentEvent::Token(t) => print!("{t}"),
        AgentEvent::ToolCallStart { id, name } => println!("\n[{name}] starting..."),
        AgentEvent::ToolResult { name, output, .. } => println!("[{name}] done"),
        AgentEvent::Done => break,
        _ => {}
    }
}
```

---

## 8. Streaming Events 与 TUI 集成

### 8.1 AgentEvent 类型

[`AgentEvent`](../libs/engine/src/agent.rs) 枚举覆盖了 Agent 执行过程中的所有事件：

```rust
pub enum AgentEvent {
    Token(String),                        // 流式文本 token
    ReasoningToken(String),               // 推理 token（reasoning 模型）
    ToolCallStart { id: String, name: String },   // 工具调用开始
    ToolCallArgsDelta { id: String, delta: String }, // 工具参数片段
    ToolResult { id: String, name: String, output: String }, // 工具执行结果
    ShellRunning { command: String },     // Shell 正在执行
    ShellOutput { command: String, output: String }, // Shell 输出
    ShellApprovalRequested {              // 需要审批的 Shell 命令
        tool_call_id: String,
        command: String,
    },
    Done,                                  // Agent 结束
}
```

### 8.2 通道拓扑（TUI 架构）

loomis 的 TUI 使用以下通道架构：

```text
TUI 线程                          Agent 任务 (tokio::spawn)
─────────                          ────────────────────────
cmd_tx ───────── TuiCommand ──────→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx
```

### 8.3 消费事件流

```rust
// 在 tokio::spawn 中运行 agent
tokio::spawn(async move {
    let agent_tx_clone = agent_tx.clone();
    if let Err(e) = agent.run_with_events(&user_input, agent_tx_clone).await {
        let _ = agent_tx.send(AgentEvent::Token(format!("\n错误: {e}")));
    }
    let _ = agent_tx.send(AgentEvent::Done);
});

// 在主线程消费事件
while let Some(event) = agent_rx.blocking_recv() {
    match event {
        AgentEvent::Token(text) => {
            // 流式输出每个 token
            print!("{text}");
        }
        AgentEvent::ToolCallStart { name, .. } => {
            println!("\n🔧 调用工具: {name}");
        }
        AgentEvent::ToolResult { output, .. } => {
            // 显示工具输出（可能需要截断）
            let preview = output.chars().take(200).collect::<String>();
            println!("结果: {preview}");
        }
        AgentEvent::Done => break,
        _ => {}
    }
}
```

---

## 9. WorkspaceFs 文件沙箱

[`WorkspaceFs`](../libs/tools/src/fs.rs) 确保所有文件操作都在 `workspace_root` 内进行：

```rust
use tools::WorkspaceFs;

let fs = WorkspaceFs::new("/path/to/project")?;

// 路径解析 — 自动规范化并检查是否逃逸沙箱
let resolved = fs.resolve("src/main.rs")?;  // → /path/to/project/src/main.rs
// fs.resolve("../etc/passwd") → Err(FsError::PathEscapesWorkspace)

// 读文件（支持 offset 和 limit）
let content = fs.read("src/main.rs", Some(10), Some(50))?;

// 写文件（自动创建父目录）
fs.write("new/file.txt", "hello world")?;

// 编辑文件行
fs.edit_lines("src/main.rs", 5, 10, "replacement\ncontent")?;

// Glob 匹配
let files = fs.glob("src/**/*.rs")?;

// Grep 搜索
let matches = fs.grep("TODO", Some("src/**/*.rs"))?;

// 列出目录
let entries = fs.ls(Some("src/"))?;
```

**安全特性：**
- 所有路径通过 `canonicalize` 规范化
- 逃逸沙箱的路径返回 `FsError::PathEscapesWorkspace`
- 写操作会自动创建父目录
- Glob/Grep 结果自动转为相对于 workspace root 的路径

---

## 10. 对话持久化

[`memory::persistence`](../libs/memory/src/persistence.rs) 模块提供会话的保存和恢复：

```rust
use memory::persistence::{
    save_conversation, load_conversation, list_threads,
    ThreadInfo, default_thread_name, generate_thread_name,
    read_current_thread, write_current_thread,
};

// 保存当前会话
save_conversation(&memory, "my_session")?;
// → 写入 .loomis/threads/my_session.json 和 .md

// 列出所有会话
let threads: Vec<ThreadInfo> = list_threads()?;
for t in &threads {
    println!("{} - {} messages", t.name, t.message_count);
}

// 恢复历史会话
let history = load_conversation("my_session")?;
let memory = Memory::from(history.messages);

// 自动生成会话名称（基于第一条用户消息）
let name = generate_thread_name("帮我写一个 Rust web server");
// → "帮我写一个 Rust web server" (截断)
```

---

## 11. 完整示例：代码审查 Agent

这是一个从零构建的完整 Agent，它会读取代码文件并提供审查意见。

```rust
// bins/code-reviewer/src/main.rs
use std::sync::Arc;
use std::time::Duration;

use deepseek::DeepSeekClient;
use engine::{Agent, AgentError, AgentEvent, AgentHook, EngineContext};
use memory::{Memory, SharedMemory};
use provider::{Message, Role, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;
use tools::{ToolError, ToolRegistry, WorkspaceFs, tool};

// ══════════════════════════════════════════════════════════════════════
// 工具：读取文件
// ══════════════════════════════════════════════════════════════════════

#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeReviewArgs {
    #[schemars(description = "要审查的文件的路径")]
    pub file_path: String,
}

#[tool(
    name = "review_file",
    description = "读取一个源代码文件并返回其内容以供审查。只读操作。",
    args = CodeReviewArgs
)]
struct ReviewFileTool {
    fs: Arc<WorkspaceFs>,
}

impl ReviewFileTool {
    fn new(fs: Arc<WorkspaceFs>) -> Self { Self { fs } }

    fn execute(&self, args: CodeReviewArgs) -> Result<String, ToolError> {
        self.fs
            .read(&args.file_path, None, None)
            .map_err(|e| ToolError::Execution(e.to_string()))
    }
}

// ══════════════════════════════════════════════════════════════════════
// Hook: 记录审查进度
// ══════════════════════════════════════════════════════════════════════

struct ReviewProgressHook;

impl AgentHook for ReviewProgressHook {
    fn before_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
    ) -> Result<(), AgentError> {
        println!("📖 正在审查: {}", tool.function.arguments);
        Ok(())
    }

    fn after_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
        observation: &str,
    ) {
        let lines = observation.lines().count();
        println!("✅ 已读取 {} ({} 行)", tool.function.name, lines);
    }
}

// ══════════════════════════════════════════════════════════════════════
// System Prompt
// ══════════════════════════════════════════════════════════════════════

const SYSTEM_PROMPT: &str = "\
你是一个资深的代码审查专家。你的任务是：
1. 使用 review_file 工具读取用户指定的文件
2. 对代码进行全面的审查：
   - 逻辑错误和潜在 bug
   - 安全漏洞
   - 性能和内存问题
   - 代码风格和改进建议
3. 将发现的问题按严重程度分类（严重 / 中等 / 建议）
4. 给出具体的、可操作的改进建议

只审查用户请求的文件，不要主动去探索项目结构。";

// ══════════════════════════════════════════════════════════════════════
// 主函数
// ══════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API").expect("DEEPSEEK_API not set");
    let cwd = std::env::current_dir().unwrap();

    // ── 沙箱 ──────────────────────────────────────────
    let fs = WorkspaceFs::new(&cwd).expect("workspace init failed");
    let fs = Arc::new(fs);

    // ── 工具注册 ─────────────────────────────────────
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReviewFileTool::new(fs)));
    let registry = Arc::new(registry);

    // ── Memory ────────────────────────────────────────
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));

    // ── LLM ───────────────────────────────────────────
    let client = DeepSeekClient::new(&api_key);

    // ── Context ───────────────────────────────────────
    let ctx = EngineContext {
        llm: client,
        memory: memory.clone(),
        tools: registry,
        hooks: vec![Box::new(ReviewProgressHook)],
        model: "deepseek-v4-pro".into(),
        max_steps: 15,
        max_retries: 3,
        streaming: true,
    };

    let agent = Agent::new(ctx);

    // ── Seed ──────────────────────────────────────────
    {
        let mut mem = memory.write().unwrap();
        mem.push(Message::new(Role::System, SYSTEM_PROMPT));
    }

    // ── Event channel ─────────────────────────────────
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    println!("=== 代码审查 Agent ===");
    println!("输入要审查的文件路径（相对于当前目录）：");

    let mut file_path = String::new();
    std::io::stdin().read_line(&mut file_path).unwrap();
    let file_path = file_path.trim().to_string();

    let prompt = format!("请审查文件: {file_path}");

    {
        let mut mem = memory.write().unwrap();
        mem.push(Message::new(Role::User, &prompt));
    }

    // ── 运行并消费流式事件 ──────────────────────────
    let tx_clone = tx.clone();
    let handle = tokio::spawn(async move {
        if let Err(e) = agent.run_with_events(&prompt, tx_clone).await {
            let _ = tx.send(AgentEvent::Token(format!("\n[错误] {e}")));
        }
        let _ = tx.send(AgentEvent::Done);
    });

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::Token(text) => print!("{text}"),
            AgentEvent::ToolCallStart { name, .. } => {
                println!("\n━━━ 调用工具: {name} ━━━");
            }
            AgentEvent::ToolResult { name, output, .. } => {
                let lines = output.lines().count();
                println!("[{}] 完成，读取了 {lines} 行", name);
            }
            AgentEvent::Done => break,
            _ => {}
        }
    }

    handle.await.unwrap();
    println!("\n=== 审查完成 ===");
}
```

### 运行

```bash
cargo run -p code-reviewer
# 输入: src/main.rs
# Agent 会读取文件内容，然后给出详细的代码审查意见
```

---

## 附录 A：开发建议

### A.1 设计原则

1. **工具要原子化** — 一个工具做一件事。LLM 更擅长组合简单工具而不是使用复杂工具。
2. **描述要详细** — `Tool::description()` 是 LLM 选择工具的唯一依据。写明使用场景和 NOT to use 的场景。
3. **参数 Schema 要精确** — 使用 `#[schemars(description = "...")]` 给每个字段添加描述。
4. **Hook 是插桩点，不是业务逻辑** — 不要在 Hook 中放复杂的业务逻辑。
5. **同步 Tool trait** — Tool 是同步的。如果工具需要异步 I/O，使用 `tokio::task::block_in_place` 或 `spawn_blocking`。

### A.2 调试技巧

```rust
// 1. 查看 Agent 的完整对话历史
let mem = agent.memory().read().unwrap();
for msg in mem.messages() {
    println!("[{:?}] {}", msg.role, msg.content);
}

// 2. 设置较低的最大步数来调试工具循环
let ctx = EngineContext {
    max_steps: 3,  // 限制 3 步，方便观察
    ..
};

// 3. 使用非流式模式做原型验证
let ctx = EngineContext {
    streaming: false,  // 非流式输出更易读
    ..
};
```

### A.3 添加 Provider 的检查清单

- [ ] 实现 `LLMClient::generate()` — 非流式
- [ ] 实现 `LLMClient::stream()` — SSE 流式
- [ ] 正确映射请求格式（Message → Provider API）
- [ ] 正确映射响应格式（Provider API → CompletionResponse/StreamChunk）
- [ ] 处理错误重试（5xx → 重试，4xx → 不重试）
- [ ] 处理 Tool Call 的流式和非流式返回

### A.4 添加工具的检查清单

- [ ] 定义参数结构体（`Deserialize + JsonSchema`）
- [ ] 用 `#[tool(name, description, args)]` 标注或手动实现 `Tool` trait
- [ ] 实现 inherent `execute` 方法
- [ ] 处理参数错误 → `ToolError::InvalidArgs`
- [ ] 处理执行错误 → `ToolError::Execution`
- [ ] 如果需要文件访问，通过 `Arc<WorkspaceFs>` 注入
- [ ] 在组装函数中注册到 `ToolRegistry`

---

## 附录 B：现有代码参考

| 想了解... | 看这个文件 |
|-----------|-----------|
| Agent 循环核心逻辑 | [`libs/engine/src/agent.rs`](../libs/engine/src/agent.rs) |
| Hook trait 定义 | [`libs/engine/src/hooks.rs`](../libs/engine/src/hooks.rs) |
| Tool trait + `#[tool]` 宏 | [`libs/tools/src/tool.rs`](../libs/tools/src/tool.rs) + [`libs/tools-macros/src/lib.rs`](../libs/tools-macros/src/lib.rs) |
| ToolRegistry | [`libs/tools/src/registry.rs`](../libs/tools/src/registry.rs) |
| WorkspaceFs 沙箱 | [`libs/tools/src/fs.rs`](../libs/tools/src/fs.rs) |
| Memory + 压缩 | [`libs/memory/src/memory.rs`](../libs/memory/src/memory.rs) |
| LLMClient trait | [`libs/provider/src/client.rs`](../libs/provider/src/client.rs) |
| Message / ToolCall 类型 | [`libs/provider/src/message.rs`](../libs/provider/src/message.rs) |
| DeepSeek Client 实现 | [`libs/deepseek/src/lib.rs`](../libs/deepseek/src/lib.rs) |
| 组装入口（完整 wiring） | [`bins/loomis/src/app.rs`](../bins/loomis/src/app.rs) |
| 计算器工具（手写解析器） | [`bins/loomis/src/tools/calculator.rs`](../bins/loomis/src/tools/calculator.rs) |
| 文件读取工具（WorkspaceFs） | [`bins/loomis/src/tools/tool_read.rs`](../bins/loomis/src/tools/tool_read.rs) |
| Shell 审批 Hook | [`bins/loomis/src/hooks/shell_approval.rs`](../bins/loomis/src/hooks/shell_approval.rs) |
| 日志 Hook | [`bins/loomis/src/hooks/cli_logger.rs`](../bins/loomis/src/hooks/cli_logger.rs) |
| TUI 集成 | [`bins/loomis/src/tui/mod.rs`](../bins/loomis/src/tui/mod.rs) |
