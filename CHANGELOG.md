# Changelog

All notable changes to Agent Portal are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1] - 2026-07-18

### Fixed
- **Session naming** no longer re-names live sessions every pass as size/mtime
  churns — that previously looped the most recent session and pinned the fast
  cadence with a permanent “1 queued” feel.
- Empty or content-less sessions get a local fallback title instead of staying
  pending forever after a silent model failure.
- The board keeps showing the last generated title while a session is still
  being written; Activity treats live titles as current so coverage does not
  thrash.

### Installers
Download the installer for your platform from the GitHub Release assets
(macOS universal DMG, Windows MSI/NSIS, Linux AppImage/deb/rpm).

## [0.2.0] - 2026-07-17

Broader multi-agent coverage, faster ways to jump into a project, and a first
pass at usage insight across your local agent fleet.

### Added

#### Agents
- **Factory Droid** adapter (`droid` / `~/.factory/sessions`) — read sessions,
  resume, open project, and accept brief-mode migrations.
- **Pi** adapter (`pi` / `~/.pi/agent/sessions`) — read sessions, resume, open
  project, and accept brief-mode migrations.
- **Junie** (JetBrains) adapter (`junie` / `~/.junie/sessions`) — read from the
  event stream + index, resume, open project, and accept brief-mode migrations.

#### Board & launch
- **Open with agent** on a selected project folder — starts an interactive
  session in that workspace without a handoff prompt
  (`open_project_command` on adapters).
- **Terminal shell preference** in Settings (Auto, PowerShell 7, Windows
  PowerShell, cmd, bash, zsh, fish) for agent launches from the board and
  migration wizard.

#### Insights
- **Usage** page — local session/migration activity, rough token estimates, and
  agent breakdowns (no cloud upload).
- Richer **Activity** page layout for migrations and background session
  naming.

### Notes
- New agents are **read + resume + brief-target** in this release. Native
  write (full session conversion into the agent’s store) is intentionally
  deferred until each format has a stable import path.
- Junie transcripts are reconstructed from UI event blocks, so intermediate
  assistant prose may be incomplete compared with Claude/Codex JSONL.
- Recent **Grok Build** CLIs removed `grok import`. Native Claude→Grok
  migration fails closed with a clear error (brief mode still works);
  older CLIs that still ship `import` keep working.

### Installers
Download the installer for your platform from the GitHub Release assets
(macOS universal DMG, Windows MSI/NSIS, Linux AppImage/deb/rpm).

## [0.1.3] - 2026-07-17

### Added
- **Grok Build** adapter with native migration from Claude Code via
  `grok import`, plus brief-mode as a target for other sources.
- Compact idle-agent board rail so unused lanes take less space.

### Installers
Download the installer for your platform from the GitHub Release assets.

## [0.1.2] - 2026-07-15

### Added
- macOS **signing and notarization** for release disk images.
- Community health files (code of conduct, contributing, security).

### Fixed
- Release workflow only publishes on `v*` tag pushes (not manual
  `workflow_dispatch` dry runs).

### Installers
Download the installer for your platform from the GitHub Release assets.

## [0.1.1] - 2026-07-11

### Added
- Initial public installer set for macOS, Windows, and Linux.
- One-line install script for macOS/Linux.

### Installers
Download the installer for your platform from the GitHub Release assets.

[Unreleased]: https://github.com/igorls/agent-portal/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/igorls/agent-portal/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/igorls/agent-portal/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/igorls/agent-portal/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/igorls/agent-portal/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/igorls/agent-portal/releases/tag/v0.1.1
