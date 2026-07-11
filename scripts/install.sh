#!/usr/bin/env bash
# Agent Portal installer — downloads the latest release build for your platform
# and installs it. macOS and Linux only (on Windows, use the .msi/.exe from the
# Releases page).
#
#   curl -fsSL https://raw.githubusercontent.com/igorls/agent-portal/main/scripts/install.sh | bash
#
# Environment overrides:
#   AGENT_PORTAL_VERSION   release tag to install (default: latest)
#   AGENT_PORTAL_REPO      owner/repo (default: igorls/agent-portal)
#   GITHUB_TOKEN           token for private-repo / rate-limited access
set -euo pipefail

REPO="${AGENT_PORTAL_REPO:-igorls/agent-portal}"
VERSION="${AGENT_PORTAL_VERSION:-latest}"

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required"

# curl wrapper that adds auth when a token is present (needed for private repos
# and to dodge anonymous API rate limits).
fetch() {
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" "$@"
  else
    curl -fsSL "$@"
  fi
}

# Resolve the release metadata.
api="https://api.github.com/repos/$REPO/releases"
if [ "$VERSION" = "latest" ]; then
  release_json="$(fetch "$api/latest")" \
    || die "could not fetch the latest release — is the repo public, or is GITHUB_TOKEN set?"
else
  release_json="$(fetch "$api/tags/$VERSION")" || die "release '$VERSION' not found"
fi

tag="$(printf '%s' "$release_json" | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
[ -n "$tag" ] || die "could not read the release tag (a draft release is not downloadable — publish it first)"

# Pick the asset for this platform.
case "$(uname -s)" in
  Darwin) suffix='\.dmg$' ; kind=dmg ;;
  Linux)  suffix='\.AppImage$' ; kind=appimage ;;
  *) die "unsupported OS '$(uname -s)' — use the Windows installer from the Releases page" ;;
esac

url="$(printf '%s' "$release_json" \
  | grep -oE '"browser_download_url": *"[^"]+"' \
  | sed -E 's/.*"(https[^"]+)".*/\1/' \
  | grep -E "$suffix" | head -n1 || true)"
[ -n "$url" ] || die "no matching asset for this platform in release $tag"

say "Installing Agent Portal $tag"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
file="$tmp/$(basename "$url")"
say "Downloading $(basename "$url")"
fetch "$url" -o "$file"

if [ "$kind" = dmg ]; then
  mount="$(hdiutil attach -nobrowse -readonly "$file" | grep -oE '/Volumes/[^	]+' | tail -1)"
  [ -n "$mount" ] || die "failed to mount the disk image"
  app="$(find "$mount" -maxdepth 1 -name '*.app' | head -n1 || true)"
  if [ -z "$app" ]; then hdiutil detach "$mount" >/dev/null 2>&1 || true; die "no .app inside the disk image"; fi
  dest="/Applications"; [ -w "$dest" ] || dest="$HOME/Applications"
  mkdir -p "$dest"
  say "Installing to $dest"
  rm -rf "${dest:?}/$(basename "$app")"
  cp -R "$app" "$dest/"
  hdiutil detach "$mount" >/dev/null 2>&1 || true
  # Clear the quarantine flag so the unsigned app opens without a Gatekeeper block.
  xattr -dr com.apple.quarantine "$dest/$(basename "$app")" 2>/dev/null || true
  say "Done. Open 'Agent Portal' from $dest."
else
  dest="$HOME/.local/bin"; mkdir -p "$dest"
  bin="$dest/agent-portal"
  install -m 0755 "$file" "$bin"
  say "Done. Installed to $bin"
  case ":$PATH:" in
    *":$dest:"*) : ;;
    *) say "Note: add $dest to your PATH to run 'agent-portal' directly." ;;
  esac
fi
