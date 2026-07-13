# Contributing to Agent Portal

Thanks for your interest in improving Agent Portal! This guide covers the
practical bits: setup, the checks CI enforces, and how to add a new agent.

## Getting started

See the [Prerequisites](README.md#prerequisites) in the README (Rust, Node 20+,
pnpm 10, and the Tauri system dependencies). Then:

```sh
pnpm install && pnpm --dir ui install
pnpm dev            # tauri dev: Angular dev server + the Rust app
```

## Before you open a PR

CI runs on every pull request and must pass. Run the same checks locally first —
they are fast and catch almost everything:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm --dir ui build
node scripts/check-command-parity.mjs
```

A few things that trip people up:

- **Generated TypeScript bindings are committed.** Rust DTOs derive
  [`ts-rs`](https://github.com/Aleph-Alpha/ts-rs) types into
  `ui/src/app/core/ipc/gen/`. If you change a DTO, run `pnpm gen-types` and
  commit the regenerated files — CI fails on drift.
- **Tauri commands come in pairs.** Every command in `src-tauri`'s
  `generate_handler!` needs a typed wrapper in
  `ui/src/app/core/ipc/commands.ts` (native-only commands are allow-listed in
  `scripts/check-command-parity.mjs`). The parity check enforces this.
- Keep new code consistent with the surrounding style — match the existing
  naming, comment density, and module layout rather than introducing new
  patterns.

## Adding an agent adapter

Each agent is one module under `crates/portal-adapters/src/`, registered in that
crate's `builtin_adapters()`. Implement the `AgentAdapter` trait, add fixtures,
and make the conformance suite pass. `crates/portal-core` holds the
agent-agnostic domain (the IR, migration engine, DTOs) — adapters translate a
specific agent's on-disk format to and from that IR. Look at an existing adapter
(`claude_code` is the most complete) as a template.

## Pull request process

- Branch off `main`; keep PRs focused on a single change.
- `main` is protected: PRs need CI green and one approving review before merge.
- Write a clear description of what changed and why. If it changes behavior,
  say how you verified it.
- By contributing, you agree your work is licensed under the project's
  [MIT license](LICENSE).

## Reporting bugs and requesting features

Use the issue templates. For security issues, do **not** open a public issue —
see [SECURITY.md](SECURITY.md).
