#!/usr/bin/env node
// Verify the Tauri command surface stays in sync across the two hand-maintained
// lists: the Rust `generate_handler!` registration and the typed Angular
// wrappers. A command present in one but not the other is a bug that only shows
// up at runtime, so we catch it in CI instead.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');

// Commands intentionally registered without an Angular wrapper — invoked from
// the native side (tray, deep links) rather than the board UI.
const NATIVE_ONLY = new Set(['show_main_window']);

// Rust: names inside `tauri::generate_handler![ ... ]`, as `commands::<name>`.
const libRs = readFileSync(join(root, 'src-tauri/src/lib.rs'), 'utf8');
const handler = libRs.match(/generate_handler!\s*\[([\s\S]*?)\]/);
if (!handler) {
  console.error('could not find generate_handler![ ... ] in src-tauri/src/lib.rs');
  process.exit(2);
}
const rust = new Set([...handler[1].matchAll(/commands::([a-z_]+)/g)].map((m) => m[1]));

// TS: the first string argument of every `this.tauri.invoke<...>('<name>'...)`.
const commandsTs = readFileSync(join(root, 'ui/src/app/core/ipc/commands.ts'), 'utf8');
const ts = new Set([...commandsTs.matchAll(/invoke<[^>]*>\(\s*'([a-z_]+)'/g)].map((m) => m[1]));

const missingInTs = [...rust].filter((c) => !ts.has(c) && !NATIVE_ONLY.has(c)).sort();
const missingInRust = [...ts].filter((c) => !rust.has(c)).sort();

if (missingInTs.length === 0 && missingInRust.length === 0) {
  console.log(`command parity OK — ${rust.size} commands match`);
  process.exit(0);
}
if (missingInTs.length) {
  console.error(`Registered in Rust but no TS wrapper: ${missingInTs.join(', ')}`);
}
if (missingInRust.length) {
  console.error(`Called from TS but not registered in Rust: ${missingInRust.join(', ')}`);
}
process.exit(1);
