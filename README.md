# 🦀 Loomis

<p align="center">
  <img src="https://img.shields.io/badge/Rust-2024%20edition-orange?style=flat-square&logo=rust" alt="Rust 2024">
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License">
  <img src="https://img.shields.io/badge/状态-MVP-green?style=flat-square" alt="Status: MVP">
  <img src="https://img.shields.io/badge/DeepSeek-API-4B6BFB?style=flat-square" alt="DeepSeek API">
</p>

<p align="center">
  <em>一个完全用 Rust 从零构建的 AI 编码助手。<br>
  Tokio 异步 · Trait 工具抽象 · SSE 流式响应 · 终端交互界面</em>
</p>

<p align="center">
  <img src="UI_screenshot.png" alt="Loomis TUI 截图" width="80%">
</p>

---

## 项目简介

Loomis 是一个纯 Rust（2024 edition）编写的 **AI 编码助手**。它在终端中运行，提供完整的 TUI 聊天界面，能够在沙箱化的工作区内读写文件、用 glob 和 grep 搜索代码，并通过 SSE 协议实时流式输出模型回复——全部基于 DeepSeek API。

不依赖任何黑盒框架，从 HTTP 请求到 SSE 解析再到终端渲染，每一层的设计决策都透明可追溯。

## 特性

