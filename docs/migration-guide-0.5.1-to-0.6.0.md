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
