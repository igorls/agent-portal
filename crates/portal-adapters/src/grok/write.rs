//! Native write into Grok Build via the official `grok import` CLI.
//!
//! Compatible origins today: Claude Code only (`claude-code`). The CLI reads a
//! Claude project JSONL (or session id) and materializes a Grok session under
//! `~/.grok/sessions`. We never synthesize `chat_history.jsonl` ourselves.
//!
//! Windows note: current Grok builds treat only paths starting with `/` as
//! absolute CWDs. Real Claude transcripts on Windows use `C:\…`, so we rewrite
//! those fields to `/C:/…` for import, then re-home the session directory and
//! patch `summary.json` back to the real workspace path.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use portal_core::dto::Installation;
use portal_core::error::{PortalError, Result};
use portal_core::ir::{CanonicalSession, LossCode, LossNote};
use portal_core::migration::types::{
    ArtifactKind, WriteOptions, WritePlan, WrittenArtifact, WrittenSession,
};
use portal_core::util::paths::quick_hash;

/// Agent ids `grok import` is documented / validated to accept.
pub const NATIVE_IMPORT_SOURCES: &[&str] = &["claude-code"];

pub fn accepts_source(source_agent_id: &str) -> bool {
    NATIVE_IMPORT_SOURCES.contains(&source_agent_id)
}

pub fn plan_write(inst: &Installation, session: &CanonicalSession) -> Result<WritePlan> {
    ensure_compatible(session)?;

    let mut losses = session.losses.clone();
    losses.push(LossNote {
        code: LossCode::UnknownRecord,
        detail: "native path uses `grok import` (Claude Code → Grok); tool names may be remapped and thinking signatures are not preserved"
            .into(),
        turn_id: None,
    });

    let real_cwd = session.workspace.cwd.clone();
    let hint = PathBuf::from(&inst.store_root)
        .join(percent_encode(&real_cwd))
        .join(&session.identity.native_id);

    Ok(WritePlan {
        predicted_losses: losses,
        target_path_hint: hint.display().to_string(),
    })
}

pub fn write_session(
    inst: &Installation,
    session: &CanonicalSession,
    _opts: &WriteOptions,
) -> Result<WrittenSession> {
    ensure_compatible(session)?;

    let source_path = PathBuf::from(&session.identity.store_path);
    if !source_path.is_file() {
        return Err(PortalError::Other(format!(
            "Claude session file not found for import: {}",
            source_path.display()
        )));
    }

    let real_cwd = session.workspace.cwd.as_str();
    if real_cwd.is_empty() {
        return Err(PortalError::Other(
            "cannot import into Grok without a working directory".into(),
        ));
    }

    let rewrite = cwd_needs_import_rewrite(real_cwd);
    let (import_path, temp_path) = if rewrite {
        let temp = write_rewritten_import_copy(&source_path, real_cwd)?;
        (temp.clone(), Some(temp))
    } else {
        (source_path.clone(), None)
    };

    let import = run_grok_import(inst, &import_path);
    if let Some(temp) = &temp_path {
        let _ = std::fs::remove_file(temp);
    }
    let import = import?;

    let native_id = import.session_id;
    let import_cwd = import.cwd.unwrap_or_else(|| {
        if rewrite {
            to_importable_cwd(real_cwd)
        } else {
            real_cwd.to_string()
        }
    });

    let store_root = PathBuf::from(&inst.store_root);
    let imported_dir = store_root
        .join(percent_encode(&import_cwd))
        .join(&native_id);

    if !imported_dir.is_dir() {
        return Err(PortalError::Other(format!(
            "grok import reported success but session dir missing: {}",
            imported_dir.display()
        )));
    }

    let final_dir = if rewrite && import_cwd != real_cwd {
        rehome_session(&store_root, &imported_dir, real_cwd, &native_id)?
    } else {
        imported_dir
    };

    // Ensure summary cwd matches the real workspace (rehome patches; no-op otherwise).
    patch_summary_cwd(&final_dir, real_cwd)?;

    let artifacts = collect_dir_artifacts(&final_dir)?;
    Ok(WrittenSession {
        native_id,
        primary_path: final_dir.display().to_string(),
        artifacts,
    })
}