- **DeepSeek API 客户端** — 完整的请求/响应类型定义，三层 SSE 流式解析管道，`FinishReason::Other(String)` 向前兼容未知值
- **Agent 主循环** — `run_loop()` 批量模式 + `run_with_events()` 实时流式模式，通过 `mpsc` 通道推送事件，`max_steps` 防止无限循环
- **可插拔工具系统** — `Tool` trait + `ToolRegistry`，内置计算器、Shell 命令执行（需用户确认）、文件读写/编辑、glob 文件匹配、grep 内容搜索、ls 目录列表等 9 个工具，统一由 `WorkspaceFs` 沙箱隔离
- **对话记忆管理** — 可配置压缩阈值，两阶段压缩（`drain_for_compact` → `apply_compact`），`Arc<RwLock<Memory>>` 多任务共享，系统消息永不被压缩
- **对话持久化** — 自动保存到 `.loomis/threads/`（JSON + Markdown），多线程命名管理，`/resume` 弹窗选择器恢复历史对话，`/save <name>` 手动命名快照
- **终端交互界面** — [ratatui](https://ratatui.rs/) + [crossterm](https://crates.io/crates/crossterm) 打造，支持实时流式 Token、工具调用状态展示、滚动历史、斜杠命令、线程选择器弹窗

## 快速开始

### 前置条件

- [Rust](https://www.rust-lang.org/tools/install) 工具链（2024 edition）
- [DeepSeek API](https://platform.deepseek.com/) 密钥

### 环境配置

```bash
# 克隆仓库
git clone https://github.com/Nie-Tianyi/loomis.git
cd loomis

# 创建 .env 文件
echo 'DEEPSEEK_API=sk-your-key-here' > .env
```

### 运行

```bash
# 启动 TUI 模式（默认）
cargo run --release

# 或使用传统命令行模式
cargo run --release -- --no-tui
```

### 测试与检查

```bash
cargo test                # 运行所有测试
cargo test tui            # 仅运行 TUI 模块测试
cargo clippy              # 代码检查
```

## 架构详解

### 项目结构

```text
loomis/
├── Cargo.toml
├── .env                        # DeepSeek API 密钥（不纳入版本控制）
├── UI_screenshot.png           # TUI 截图
└── src/
    ├── main.rs                 # 程序入口，TUI（默认）/ --no-tui 传统 CLI
    ├── lib.rs                  # 库根，统一 re-export 各模块
    ├── core/
    │   ├── mod.rs              # 声明 pub mod client; mod agent;
    │   ├── agent.rs            # Agent 核心主循环
    │   └── client/
    │       ├── mod.rs          # 扁平重导出所有 client 类型
    │       ├── client.rs       # DeepSeekClient：send()（非流式）+ stream()（SSE 流式）
    │       ├── request.rs      # 请求类型：DeepSeekRequest, Message, Role, ToolCall, ToolDef, FunctionDef 等
    │       ├── response.rs     # 响应类型：DeepSeekResponse, Choice, FinishReason, Usage
    │       ├── stream.rs       # SSE 流解析：DeepSeekStream, DeepSeekChunk, ChunkChoice, Delta
    │       └── error.rs        # DeepSeekError：Http / Api / Parse / StreamingNotSupported
    ├── memory/
    │   └── mod.rs              # Memory, SharedMemory, MemoryBuilder, 两阶段压缩
    ├── persistence/
    │   └── mod.rs              # 对话持久化：save/load/list/generate_thread_name
    ├── tools/
    │   ├── mod.rs              # 模块根，re-export Tool, ToolRegistry, 所有内置工具
    │   ├── tool.rs             # Tool trait 定义
    │   ├── registry.rs         # ToolRegistry：HashMap 存储，register/execute/to_tool_defs
    │   ├── schema.rs           # generate_schema<T>()：schemars 自动生成 JSON Schema
    │   ├── error.rs            # ToolError（Execution/InvalidArgs）+ FsError（文件系统错误）
    │   ├── fs.rs               # WorkspaceFs：路径规范化 + 沙箱边界检查
    │   ├── calculator.rs       # CalculatorTool：词法分析器 → 递归下降解析器 → 求值
    │   ├── echo.rs             # EchoTool：最小参考实现，用于自定义工具开发
    │   ├── tool_read.rs        # ReadTool：支持 offset/limit，cat -n 风格输出
    │   ├── tool_write.rs       # WriteTool：创建或覆盖文件，自动创建父目录
    │   ├── tool_edit.rs        # EditTool：行级替换（start_line..=end_line，1-indexed）
    │   ├── tool_glob.rs        # GlobTool：模式匹配文件，返回排序后的相对路径
    │   ├── tool_grep.rs        # GrepTool：正则搜索，支持 path_glob 过滤
    │   ├── tool_ls.rs          # LsTool：列出目录内容，显示类型/大小，目录优先
    │   └── tool_shell.rs       # ShellTool：在沙箱内执行 Shell 命令，每次调用需用户确认 (Y/n)
    └── tui/
        ├── mod.rs              # 模块根，re-export App, ChatMessage, ToolCallState, TuiCommand, run
        ├── app.rs              # 核心状态机：App, ChatMessage（6 种变体）, apply_event 流式合并, 键盘处理
        ├── ui.rs               # ratatui 渲染：三面板布局（聊天/输入/状态栏），message_to_lines 样式映射
        └── event.rs            # 事件循环 + Agent 桥接：run() 入口，run_event_loop() 主循环，agent_handler 后台任务
```

### 模块职责表

| 模块 | 核心类型 | 职责 |
| ---- | -------- | ---- |
| `core/client/` | `DeepSeekClient`, `DeepSeekStream`, `DeepSeekChunk` | DeepSeek API 的类型化请求/响应封装，SSE 流式解析与反序列化 |
| `core/agent.rs` | `Agent`, `AgentEvent`, `AgentError` | Agent 主循环：两种运行模式、指数退避重试、工具调用分发、内存压缩触发 |
| `memory/` | `Memory`, `SharedMemory`, `MemoryBuilder`, `CompactSignal` | 对话上下文存储：可配置阈值、两阶段压缩、字符数估算 |
| `persistence/` | `save_conversation`, `load_conversation`, `list_threads`, `generate_thread_name` | 对话持久化：多线程保存/恢复，JSON + Markdown 双格式，首条消息自动命名，`/resume` 选择器 |
| `tools/` | `Tool`, `ToolRegistry`, `WorkspaceFs`, 9 个工具结构体 | 工具抽象层：trait 定义、注册分发、沙箱文件系统、内置工具集（含 Shell 命令确认） |
| `tui/` | `App`, `ChatMessage`, `ThreadPicker`, `ToolCallState`, `TuiCommand` | 终端 UI：滚动历史、流式 Token 合并、工具调用状态卡片、键盘输入与斜杠命令、线程选择器弹窗 |

### Agent 主循环 (`core/agent.rs`)

Agent 是整个框架的控制中心，对外提供两种运行模式：

| 模式 | 传输方式 | 首字节延迟 | 适用场景 |
| ---- | -------- | --------- | -------- |
| **流式**（默认） | SSE (`text/event-stream`) | ~100ms | 交互式聊天、实时 UI |
| **非流式** | JSON (`application/json`) | 完整响应时间 | 批量处理、调试 |

**运行流程**：

```text
run_loop()
  ├─ [流式]  run_streaming_loop()
  └─ [非流]  run_non_streaming_loop()
       │
       ├─ maybe_compact()          — 检查是否触发记忆压缩
       ├─ stream_with_retry()      — 指数退避重试瞬时错误
       │
       ├─ 有 tool_calls? → execute_all() → push 结果 → 继续循环
       └─ 纯文本?       → push 到 memory → 返回
```

**关键设计决策**：

1. **`std::sync::RwLock` + async** — 锁在每次 `.await` 之前释放。`std::sync` guard 不能跨越 await 点，因为 Tokio 的工作窃取调度器可能会将 future 迁移到不同 OS 线程。这是 Rust 异步编程的基本模式。
2. **同步 Tool 执行** — `Tool` trait 是对象安全的，无需 `async_trait`。CPU 密集型工具内联执行；I/O 密集型工具可内部包装 `tokio::task::spawn_blocking`。
3. **流式 Tool Call 合并** — SSE 流中，单个 tool call 被分散到多个 chunk。`StreamAccumulator` 按 index 重新组装：第一个 chunk 携带 `id` + `function.name`，后续 chunk 追加 `function.arguments`。
4. **批量锁获取** — 所有工具结果先收集到 `Vec<Message>` 中，然后在单次写锁下批量写入 memory，减少锁竞争。

### SSE 流式管道 (`core/client/stream.rs`)

```text
HTTP chunk → buffer → find_event_end(\n\n) → trim_trailing_newlines
           → extract_sse_data("data: ") → [DONE]? 跳过
                                         → parse JSON → DeepSeekChunk
                                                     ├─ content → Token
                                                     ├─ reasoning_content → ReasoningToken
                                                     └─ tool_calls → ToolCallStart / ToolCallArgsDelta
```

三层解析，逐层剥离：

| 层 | 函数 | 输入 | 输出 |
| --- | ---- | --- | ---- |
| 1. 分帧 | `find_event_end` | 字节流 buffer | 完整 SSE 事件（以 `\n\n` 分隔） |
| 2. 提取 | `extract_sse_data` | SSE 事件 | `data:` 行内容（空行 → `[DONE]` 跳过） |
| 3. 反序列化 | `serde_json::from_str` | JSON 字符串 | `DeepSeekChunk` 结构体 |

### 记忆模块 (`memory/mod.rs`)

**核心类型**：

| 类型 | 作用 |
| ---- | ---- |
| `Memory` | 持有 `Vec<Message>`，包含压缩逻辑 |
| `SharedMemory` | `Arc<RwLock<Memory>>` — 线程安全共享包装 |
| `MemoryBuilder` | 流式构造器：`.threshold().keep_last().build()` |
| `CompactSignal` | `push()` 返回的建议信号：`WithinBudget` / `NeedsCompact` |
| `MemoryError` | 压缩操作错误：`SummariserFailed` / `NothingToCompact` |

**两阶段压缩**：

将 Memory 与 LLM 提供者解耦——压缩由调用方完全控制：

```rust
// Phase 1: 排空旧的非 System 消息
let drained: Vec<Message> = mem.drain_for_compact();
// Phase 2: 外部调用 LLM 总结（调用方控制），然后应用
mem.apply_compact(summary);
// 结果: [System("summary..."), System(原始...), 最近消息...]
```

**设计要点**：

- **System 消息永不被排空**——它们承载持久化指令（系统提示词、历史摘要），必须保留
- **字符数阈值而非 Token 数**——不同模型的 Tokenizer 不同且计算成本高；字符数是快速、确定性的近似值（2M 字符 ≈ 500k–1M Token）
- **`CompactSignal` 仅为建议**——调用方决定何时执行压缩

### 工具系统 (`tools/`)

**层次架构**：

```text
tools/
  ├── Tool trait (tool.rs)          — 定义统一的工具接口
  ├── ToolRegistry (registry.rs)    — HashMap<名称, Arc<dyn Tool>> 存储与分发
  ├── WorkspaceFs (fs.rs)           — 沙箱文件系统层，所有文件工具共享
  ├── 错误类型 (error.rs)            — ToolError（LLM 可见）+ FsError（I/O 层）
  └── 9 个内置工具
      ├── CalculatorTool             — 递归下降表达式求值（Lexer → Parser 分离）
      ├── EchoTool                   — 最小参考实现
      ├── ShellTool                  — Shell 命令执行，超时控制 + 用户确认 (Y/n)
      ├── ReadTool                   — cat -n 风格带行号输出
      ├── WriteTool                  — 创建/覆盖，自动创建父目录
      ├── EditTool                   — 行级替换（1-indexed 闭区间）
      ├── GlobTool                   — 文件模式匹配
      ├── GrepTool                   — 正则内容搜索
      └── LsTool                     — 目录列表（类型/大小显示）
```

**Tool trait** — 同步、对象安全，无需 `async_trait`：

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;                // schemars 自动生成，构造时缓存
    fn execute(&self, args: &str) -> Result<String, ToolError>;
    fn to_def(&self) -> ToolDef { .. }            // 提供默认实现
}
```

**集成链路**：

```text
Args struct (JsonSchema + Deserialize) → generate_schema() → 缓存在工具中 → parameters() clone 返回
Tool impl → to_def() → ToolDef → DeepSeekRequest.tools
Tool impl ← ToolRegistry::execute(name, args) ← serde_json::from_str::<Args>()  ← ToolCall.function.{name, arguments}
```

**工具参数模式**：每个工具定义带有 `#[derive(JsonSchema, Deserialize)]` 的参数结构体，通过 `generate_schema::<Args>()` 在构造时生成一次 JSON Schema 并缓存，`execute()` 中使用 `serde_json::from_str::<Args>()` 进行类型化反序列化。

**WorkspaceFs 沙箱**：

所有文件工具持有 `Arc<WorkspaceFs>`。`resolve()` 方法会规范化路径并拒绝任何超出 `workspace_root` 的访问。`normalize_partial()` 辅助函数处理不存在的路径（向上查找到第一个存在的祖先目录）。`FsError` 映射为 `ToolError` 后展示给 LLM。

### TUI 交互界面 (`tui/`)

ratatui + crossterm 实现的终端聊天界面，行为上参考了 Claude Code 的交互体验。

**通道拓扑** — 单 Tokio 运行时，主线程同步 TUI 循环，后台 `tokio::spawn` 管理 Agent 生命周期：

```text
TUI 线程                                 Agent 任务 (tokio::spawn)
─────────                               ────────────────────────
cmd_tx ───── TuiCommand ──────────────→ cmd_rx
agent_rx ←── AgentEvent ─────────────── agent_tx
```

**`ChatMessage` 变体（8 种）**：`User`, `Assistant`, `Reasoning`, `ToolCall { id, name, args, state }`, `System`, `ShellConfirm { tool_call_id, command, responded }`, `ShellOutput { command, state: ShellOutputState }`, `Error` —— 每种在 `ui.rs` 中有独立渲染样式。`ShellOutputState` 用于 `!command` 前缀：`Running` 显示黄色 "Running…"，`Complete(output)` 显示实际输出。

**`apply_event` 流式状态机**：

| 事件 | 行为 |
| ---- | ---- |
| `Token(t)` | 追加到最后一条 `Assistant` 消息（或创建新的） |
| `ReasoningToken(t)` | 追加到最后一条 `Reasoning` 消息（或创建新的） |
| `ToolCallStart { id, name }` | 创建新 `ToolCall { state: Running }` |
| `ToolCallArgsDelta { id, delta }` | 按 id 查找，追加 args |
| `ToolResult { id, name, output }` | 按 id 查找，设置 `state: Complete(output)` |
| `ConfirmShell { tool_call_id, command }` | 创建 `ShellConfirm { responded: false }`，触发用户确认提示 |
| `ShellRunning { command }` | 创建 `ShellOutput { state: Running }`（`!command` 即时反馈） |
| `ShellOutput { command, output }` | 查找匹配的 Running 条目，更新为 `Complete(output)` |
| `Done` | 设置 `streaming = false` |

**键盘操作**：Enter（发送）、Ctrl+C（取消/退出）、Esc（取消）、Ctrl+D（输入为空时退出）、PgUp/PgDown（滚动）、↑/↓（历史）、←/→/Home/End（光标移动）、Y/n（批准/拒绝 Shell 命令）。`!<命令>` 前缀直接在 TUI 中执行 Shell 命令，输出实时显示并注入到 Agent 对话上下文；`!!` 作为转义符，允许以 `!` 开头的普通文本。

**斜杠命令**（本地处理）：`/exit`, `/new`, `/save <name>`, `/resume [name]`, `/threads`, `/stats`, `/tools`, `/help`。`/resume`（无参数）打开居中弹窗选择器（↑↓ 导航，Enter 选择，Esc 取消），列出所有已保存的对话线程。`/resume <name>` 直接恢复指定线程。`/new` 在清空对话前自动保存当前线程（以首条用户消息命名）。**Bang 前缀**（`!<命令>`）：在沙箱根目录下异步执行 Shell 命令，先即时显示 "Running…" 后替换为捕获的 stdout/stderr。输出作为 User 消息注入 `SharedMemory`，Agent 在后续对话轮次中可以"看到"命令结果。

**事件循环**：50ms 轮询间隔，每帧后 drain agent 事件，收集到 `Vec` 后在 `terminal.draw()` 释放不可变借用后应用，确保渲染流畅。

### 关键设计模式总结

| 模式 | 应用位置 | 说明 |
| ---- | -------- | ---- |
| 向前兼容枚举 | `FinishReason::Other(String)` | 捕获未知值，反序列化不失败 |
| 两阶段操作 | `drain_for_compact` → `apply_compact` | Memory 与 LLM 提供者解耦 |
| 沙箱隔离 | `WorkspaceFs::resolve()` | 路径规范化 + 工作区边界检查 |
| 通道桥接 | `mpsc::unbounded_channel` | TUI 主线程 ↔ Agent 异步任务 |
| 批量锁操作 | Agent::execute_all | 工具结果收集后一次写锁写入 |
| 异步确认握手 | `PendingConfirmations` + oneshot | Shell 命令执行前的用户审批（Agent 暂停 → TUI 提示 → 用户 Y/n → Agent 继续） |
| `!command` 异步执行 | `TuiCommand::RunShell` → `spawn_blocking` → `AgentEvent::ShellRunning/ShellOutput` | 用户命令通过 channel 流转到后台线程执行，即时显示状态，输出注入 Memory |
| Watchdog 早退信号 | `Arc<AtomicBool>` | Watchdog 线程每 100ms 检查标志位，命令完成即刻退出，不阻塞 join() |
| Windows 管道编码 | `decode_stdout()`: UTF-8 → `GetACP()` → `MultiByteToWideChar()` | 解决 cmd 管道输出 GBK/CP936 中文乱码，无外部依赖 |
| 指数退避重试 | `stream_with_retry` | 瞬时网络错误的健壮性保障 |
| 对话持久化 | `save_conversation` / `load_conversation` / `generate_thread_name` | 每次 Agent 回合后自动保存到 `.loomis/threads/`（JSON + Markdown），首条用户消息自动命名线程，`/resume` 弹窗选择器恢复 |

## 开发路线

### 第一阶段 — MVP ✅

- [x] DeepSeek API 客户端 + SSE 流式响应
- [x] Agent 主循环：批量 + 流式两种模式，`max_steps` 保护，工具自动分发
- [x] 工具系统：`Tool` trait + `ToolRegistry`，9 个内置工具，`WorkspaceFs` 沙箱，Shell 命令用户确认
- [x] 记忆模块：可配置阈值，两阶段压缩
- [x] 终端 UI：ratatui 聊天界面，流式 Token，工具调用卡片，斜杠命令（`/exit`, `/new`, `/save`, `/resume`, `/threads`, `/stats`, `/tools`, `/help`）
- [x] 对话持久化：`.loomis/threads/` 多线程保存/恢复，JSON + Markdown 双格式，首条消息自动命名，`/resume` 选择器弹窗

### 第二阶段 — 进阶

- [x] `schemars` 驱动的 `#[derive(JsonSchema)]` 自动生成工具 JSON Schema
- [ ] Jinja 风格提示词模板引擎
- [ ] 结构化输出（`response_format` → 类型反序列化）
- [ ] 流式交互体验打磨

### 第三阶段 — 生产级

- [ ] RAG + 向量数据库（Qdrant / pgvector）
- [ ] HTTP API + 桌面客户端（Axum / Tauri）
- [ ] `tracing` 全链路可观测性埋点

## 参与贡献

欢迎提交 Issue 和 PR！项目处于活跃开发阶段，任何 bug 修复、功能改进或文档完善都很有价值。

```bash
git checkout -b feature/your-feature
cargo test
cargo clippy
# ... 提交、推送、创建 PR
```

## 许可证

MIT © 2025
