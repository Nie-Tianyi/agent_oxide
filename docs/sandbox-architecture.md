# 沙箱架构文档

本文档描述了 Loomis 中一个 LLM 工具调用从发起到执行完毕所经过的完整安全检查链。

---

## 一图总览

![沙箱权限检查总览](./assets/鉴权检查.jpg)

---

## 第零步：Agent Loop 调度

代码位置：[`core/engine/src/agent.rs`](../core/engine/src/agent.rs#L217-L269)
![Agent Loop权限检查流程](./assets/sandbox.png)

当前注册的唯一 Hook 是 `SandboxHook`

---

## 第一步：SandboxHook::before_tool_call()

代码位置：[`extensions/sandbox/src/sandbox_hook.rs`](../extensions/sandbox/src/sandbox_hook.rs)

![before_tool_call工具调用权限检查](./assets/pre-hook.png)

### ShellFilter 分类优先级

```
分类优先级（从上到下，命中即停止）:

1. Strict allowlist   →  binary 不在白名单? → Blocked
2. Deny patterns      →  完整命令匹配正则?   → Blocked
3. Auto-approve       →  命令前缀匹配?       → AutoApproved
4. Fallthrough        →  以上都不匹配        → RequiresApproval
```

代码位置：[`bins/loomis/src/sandbox/shell_filter.rs`](../bins/loomis/src/sandbox/shell_filter.rs#L77-L117)

---

## 第二步：工具执行阶段

### 文件工具 (read / write / edit / glob / grep / ls)

所有文件工具共享 `Arc<WorkspaceFs>`，在执行阶段经过以下检查：

![文件工具权限检查](./assets/toolcall.png)

代码位置：[`extensions/sandbox/src/fs.rs`](../extensions/sandbox/src/fs.rs)

### Shell 工具 (shell)

![Shell调用权限检查](./assets/shellexe.png)

代码位置：
- ShellTool: [`bins/loomis/src/tools/shell.rs`](../bins/loomis/src/tools/shell.rs)
- EnvSanitizer: [`bins/loomis/src/sandbox/env_sanitizer.rs`](../bins/loomis/src/sandbox/env_sanitizer.rs)

---

## 第三步：SandboxHook::after_tool_call()

![记录审查结果](./assets/auditlogger.png)

代码位置：
- ResourceTracker: [`bins/loomis/src/sandbox/resource_tracker.rs`](../bins/loomis/src/sandbox/resource_tracker.rs)
- AuditLogger: [`bins/loomis/src/sandbox/audit_logger.rs`](../bins/loomis/src/sandbox/audit_logger.rs)

---

## 检查清单

一个 LLM 发起的工具调用经过以下**全部检查**才能成功执行：

| # | 在哪 | 检查内容 | 失败后果 |
|---|------|----------|----------|
| 1 | `ResourceTracker` | 会话总操作数是否超配额 | 调用被拒绝 |
| 2 | `ResourceTracker` | 并发 Shell 数是否超上限 | 调用被拒绝 |
| 3 | `ShellFilter` | 命令是否在严格白名单内 | 直接阻止 |
| 4 | `ShellFilter` | 命令是否匹配 deny_pattern | 直接阻止 |
| 5 | `ShellFilter` | 命令是否需要用户确认 | TUI 弹窗 |
| 6 | `WorkspaceFs::resolve` | 路径是否逃逸工作区 | InvalidArgs |
| 7 | `WorkspaceFs::resolve` | TOCTOU 二次校验 | PathEscapesWorkspace |
| 8 | `WorkspaceFs::read` | 文件大小 ≤ max_read_bytes | FileTooLarge |
| 9 | `WorkspaceFs::write` | 内容大小 ≤ max_write_bytes | FileTooLarge |
| 10 | `WorkspaceFs::write` | 扩展名不在黑名单 | ExtensionBlocked |
| 11 | `WorkspaceFs::write` | 内容无 NULL 字节 | BinaryContentDetected |
| 12 | `WorkspaceFs::write` | 非隐藏文件 (.开头) | HiddenFileBlocked |
| 13 | `ShellFilter` (二次) | ShellTool 执行前再分类 | ToolError::Execution |
| 14 | `EnvSanitizer` | 环境变量清洗 | (不失败，但限制暴露面) |
| 15 | `Watchdog` | 进程超时杀 | 进程终止 |
| 16 | 输出截断 | stdout+stderr ≤ 100KB | 截断并标记 |

检查 3-5 的决策由 `.loomis/config.toml` 控制：

```toml
[sandbox.shell.auto_approve]
prefixes = ["cargo", "git", "npm", ...]

[sandbox.shell.deny_patterns]
patterns = ["rm -rf\\s+(/|~)", "sudo\\s+", "shutdown", ...]

[sandbox.shell.allowed_commands]
# binaries = ["cargo", "git"]  # 取消注释启用严格白名单
```

---

## 关键设计原则

1. **双重防线** — Hook 层做策略判断（允许/阻止/询问），工具层做技术强制执行。即使 Hook 层被绕过（例如未来新增其他 Hook），工具层的 ShellFilter 仍会拦截。

2. **Fail closed** — 任何检查失败都阻止执行。配置缺失时使用最严格的安全默认值。

3. **纵深防御** — ShellFilter 在 Hook 层 (`before_tool_call`) 和 ShellTool 层 (`execute`) 各执行一次，互为备份。

4. **全链路审计** — 从分类 → 决策 → 执行 → 结果，每一步都记录到 `.loomis/audit.jsonl`，事后可追溯。

5. **同步执行** — `Tool::execute` 和 `AgentHook` 方法都是同步的。Shell 命令阻塞 tokio worker 线程直到完成（或超时）。对于短命令（<30s）这是可接受的；长期来看可以迁移到 `spawn_blocking`。

6. **配置即策略** — 所有安全检查的行为都不硬编码在代码中，而是由 `SandboxConfig` 驱动。用户可以通过 `.loomis/config.toml` 调节安全等级。
