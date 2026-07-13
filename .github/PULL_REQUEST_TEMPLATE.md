<!-- Thanks for contributing! Keep PRs focused on a single change. -->

## What & why

<!-- What does this change, and what problem does it solve? Link any related issue (e.g. "Closes #123"). -->

## How it was verified

<!-- Tests added/updated, and/or how you exercised the change in the running app. -->

## Checklist

- [ ] `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` are clean
- [ ] `cargo test --workspace` and `pnpm --dir ui build` pass
- [ ] `node scripts/check-command-parity.mjs` passes (if commands changed)
- [ ] Regenerated TS bindings are committed (`pnpm gen-types`, if a DTO changed)
- [ ] Docs/README updated if behavior or setup changed
