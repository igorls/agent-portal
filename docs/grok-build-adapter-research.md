# Grok Build adapter research

Research date: 2026-07-11. This note evaluates the official xAI **Grok Build**
CLI as an Agent Portal adapter target/source. The CLI is an early-beta product,
so its on-disk representation should be treated as version-sensitive.

## Conclusion

Grok Build is a viable adapter. It has all the pieces Agent Portal needs:

- a locally installed `grok` executable;
- automatically persisted, workspace-keyed sessions;
- explicit resume and continue commands;
- a documented Claude Code import path; and
- an ACP JSON-RPC transport (`grok agent stdio`) that is preferable to driving
  the interactive terminal UI.

The first implementation should be **read and resume capable**, then add writes
behind an experimental feature flag. xAI documents importing *into* Grok from
Claude Code and exporting to Markdown, but does not document a stable, general
JSON session-import schema. Therefore, do not synthesize Grok session files as
the normal migration path until they are validated against the installed binary.

## Installation and detection

| Platform | Official installation | Adapter detection |
| --- | --- | --- |
| Windows | `irm https://x.ai/cli/install.ps1 \| iex` | resolve `grok.exe` on `PATH`; this machine resolves it at `%USERPROFILE%\\.grok\\bin\\grok.exe` |
| macOS/Linux/WSL | `curl -fsSL https://x.ai/cli/install.sh \| bash` | resolve `grok` on `PATH` |

The official docs also identify `npm install -g @xai-official/grok` as an
enterprise-supported installation option. Detection should call
`grok version` and retain the version in adapter diagnostics; this local
inspection found `grok 0.2.93 (f00f96316d) [stable]`.

Authentication is interactive-browser based on first launch; unattended
machines can use `XAI_API_KEY`. An adapter must never copy `%USERPROFILE%\\.grok\\auth.json`
or API keys as part of a migration.

