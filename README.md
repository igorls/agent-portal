# Agent Portal

A native desktop portability layer for coding-agent sessions. Agent Portal is a
task board that shows every installed coding agent and the sessions each one is
working on — and lets you **migrate a session from one agent to another** by
dragging its card, either as a native session conversion (the target agent
resumes it with full history) or as a deterministic handoff brief.

It manages the multi-agent work you already do. It is not a chat app and it does
not host conversations. The only model calls it ever makes are **local and
optional** (a local [Ollama](https://ollama.com) server, for brief polishing and
background session titling) — your sessions are never sent to any cloud service.

## Supported agents

| Agent | Read | Native write | Notes |
| --- | :---: | :---: | --- |
| Claude Code | ✓ | ✓ | JSONL transcripts |
| Codex CLI | ✓ | ✓ | JSONL per session |
| OpenCode | ✓ | ✓ | |
| Antigravity (Gemini) | ✓ | — | protobuf + SQLite summaries |
| GitHub Copilot | ✓ | — | VS Code chat sessions |
| Grok | ✓ | — | |

Adding an agent is one module in `crates/portal-adapters` plus one line in its
registry, then making the conformance suite pass.

## Features

- **Board** — every detected agent as a lane, its sessions grouped by project.
- **Migrate** — drag a session onto another agent. Native conversion when both
  sides support it, otherwise a deterministic handoff brief that launches a
  fresh session. A dry-run report shows exactly what will be written first.
- **Activity** — an append-only ledger of every migration, each one undoable
  (undo removes only what the migration created; source sessions are never
  touched).
- **Session naming** — a background worker reads each session's recent activity
  through a local model and gives it a short title. It prioritizes sessions
  touched in the last 24 hours and runs at an adaptive cadence, all watchable
  live from the Activity page. Entirely offline; skipped if Ollama isn't running.

## Stack

- **Tauri 2** — Rust core: adapters, session parsing, migration engine, fs
  watching, terminal launch.
- **Angular** (standalone + signals + CDK drag-drop) — the board UI.
- Cargo workspace: `crates/portal-core` (agent-agnostic domain),
  `crates/portal-adapters` (one module per agent), `src-tauri` (shell).

The Rust ↔ TypeScript boundary is typed end to end: Rust DTOs derive
[`ts-rs`](https://github.com/Aleph-Alpha/ts-rs) bindings that are committed under
`ui/src/app/core/ipc/gen/` and checked for drift in CI.

## Install

Download the installer for your platform from the
[latest release](https://github.com/igorls/agent-portal/releases/latest)
(`.dmg` for macOS, `.AppImage`/`.deb` for Linux, `.msi`/`.exe` for Windows).

On macOS or Linux you can grab and install the latest build in one line:

```sh
curl -fsSL https://raw.githubusercontent.com/igorls/agent-portal/main/scripts/install.sh | bash
```

Builds are currently unsigned, so the first launch may need a Gatekeeper /
SmartScreen override.

## Prerequisites

- [Rust](https://rustup.rs) (stable) and the
  [Tauri 2 system dependencies](https://v2.tauri.app/start/prerequisites/) for
  your OS.
- [Node](https://nodejs.org) 20+ and [pnpm](https://pnpm.io) 10.
- *(optional)* [Ollama](https://ollama.com) for brief polishing and session
  naming. Configure the host and model under Settings.

## Development

```sh
pnpm install && pnpm --dir ui install
pnpm dev            # tauri dev (starts the Angular dev server + Rust app)
pnpm gen-types      # regenerate TS bindings from Rust DTOs (ts-rs)
cargo test --workspace
```

Before opening a PR, keep the checks CI runs green:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm --dir ui build
node scripts/check-command-parity.mjs   # Rust command list ↔ Angular wrappers
```

## License

[MIT](LICENSE) © Igor Lins e Silva
