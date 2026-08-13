# Sandbox Architecture

This document describes the complete security-check chain that an LLM tool call passes through — from initiation to execution — in Agent Oxide.

---

## Overview

![Sandbox permission checks overview](./assets/鉴权检查.jpg)

---

## Step 0: Agent Loop Dispatch

Code: [`src/core/engine/agent.rs`](../src/core/engine/agent.rs#L217-L269)
![Agent Loop permission-check flow](./assets/sandbox.png)

The only hook currently registered is `SandboxHook`.

---

## Step 1: SandboxHook::before_tool_call()

Code: [`src/extensions/sandbox/sandbox_hook.rs`](../src/extensions/sandbox/sandbox_hook.rs)

![before_tool_call tool-call permission check](./assets/pre-hook.png)

### ShellFilter classification priority

```text
Classification priority (top to bottom, first match wins):

1. Strict allowlist   →  binary not in allowlist? → Blocked
2. Deny patterns      →  full command matches regex? → Blocked
3. Auto-approve       →  command prefix matches?  → AutoApproved
4. Fallthrough        →  nothing above matched   → RequiresApproval
```

Code: [`src/extensions/sandbox/shell_filter.rs`](../src/extensions/sandbox/shell_filter.rs#L77-L117)

---

## Step 2: Tool Execution Phase

### File tools (read / write / edit / glob / grep / ls)

All file tools share an `Arc<WorkspaceFs>` and go through the following checks during execution:

![File tool permission checks](./assets/toolcall.png)

Code: [`src/extensions/sandbox/fs.rs`](../src/extensions/sandbox/fs.rs)

### Shell tool (shell)

![Shell call permission check](./assets/shellexe.png)

Code:

- ShellTool: [`src/extensions/sandbox/shell_tool.rs`](../src/extensions/sandbox/shell_tool.rs) — the in-library `shell` tool; downstream crates register it instead of hand-wiring the execution chain
- ShellRunner: [`src/extensions/sandbox/shell_runner.rs`](../src/extensions/sandbox/shell_runner.rs) — composed execution chain: env sanitize → tree watchdog → bounded capture → decode/truncate
- EnvSanitizer: [`src/extensions/sandbox/env_sanitizer.rs`](../src/extensions/sandbox/env_sanitizer.rs)

```text
ShellTool.execute_stream
  ├─ strict arg parse (fail closed on malformed JSON)
  ├─ second-pass ShellFilter.classify   ← check #13
  └─ background thread → ShellRunner.run
       ├─ cmd /D /S /C <cmd> (AutoRun disabled) | sh -c <cmd> (own process group)
       ├─ env sanitize                  ← check #14
       ├─ Watchdog::spawn_tree          ← check #15 (timeout kill)
       ├─ bounded stdout/stderr capture ← check #16 (budget kill, no unbounded reads)
       └─ decode + truncate
```

---

## Step 3: SandboxHook::after_tool_call()

![Recording the review result](./assets/auditlogger.png)

Code:

- ResourceTracker: [`src/extensions/sandbox/resource_tracker.rs`](../src/extensions/sandbox/resource_tracker.rs)
- AuditLogger: [`src/extensions/sandbox/audit_logger.rs`](../src/extensions/sandbox/audit_logger.rs)

---

## Check Checklist

A tool call initiated by the LLM must pass **all** of the following checks before it can execute:

| # | Where | Check | Failure result |
| --- | --- | --- | --- |
| 1 | `ResourceTracker` | Total operations for the session within quota | Call rejected |
| 2 | `ResourceTracker` | Concurrent shell count under the limit | Call rejected |
| 3 | `ShellFilter` | Command in the strict allowlist | Blocked immediately |
| 4 | `ShellFilter` | Command matches a deny_pattern | Blocked immediately |
| 5 | `ShellFilter` | Command requires user confirmation | TUI prompt |
| 6 | `WorkspaceFs::resolve` | Path does not escape the workspace | InvalidArgs |
| 7 | `WorkspaceFs::resolve` | TOCTOU re-check | PathEscapesWorkspace |
| 8 | `WorkspaceFs::read` | File size ≤ max_read_bytes | FileTooLarge |
| 9 | `WorkspaceFs::write` | Content size ≤ max_write_bytes | FileTooLarge |
| 10 | `WorkspaceFs::write` | Extension not on the blocklist | ExtensionBlocked |
| 11 | `WorkspaceFs::write` | No NUL bytes in content | BinaryContentDetected |
| 12 | `WorkspaceFs::write` | Not a hidden file (starts with `.`) | HiddenFileBlocked |
| 13 | `ShellFilter` (second pass, in `ShellTool`) | Re-classified before execution | ToolError::Execution |
| 14 | `EnvSanitizer` (in `ShellRunner`) | Environment sanitization (`cmd /D` also disables the AutoRun registry hook) | (does not fail, but limits the attack surface) |
| 15 | `Watchdog::spawn_tree` (in `ShellRunner`) | Process **tree** killed on timeout (Unix: process group) | Process terminated |
| 16 | Bounded capture + truncation (in `ShellRunner`) | stdout + stderr are capped **at read time** (budget exceeded → tree killed), then truncated | Truncated and marked |

Checks 13–16 are now enforced **in-library** by `ShellTool` / `ShellRunner` —
registering `ShellTool` wires them automatically; no downstream hand-wiring.

Checks 3–5 are controlled by `.agent/config.toml`:

```toml
[sandbox.shell.auto_approve]
prefixes = ["cargo", "git", "npm", ...]

[sandbox.shell.deny_patterns]
patterns = ["rm -rf\\s+(/|~)", "sudo\\s+", "shutdown", ...]

[sandbox.shell.allowed_commands]
# binaries = ["cargo", "git"]  # uncomment to enable the strict allowlist
```

---

## Key Design Principles

1. **Two lines of defense** — the hook layer makes policy decisions (allow / block / ask), while the tool layer enforces the policy technically. Even if the hook layer is bypassed (for example by adding other hooks in the future), the tool layer's ShellFilter still intercepts.

2. **Fail closed** — any check failure blocks execution. When configuration is missing, the strictest safe defaults are used.

3. **Defense in depth** — ShellFilter runs once in the hook layer (`before_tool_call`) and once in the ShellTool layer (`execute`), acting as backups for each other. The tool-layer pass defaults to `ToolApprovalMode::BlockOnly` (only `Blocked` verdicts are refused — the hook already prompted for `RequiresApproval` commands); deployments that register `ShellTool` **without** `SandboxHook` must use `ToolApprovalMode::DenyUnapproved`, which also refuses `RequiresApproval` commands.

4. **Full-chain auditing** — from classification → decision → execution → result, every step is recorded to `.agent/audit.jsonl` for later traceability.

5. **Non-blocking execution** — `Tool::execute` and `AgentHook` methods are synchronous, but `ShellTool` runs the command on a background thread and streams progress over a channel (same pattern as `SubagentTool`) — the tokio worker driving the tool loop stays free, and `InProgress` events reach the TUI live. `ShellRunner::run` itself is a blocking, synchronous API for callers who want direct use (e.g. user `!command` execution).

6. **Configuration as policy** — the behavior of every security check is not hardcoded but driven by `SandboxConfig`. Users can tune the security level through `.agent/config.toml`.