Sources: [overview](https://docs.x.ai/build/overview),
[enterprise deployments](https://docs.x.ai/build/enterprise),
[CLI reference](https://docs.x.ai/build/cli/reference).

## Session persistence and local format

xAI documents that every conversation — prompts, responses, tool calls, and
file snapshots — is stored automatically under `~/.grok/sessions/`, keyed by
working directory. On Windows this expands to `%USERPROFILE%\\.grok\\sessions`.
The same behavior applies to TUI, headless, and ACP usage.

Observed on this Windows host with Grok Build 0.2.93 (implementation detail,
not a compatibility promise):

```
%USERPROFILE%\\.grok\\sessions\\
  C%3A%5C...%5C<workspace>\\
    prompt_history.jsonl
    <session-uuid>\\
      chat_history.jsonl
      events.jsonl
      updates.jsonl
      summary.json
      prompt_context.json
      [optional plan_mode.json, rewind_points.jsonl, resources_state.json, ...]

%USERPROFILE%\\.grok\\session_search.sqlite
```

This is attractive for fast local discovery: enumerate workspace directories,
then session UUID directories and only parse small summary/index files first.
However, the files contain credentials-adjacent and system-prompt context in
addition to the transcript. The adapter must use the least data needed and
never migrate auth/configuration files.

Source for the supported location and contents:
[Sessions](https://docs.x.ai/build/features/sessions). The detailed filenames
above are a local observation and need a fixture/compatibility test per Grok
release.

### Local validation: Grok Build 0.2.93

The store on this Windows host was checked across all discoverable primary
sessions, without reading authentication files or emitting prompt bodies:

- 16 workspace directories; every URL-encoded key decoded successfully;
- 39 UUID session directories;
- 39/39 had valid `summary.json` with `info.id` and `info.cwd` strings;
- 39/39 had `chat_history.jsonl`;
- all 6,968 transcript records parsed as JSON, with zero invalid lines;
- 35/39 had generated titles and Git-root metadata (the remaining four were
  minimal `system,user` sessions);
- observed transcript record types were `system`, `user`, `assistant`,
  `reasoning`, `tool_result`, and optional `backend_tool_call`;
- richer sessions also carried `events.jsonl`, `updates.jsonl`, terminal/MCP
  call artifacts, rewind points, plans, and file-hunk records.

This is sufficient evidence for a version-gated read/resume adapter against
0.2.93. `summary.json` should drive board enumeration; full transcript parsing
should touch `chat_history.jsonl` only when the user opens or migrates a session.

## Resuming and launching

Grok supports direct continuation:

```bash
grok --resume <session-id>   # exact session
grok --resume                # latest for the directory
grok -c                      # shorthand for latest in current directory
grok --cwd <project> --resume <session-id>
```

For a branch rather than mutation of the original, add `--fork-session` while
resuming. A supplied `-s, --session-id <UUID>` creates/names a new session; it
does not resume an existing one.

The portal should persist a stable mapping of `CanonicalSession.id` to the
Grok session UUID plus workspace path. Its `resume_command` implementation can
then return `grok --cwd <workspace> --resume <uuid>` (with native argument
passing, not shell string concatenation).

Sources: [Sessions](https://docs.x.ai/build/features/sessions),
[CLI reference](https://docs.x.ai/build/cli/reference).

## Transport and migration capability

For live orchestration, use the documented ACP endpoint:

```bash
grok agent stdio
```

It communicates over stdin/stdout with JSON-RPC and publishes assistant output
as `session/update` chunks. It supports `session/new` and `session/prompt`, so
it is a much more stable integration seam than screen-scraping the TUI.

Headless mode also supports `-p`, `--output-format json` or
`streaming-json`, `--session-id`, `--resume`, and `--continue`. JSON output
exposes the `sessionId`, which allows Agent Portal to capture the ID when it
creates a Grok session.

The official CLI reference provides:

- `grok sessions list|search|delete` for discovery/housekeeping;
- `grok export <session-id> [output]` for Markdown transcript export; and
- `grok import [targets...]` to import sessions from Claude Code.

That means the feasibility matrix should classify **Claude Code -> Grok Build**
as a documented native import candidate (validate import behavior/version in
an integration test), while **other-agent -> Grok Build** should initially use
the portal's deterministic handoff brief plus a new Grok session. **Grok Build
-> any target** can supply a canonical transcript extracted from its local
files or its documented Markdown export, but xAI does not document a general
machine-readable export contract.

Sources: [Headless and Scripting](https://docs.x.ai/build/cli/headless-scripting),
[CLI reference](https://docs.x.ai/build/cli/reference),
[Sessions](https://docs.x.ai/build/features/sessions).

## Recommended adapter shape

1. `detect`: resolve `grok`, run `grok version`, test the known user data root.
2. `list_projects`: decode workspace-directory names under `.grok/sessions`;
   retain the raw encoded directory key for reliable lookup.
3. `list_sessions`: use `grok sessions list` where practical; fall back to the
   workspace/session directory layout only as a version-gated implementation
   detail.
4. `read_session`: parse a bounded, schema-checked subset of `summary.json`,
   `chat_history.jsonl`, and `events.jsonl`; preserve tool and file snapshot
   metadata only when it is safe and useful to the canonical IR.
5. `resume_command`: `grok --cwd <workspace> --resume <grok-session-uuid>`.
6. `write_session`: do **not** write raw files initially. Prefer invoking
   `grok import` only for validated Claude Code imports; otherwise create a
   new Grok session through ACP/headless mode and send a deterministic handoff
   brief.

## Validation before implementation

- Capture sanitized fixtures for at least two Grok versions and verify that
  discovery survives optional/missing files.
- Create, resume, fork, export, and delete a disposable session through the
  CLI; verify the portal's stored ID maps back to `grok --resume`.
- Exercise `grok import --list --json` and a disposable Claude Code fixture to
  determine its accepted source formats and whether it preserves session IDs.
- Exercise ACP authentication and `session/new`/`session/prompt` with an API
  key in a test environment; ensure logs redact it.
- Treat remote sync (where enabled) as out of scope until its privacy and
  conflict behavior is separately verified.

## Implemented first slice

The initial adapter intentionally exposes partial read plus native resume. It
enumerates `chat_format_version = 1` stores, parses summaries and canonical
chat records, and preserves unsupported records as metadata. It does not yet
advertise incoming brief/native migration: ACP target creation must capture
and persist the returned Grok session UUID, which the current launch-only
Portal command seam cannot do. Event/file-snapshot enrichment, ACP creation,
Claude import validation, and additional Grok-version fixtures remain follow-up
work.