fn ensure_compatible(session: &CanonicalSession) -> Result<()> {
    if !accepts_source(&session.identity.agent_id) {
        return Err(PortalError::Other(format!(
            "Grok native import only supports Claude Code sessions (got agent '{}'); use a handoff brief for other sources",
            session.identity.agent_id
        )));
    }
    Ok(())
}

#[derive(Debug)]
struct ImportResult {
    session_id: String,
    cwd: Option<String>,
}

fn grok_import_subcommand_available(program: &str) -> bool {
    let Ok(output) = Command::new(program).arg("help").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("import ") || trimmed == "import"
    })
}

fn run_grok_import(inst: &Installation, import_path: &Path) -> Result<ImportResult> {
    let program = inst
        .cli_path
        .as_deref()
        .filter(|p| !p.is_empty())
        .unwrap_or("grok");

    let grok_home = Path::new(&inst.store_root)
        .parent()
        .ok_or_else(|| {
            PortalError::Other(format!(
                "Grok store root has no parent (expected …/.grok/sessions): {}",
                inst.store_root
            ))
        })?
        .to_path_buf();

    // Guard: recent Grok CLIs removed `import` (it is no longer a subcommand and
    // would be treated as a free-form TUI prompt). Fail closed with a clear note.
    if !grok_import_subcommand_available(program) {
        return Err(PortalError::Other(
            "this Grok Build CLI no longer provides `grok import`; native Claude→Grok migration is unavailable until a replacement lands"
                .into(),
        ));
    }

    let output = Command::new(program)
        .args(["import", "--json"])
        .arg(import_path)
        .env("GROK_HOME", &grok_home)
        .output()
        .map_err(|e| {
            PortalError::Other(format!(
                "failed to run `{program} import`: {e}. Is the Grok Build CLI installed?"
            ))
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut last_error = None;
    let mut success: Option<ImportResult> = None;

    for line in stdout.lines().chain(stderr.lines()) {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let outcome = value["outcome"].as_str().unwrap_or("");
        match outcome {
            "imported" | "already_imported" => {
                if let Some(session_id) = value["sessionId"].as_str() {
                    // Prefer real session UUIDs over path strings used on failure lines.
                    if uuid::Uuid::parse_str(session_id).is_ok() {
                        success = Some(ImportResult {
                            session_id: session_id.to_string(),
                            cwd: value["cwd"].as_str().map(str::to_string),
                        });
                    }
                }
            }
            "failed" => {
                last_error = Some(
                    value["error"]
                        .as_str()
                        .unwrap_or("import failed")
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    if let Some(ok) = success {
        return Ok(ok);
    }

    let detail = last_error
        .or_else(|| {
            let combined = format!("{stdout}{stderr}");
            let trimmed = combined.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .unwrap_or_else(|| format!("exit status {:?}", output.status.code()));

    Err(PortalError::Other(format!(
        "grok import failed for {}: {detail}",
        import_path.display()
    )))
}

/// True when Grok's importer will reject this cwd (Windows drive paths).
pub(crate) fn cwd_needs_import_rewrite(cwd: &str) -> bool {
    !cwd.starts_with('/')
}

/// `C:\Users\me\proj` → `/C:/Users/me/proj` so Grok's absolute-path check passes.
pub(crate) fn to_importable_cwd(cwd: &str) -> String {
    if cwd.starts_with('/') {
        return cwd.to_string();
    }
    // Prefix `/` so Grok treats the path as absolute; keep drive letter form.
    format!("/{}", cwd.replace('\\', "/"))
}

fn write_rewritten_import_copy(source: &Path, real_cwd: &str) -> Result<PathBuf> {
    let importable = to_importable_cwd(real_cwd);
    let temp = std::env::temp_dir().join(format!(
        "portal-grok-import-{}-{}.jsonl",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));

    let file = std::fs::File::open(source)?;
    let reader = std::io::BufReader::new(file);
    let mut out = std::fs::File::create(&temp)?;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            writeln!(out)?;
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(mut value) => {
                if value.get("cwd").and_then(|v| v.as_str()) == Some(real_cwd) {
                    value["cwd"] = Value::String(importable.clone());
                } else if let Some(cwd) = value.get("cwd").and_then(|v| v.as_str()) {
                    // Defensive: rewrite any Windows-looking cwd on the event.
                    if cwd_needs_import_rewrite(cwd) {
                        value["cwd"] = Value::String(to_importable_cwd(cwd));
                    }
                }
                let serialized = serde_json::to_string(&value).map_err(|e| {
                    PortalError::Other(format!("serializing rewritten Claude event: {e}"))
                })?;
                writeln!(out, "{serialized}")?;
            }
            Err(_) => {
                // Preserve unparseable lines as-is.
                writeln!(out, "{line}")?;
            }
        }
    }

    Ok(temp)
}

fn rehome_session(
    store_root: &Path,
    imported_dir: &Path,
    real_cwd: &str,
    native_id: &str,
) -> Result<PathBuf> {
    let dest_parent = store_root.join(percent_encode(real_cwd));
    std::fs::create_dir_all(&dest_parent)?;
    let dest = dest_parent.join(native_id);
    if dest.exists() {
        return Err(PortalError::Other(format!(
            "Grok session already exists at {}",
            dest.display()
        )));
    }
    std::fs::rename(imported_dir, &dest).map_err(|e| {
        PortalError::Other(format!(
            "failed to re-home imported Grok session to real workspace path: {e}"
        ))
    })?;

    // Best-effort: drop empty import workspace dir.
    if let Some(parent) = imported_dir.parent() {
        let _ = std::fs::remove_dir(parent);
    }

    Ok(dest)
}

fn patch_summary_cwd(session_dir: &Path, real_cwd: &str) -> Result<()> {
    let summary_path = session_dir.join("summary.json");
    if !summary_path.is_file() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&summary_path)?;
    let mut value: Value = serde_json::from_str(&raw).map_err(|e| PortalError::Parse {
        path: summary_path.display().to_string(),
        detail: e.to_string(),
    })?;
    if value["info"]["cwd"].as_str() != Some(real_cwd) {
        value["info"]["cwd"] = Value::String(real_cwd.into());
        let pretty = serde_json::to_vec_pretty(&value)
            .map_err(|e| PortalError::Other(format!("serializing patched Grok summary: {e}")))?;
        std::fs::write(&summary_path, pretty)?;
    }
    Ok(())
}

fn collect_dir_artifacts(session_dir: &Path) -> Result<Vec<WrittenArtifact>> {
    let mut artifacts = Vec::new();
    collect_dir_artifacts_rec(session_dir, &mut artifacts)?;
    if artifacts.is_empty() {
        return Err(PortalError::Other(format!(
            "imported Grok session has no files: {}",
            session_dir.display()
        )));
    }
    Ok(artifacts)
}

fn collect_dir_artifacts_rec(dir: &Path, out: &mut Vec<WrittenArtifact>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_dir_artifacts_rec(&path, out)?;
        } else if path.is_file() {
            let bytes = std::fs::read(&path)?;
            out.push(WrittenArtifact {
                kind: ArtifactKind::File,
                path: path.display().to_string(),
                backup: None,
                content_hash: Some(quick_hash(&bytes)),
            });
        }
    }
    Ok(())
}

/// Percent-encode a workspace path the way Grok names session parent dirs.
pub(crate) fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_windows_workspace_key_like_grok() {
        assert_eq!(percent_encode(r"P:\demo\app"), "P%3A%5Cdemo%5Capp");
        assert_eq!(percent_encode(r"C:\Users\igorl"), "C%3A%5CUsers%5Cigorl");
    }

    #[test]
    fn importable_cwd_rewrites_windows_drive_paths() {
        assert!(cwd_needs_import_rewrite(r"C:\Users\igorl\proj"));
        assert!(!cwd_needs_import_rewrite("/tmp/demo"));
        assert_eq!(
            to_importable_cwd(r"C:\Users\igorl\proj"),
            "/C:/Users/igorl/proj"
        );
        assert_eq!(to_importable_cwd("/tmp/demo"), "/tmp/demo");
    }

    #[test]
    fn only_claude_code_is_native_source() {
        assert!(accepts_source("claude-code"));
        assert!(!accepts_source("codex"));
        assert!(!accepts_source("grok-build"));
        assert!(!accepts_source("opencode"));
    }
}
