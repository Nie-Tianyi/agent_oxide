# Security Policy

Agent Oxide takes security seriously. This framework executes LLM-generated
tool calls (including shell commands) and ships a sandbox whose job is to
contain that execution — so bugs in the sandbox are treated as security
issues.

## Supported versions

| Version | Supported |
| ------- | --------- |
| latest release | ✅ |
| older releases | ⚠️ fixes backported on a best-effort basis |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report vulnerabilities privately via GitHub's Private Vulnerability
Reporting:

- https://github.com/Nie-Tianyi/agent_oxide/security/advisories/new

You can expect:

- **Acknowledgement** within 3 business days.
- **A triage assessment** within 1 week, including severity and whether the
  report is accepted.
- **A coordinated fix** — a patch release for the latest version, plus a
  public advisory once the fix is released.

Include, when possible:

- Which crate and version are affected.
- A minimal reproduction (code or description of the scenario).
- Impact assessment (e.g. sandbox escape, prompt injection vector, denial
  of service).

## Security-relevant areas

The following components are in-scope for security review:

- `src/sandbox` — path canonicalization, shell filtering, env
  sanitization, watchdog, audit log.
- `src/subagent` — tool filtering and child-agent isolation.
- `src/engine` — prompt construction and memory handling.
- Any code path that spawns processes or writes files based on LLM output.

## Disclosure policy

- Security fixes are released as patch versions of the affected crates.
- Advisories are published on GitHub and crates.io after the fix is
  available.
- If you reported the issue, you will be credited (unless you prefer not
  to be).
