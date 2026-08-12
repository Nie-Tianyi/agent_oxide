# Contributing to Agent Oxide

Thanks for your interest! This document covers how to build, test, and
submit changes to this repository.

## Prerequisites

- **Rust 1.85 or newer** (the workspace targets `edition = "2024"`; the
  declared MSRV is `1.85`, enforced by CI).
- A working `cargo` toolchain with `rustfmt` and `clippy` components.

## Getting started

```bash
git clone https://github.com/Nie-Tianyi/agent_oxide.git
cd agent_oxide
```

## Common commands

```bash
cargo build --all              # build the whole workspace
cargo test --all               # run all tests
cargo test -p engine <name>    # one crate, or a single test by name
cargo clippy --all --all-targets   # lint everything (CI runs this with -D warnings)
cargo fmt --all --check        # formatting check (CI runs this)
cargo bench --no-run           # verify benches compile
cargo deny check               # license/advisory policy (requires cargo-deny)
```

CI runs `fmt`, `clippy`, `test`, an MSRV build, `cargo audit`, `cargo deny`,
and coverage on every push/PR — make sure these pass locally first.

## Code conventions

- **Rust 2024 edition** with native `async fn` in traits (RPITIT). Do
  **not** use the `async-trait` crate. Prefer sync traits for dyn-dispatch;
  keep async work in dedicated components.
- **Inline tests** — tests live in `#[cfg(test)] mod tests { ... }`
  co-located with the source. No separate `tests/` directories.
- **No `unsafe`** — the umbrella crate is `#![deny(unsafe_code)]`; keep
  new code that way.
- **Doc comments** — public items should carry doc comments; they show up
  on docs.rs and in the crate-level documentation.
- **Proc-macro output paths** — macros expand to absolute paths against
  `agent_oxide` (e.g. `::agent_oxide::tools::`, `::agent_oxide::agent_kit::`).
  When adding a proc macro, document which dependencies consumers must add
  directly.

## Architecture map

The four-layer design (see README): this repository implements the **agent
core** (memory, tools, agent loop, hooks) and the **LLM client** layer
(`LLMClient` trait). The **UI** and **Harness** layers are user-owned, built
on top of these APIs.

- `src/` — one module per subsystem: provider, deepseek, tools, memory,
  util, engine, skills, hooks, persistence, subagent, observability,
  sandbox, agent_kit
- `agent_oxide-macros/` — the proc-macro crate (`#[derive(Agent)]`,
  `#[agent_impl]`, `#[tool]`)
- `docs/` — guides (beginner, senior, sandbox, agent-kit, nooa, harness-ui)

See `docs/` for details; `AGENT_OXIDE.md` is the archived agent-guidance
file (its content lives in the README appendix section).

## Branching and pull requests

- Push directly to `master` for small fixes, or open a pull request for
  anything substantial — either works while the project is early.
- PRs should be small and focused: one logical change per PR, with tests.
- Release notes: when your PR introduces a user-visible change, add a
  matching entry under `[Unreleased]` in `CHANGELOG.md`.

### Commit message style

The repository uses conventional-commit style prefixes:

- `feat:` — new capability
- `fix:` — bug fix
- `docs:` — documentation
- `refactor:` — code change that neither fixes nor adds features
- `test:` — test-only changes
- `chore:` — build, tooling, maintenance

## License

By contributing you agree that your contributions are licensed under the
[MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE) licenses (the
project's dual license).
