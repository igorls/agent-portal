# Security Policy

## Reporting a vulnerability

Please report security issues **privately** — do not open a public issue or PR.

Use GitHub's private vulnerability reporting: go to the repository's
[**Security** tab → **Report a vulnerability**](https://github.com/igorls/agent-portal/security/advisories/new).
This opens a private advisory visible only to the maintainers.

Please include:

- A description of the issue and its impact.
- Steps to reproduce, or a proof of concept.
- The affected version (or commit) and your platform.

You can expect an initial acknowledgement within a few days. Once a fix is ready
we'll coordinate a release and, with your consent, credit you in the advisory.

## Scope and threat model

Agent Portal is a local desktop application. It:

- Reads the on-disk session stores of coding agents installed on your machine.
- Writes migrated sessions and handoff briefs into a target agent's store, and
  records every migration in an undoable local ledger.
- Makes model calls **only** to a local, optional Ollama server (for brief
  polishing, session naming, and — when configured — local embeddings for
  semantic search). It never sends your sessions to any remote service.
- Will keep local search indexes (FTS and a WormDB sidecar data dir) under the
  app data directory when content/semantic search is enabled. Those corpora
  may contain transcript text. They stay on-device; the WormDB sidecar must
  bind loopback only. See `docs/global-session-search.md`.

Issues of particular interest include: writing outside an agent's intended store
location, mishandling untrusted content from a session store (path traversal,
injection into launched commands), any path that would exfiltrate session
data off the machine, or a search sidecar listening beyond localhost.

## Supported versions

This project is pre-1.0. Security fixes are made against the latest release and
`main`.
