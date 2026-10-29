# BentoKit Core migrations

Agent Portal can read BentoKit Core sessions and import selected conversation
history into Core profiles. Native destination support is conditional on a
versioned Core CLI protocol; installing this Portal change alone does not make
a released Core CLI a writable destination.

## Core dependency

As checked on 2026-10-29, the published `rioblocks/bentokit-core` default branch
at `3967bd6650b6fda43db5af7e13330e0da85b7f47` and its `desktop-v0.1.9` source
route `session` to list, prune and print only. The native importer used for local
integration tests is a separate, unpublished Core working-tree implementation,
developed from Core `28b25a0`. It is not included or published by this Portal
change. The published `rioblocks/bentokit` default branch was also checked and
had no import-protocol route.

To test Bento as a native destination, obtain a Core build implementing all of:

```text
bentokit session import-protocol
bentokit session import-targets --data <data-directory>
bentokit session import-plan --data <data-directory>
bentokit session import --data <data-directory>
bentokit session undo-import --data <data-directory>
```

The first command must return JSON `{ "version": 1 }`. The others use versioned
JSON on stdin/stdout; `import-targets` enumerates existing profiles and projects,
`import-plan` validates without writing, `import` returns a durable conversation
receipt, and `undo-import` conditionally tombstones that one unchanged session.
They run locally without starting a model or Core's runtime.

Portal probes `import-protocol` before enabling native imports. Missing,
unrecognized, or older CLI implementations remain safe **read-only sources**.
A version label alone does not enable writes. A tester needs a protocol-v1 Core
build separately; the stock CLI cannot exercise the native destination yet.

## Using it

1. Put the compatible Core CLI on PATH as `bentokit` and refresh Portal.
2. Add a selected ChatGPT conversation JSON, or choose another source session.
3. Drag the session onto BentoKit Core and choose an existing profile, a project
   or the profile's default workspace, and the runtime URL used for resume.
4. Review the dry run and fidelity notes, then import. The history becomes a
   persistent conversation accessible from Core's session list and TUI.

The default Core data directory is `~/.bentokit`; a store override identifies
that whole directory. Reopening uses an already running matching Core instance:

```sh
bentokit tui --agent <profile> --session <conversation-id> --url ws://localhost:10001/api/ws
```

The runtime must serve the selected data directory. Authentication follows the
TUI's supported `BENTOKIT_API_KEY` mechanism; Portal does not put credentials
into the ledger or command arguments. New turns use the selected profile's
own model, Soul and permissions.

## Selected ChatGPT conversation files

The Board's **Import ChatGPT export** control accepts a single conversation
object with `mapping` and `current_node`, a one-element array, or an explicit
`chatgpt-visible-dom-v1` capture of one selected chat. It does not initiate a
ChatGPT account export, browse the account, or generate a public share link.
No account-history export is needed when a selected-chat capture is available.

A visible-DOM capture contains the observed user/assistant text in order,
message ids and available HTML provenance. It must be explicitly labelled with
its capture kind. Native timestamps, exact source model ids, hidden reasoning,
unrendered branches and media bytes remain unknown rather than being invented.
The capture reader's parent links describe observed display order.

If a user already has a multi-conversation `conversations.json` or numbered
JSON file, Portal lists its conversations locally and stages only the user's
selection. It does not ingest the entire archive. The ZIP format is not an
input; use the selected JSON file. No manual JSON editing is required.

The reader walks the selected branch and archives the supplied graph,
selection, citations/events and attachment references. It rejects cycles,
missing nodes, oversized inputs and changed staged files. A summary sidecar
keeps board refreshes from reparsing every transcript. Test JSON fixtures are
explicitly synthetic and contain no real account conversations.

## Transcript fidelity

Visible source turns become native messages. Historical provenance retains
source ids, roles, parent ids, available timestamps, blocks, raw frames, model
labels and usage. Sequence preserves source order even when clocks disagree.
A missing timestamp stays missing in provenance.

System/harness and graph-only records are archived in conversation metadata,
outside the operational message list. They do not become phantom user turns
or target system authority. Tool records retain associations and source frames;
operational context identifies them as historical evidence and never reruns
historical calls. Alternate branches and attachment references remain data,
with source loss notes carried through subsequent exports. Media bytes and
sidechain files are not copied by this bounded route.

Read-back checks the actual stored role/content/timestamp fingerprint before
restoring original structured turns. A later edit uses current message content
and retains the older provenance separately. A corrupt operational row cannot
pass verification merely because its source copy remains in metadata.

A migrated transcript is not a provider checkpoint: it does not transfer
credentials, permissions, runtime tools, or provider reasoning state. Reasoning
or signatures supplied in a source frame remain historical data. Hidden OpenAI
reasoning is outside the web reader's scope. Storage fidelity does not guarantee
identical character behavior or future recall from a different model.

The earlier BentoKit preview importer that stored identity in Soul backstory
and transcripts in archive/memory is a different route; it does not create
these native conversation sessions.

## Durability, recovery and undo

Core owns its per-profile SQLite database and ordinary conversation JSONL
journal. Import publishes an immutable manifest atomically, appends standard
conversation/message journal records, then projects them under a SQLite writer
lock. Deterministic ids recover partial journal writes or a missing projection
without duplicating messages. Ordinary journal rebuild understands the records.

Retries retain a plan's idempotency key. A deliberate new migration gets a new
key, including after undo. At execution Portal saves the full plan in private
app-data `migration-plans`; Activity can retry interrupted imports after restart.
The ledger records success only after read-back verification.

Each receipt identifies one conversation and its revision. Undo requests a
conditional durable tombstone, never a restore or deletion of a shared database
or journal. Changed or continued conversations refuse undo, including forced
undo. Imported-session appends serialize with conditional undo under the same
SQLite writer lock. Other conversations remain intact.

## Validation

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
pnpm --dir ui build
node scripts/check-command-parity.mjs
```

A separate opt-in integration test requires the unpublished protocol-v1 Core
implementation, Bun and a Unix host. All data is isolated; no model is called:

```sh
BENTOKIT_CORE_TEST_ROOT=/path/to/compatible/bentokit-core \
  cargo test -p portal-adapters --test bentokit_destination -- --ignored
```

Set `BENTOKIT_CORE_TEST_CLI` to a compatible standalone Core binary to test the
packaged CLI protocol instead of the source shim. The test covers exact
canonical read-back, missing timestamps, edits, corruption detection, retry,
undo, a new migration after undo, persisted continuation and refusal of undo
for continued sessions. Synthetic ChatGPT input creates only its visible native
messages; the graph is archived metadata. Native Windows Core protocol
integration and packaged distribution of the new Core implementation remain
unvalidated.
