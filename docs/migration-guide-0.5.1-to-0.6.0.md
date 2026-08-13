# Migration Guide: 0.5.1 → 0.6.0

How to upgrade downstream crates from `agent_oxide` 0.5.1 to 0.6.0.

> **This is a living document.** It is maintained alongside the
> `[Unreleased] → 0.6.0` section of `CHANGELOG.md` — every breaking change
> that lands on the road to 0.6.0 gets an entry here. Check back before
> you cut a release.

## Summary

| Change | Breaking? | Section |
| ------ | --------- | ------- |
| `PersistenceHook` generic over the LLM provider | Yes | [PersistenceHook is now generic](#persistencehook-is-now-generic) |
| `PersistenceHook::new` parameter `flash_model` → `title_model` | No (positional) | [flash_model renamed](#flash_model-renamed) |
| `DEFAULT_MODEL` moved to the `deepseek` module | No | [DEFAULT_MODEL relocation](#default_model-relocation) |
| Sandbox execution layers ship in-library (`ShellTool` + `ShellRunner`) | No (recommended migration) | [Sandbox execution layers](#sandbox-execution-layers) |
| Env sanitizer no longer passes `PYTHONPATH` / `NODE_PATH` / `RUSTC_WRAPPER` | Behavioral | [Env sanitizer narrowed](#env-sanitizer-narrowed) |

---

## PersistenceHook is now generic

`PersistenceHook` hardcoded `DeepSeekClient` in 0.5.1, which contradicted
the framework's own layering rule — every extension is provider-agnostic
(`SubagentTool<C>`, `MacroCompactHook<C>`, …). It is now
`PersistenceHook<C: LLMClient>` and works with **any** client that
implements `LLMClient`.

### Call sites using `PersistenceHook::new(...)`

**No changes needed.** The constructor arguments are positional, so the
generic parameter is inferred from the client argument:

```rust,ignore
// 0.5.1 — client must be DeepSeekClient
let hook = PersistenceHook::new(
    workspace_root,
    config,
    DeepSeekClient::new(api_key),
    "deepseek-chat".to_string(),
);

// 0.6.0 — identical call; client can be any LLMClient
let hook = PersistenceHook::new(
    workspace_root,
    config,
    client, // e.g. your own OpenAI-compatible client
    "deepseek-chat".to_string(),
);
```

### Type positions using the bare name `PersistenceHook`

These now need a type parameter (or a trait object):

```rust,ignore
// 0.5.1
struct MyApp {
    hook: PersistenceHook,
}

// 0.6.0 — option A: propagate the generic
struct MyApp<C: LLMClient> {
    hook: PersistenceHook<C>,
}

// 0.6.0 — option B: erase the type (AgentHook is object-safe)
struct MyApp {
    hook: Box<dyn AgentHook>,
}
```

Option B is recommended when you store heterogeneous hooks in one
collection: `.hook()` / `BuildConfig::hook()` already accept
`impl AgentHook + 'static`, so `Box::new(hook)` just works.

### flash_model renamed

The `PersistenceHook::new` parameter `flash_model` is now `title_model`
— "flash model" leaked an OpenAI naming convention into a
provider-agnostic API:

```rust,ignore
// 0.5.1
PersistenceHook::new(root, config, client, "deepseek-chat".into()) // flash_model

// 0.6.0
PersistenceHook::new(root, config, client, "deepseek-chat".into()) // title_model
```

Positional call sites are unaffected; only code that named the parameter
(e.g. inside docs, IDE-generated snippets, or future keyword-argument
refactors) needs the rename. The field was private, so no struct-literal
breakage exists.

---

## DEFAULT_MODEL relocation

The fallback model used by generation methods when no `#[agent(model)]`
field is present moved from the generic `agent_kit` layer to the vendor
module:

```rust,ignore
// 0.5.1
agent_oxide::agent_kit::DEFAULT_MODEL   // defined in agent_kit

// 0.6.0
agent_oxide::deepseek::DEFAULT_MODEL    // canonical home (vendor module)
agent_oxide::agent_kit::DEFAULT_MODEL   // still works — re-export, kept for compatibility
```

**Not breaking** — macro-generated code keeps resolving through the
re-export. Two recommendations for downstream crates:

1. Prefer `#[agent(model = "...")]` (or an explicit `model` argument)
   over the library default — a vendor default in a provider-agnostic
   agent is rarely the right model for your use case.
2. If you reference `DEFAULT_MODEL` directly, switch to the
   `agent_oxide::deepseek::` path so the re-export can eventually be
   removed.

---

## Sandbox execution layers

In 0.5.1 the sandbox's execution layers — env sanitization, the
watchdog, output decoding/truncation — had **zero call sites in the
library**. Downstream crates hand-wired them inside their own shell
tool. 0.6.0 ships the full chain composed as `ShellTool` +
`ShellRunner`, so downstream code shrinks to registering the library
tool:

```rust,ignore
// 0.5.1 — hand-rolled shell tool wiring (downstream)
impl Tool for MyShellTool {
    fn execute_stream(&self, args: &str) -> Result<ProgressStream, ToolError> {
        let command = parse(args)?;
        let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "sh" });
        cmd.current_dir(&workspace);
        sanitize(&mut cmd, &workspace, config.sanitize_environment); // layer 4
        let child = cmd.spawn()?;
        let watchdog = Watchdog::spawn(child.id(), timeout);          // layer 5
        let output = child.wait_with_output()?;
        watchdog.disarm();
        let text = truncate_output(&decode_stdout(&output.stdout), MAX_OUTPUT_BYTES);
        Ok(ProgressStream::done(text))
    }
}

// 0.6.0 — register the library tool; the full chain comes with it
let agent = Agent::builder(client, model)
    .tool(ShellTool::from_config(workspace_root, &sandbox_config))
    .hook(SandboxHook::new(...)) // approval layer, unchanged
    .build();
```

What the library chain now covers, enforced in one place:

- **Second-pass policy check** — `ShellTool` re-runs `ShellFilter`
  before execution (sandbox check #13). Default mode is
  `ToolApprovalMode::BlockOnly` (correct when `SandboxHook` is in the
  hook chain — the hook already prompted for `RequiresApproval`
  commands). Deployments using `ShellTool` **without** `SandboxHook`
  must use `ToolApprovalMode::DenyUnapproved`, which also refuses
  `RequiresApproval` commands.
- **Env sanitization + timeout tree-kill + bounded capture** —
  `ShellRunner` spawns `cmd /D /S /C` (AutoRun registry hook disabled)
  or `sh -c` in its own process group, and caps stdout/stderr **at read
  time** (overflow kills the tree — no more multi-GB buffering before
  truncation).

For user-initiated `!command` execution (not via the `Tool` trait),
`ShellRunner::run` is the direct, blocking API — it is deliberately
policy-free, so run your own `ShellFilter::classify` on the command
first.

New exports: `agent_oxide::sandbox::{ShellTool, ShellRunner,
ShellOutput, ShellRunnerError, ToolApprovalMode}`.

## Env sanitizer narrowed

The sanitized-environment allowlist no longer passes `PYTHONPATH`,
`NODE_PATH`, or `RUSTC_WRAPPER` to child processes — all three load
code on interpreter/compiler startup and are classic injection
vectors.

**Behavioral change:** if your harness relied on `PYTHONPATH` /
`NODE_PATH` reaching sandboxed `python` / `node` invocations (e.g.
project-local packages), those imports stop resolving. The supported
alternative is `workspace_root/bin`, which the sanitizer already
prepends to `PATH` — put wrappers there, or relax the allowlist in
your own copy of the policy. `GOPATH`, `JAVA_HOME`,
`NPM_CONFIG_USERCONFIG`, and `CARGO_HOME` remain allowed.
