# Agent Portal

A native desktop portability layer for coding-agent sessions. Agent Portal is a task board that shows every installed coding agent (Claude Code, Codex CLI, OpenCode, …) and the sessions each one is working on — and lets you **migrate a session from one agent to another** by dragging its card, either as a native session conversion (the target agent resumes it with full history) or as a deterministic handoff brief.

Not an AI app: no LLM calls, no conversation hosting. It manages the multi-agent work you already do.

## Stack

- **Tauri 2** — Rust core: adapters, session parsing, migration engine, fs watching, terminal launch
- **Angular** (standalone + signals + CDK drag-drop) — the board UI
- Cargo workspace: `crates/portal-core` (agent-agnostic domain), `crates/portal-adapters` (one module per agent), `src-tauri` (shell)

## Development

```sh
pnpm install && pnpm --dir ui install
pnpm dev            # tauri dev (starts Angular dev server + Rust app)
pnpm gen-types      # regenerate TS bindings from Rust DTOs (ts-rs)
cargo test --workspace
```

Generated TS bindings live in `ui/src/app/core/ipc/gen/` and are committed; CI fails on drift.
