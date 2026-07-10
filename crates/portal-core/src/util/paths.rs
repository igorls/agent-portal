use std::path::{Path, PathBuf};
use std::process::Command;

/// Find a CLI on PATH. On Windows tries .exe, .cmd, .bat (npm shims are
/// .cmd; Claude ships an .exe in ~/.local/bin).
pub fn find_cli(path_dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    let candidates: &[String] = &if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
        ]
    } else {
        vec![name.to_string()]
    };
    for dir in path_dirs {
        for candidate in candidates {
            let p = dir.join(candidate);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Run `<cli> --version`-style probes. .cmd/.bat shims can't be spawned by
/// CreateProcess directly, so they go through `cmd /c`.
pub fn cli_version(cli: &Path, arg: &str) -> Option<String> {
    let ext = cli
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mut command = if matches!(ext.as_str(), "cmd" | "bat") {
        let mut c = Command::new("cmd");
        c.arg("/c").arg(cli).arg(arg);
        c
    } else {
        let mut c = Command::new(cli);
        c.arg(arg);
        c
    };

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Fast non-cryptographic content fingerprint (hex). Used only to detect
/// whether a migrated artifact changed since we wrote it (an agent continuing
/// the session), which gates undo — not for any security purpose.
pub fn quick_hash(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Write-then-rename in the destination directory: readers never observe a
/// partial file, and Windows' rename-refuses-to-overwrite doubles as the id
/// collision guard (target names are always freshly generated).
pub fn atomic_write(final_path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let dir = final_path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let file_name = final_path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no name"))?
        .to_string_lossy()
        .to_string();
    let tmp = dir.join(format!(".{file_name}.portal-tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content)?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, final_path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Canonical form used to align the same project across agents that record
/// cwd differently (`P:\x`, `P:/x`, `p:\x` are all the same workspace).
pub fn normalize_cwd(cwd: &str) -> String {
    let mut s = cwd.trim().replace('\\', "/");
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let lower = bytes[0].to_ascii_lowercase() as char;
        s.replace_range(0..1, &lower.to_string());
    }
    while s.ends_with('/') && s.len() > 1 {
        s.pop();
    }
    s
}

/// Human label for a workspace: its final path segment.
pub fn label_from_cwd(cwd: &str) -> String {
    let normalized = normalize_cwd(cwd);
    normalized
        .rsplit('/')
        .find(|seg| !seg.is_empty())
        .unwrap_or(&normalized)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_separators_and_drive_case() {
        assert_eq!(
            normalize_cwd(r"P:\rioblocks\bentokit"),
            "p:/rioblocks/bentokit"
        );
        assert_eq!(
            normalize_cwd("P:/rioblocks/bentokit"),
            "p:/rioblocks/bentokit"
        );
        assert_eq!(
            normalize_cwd(r"p:\rioblocks\bentokit\"),
            "p:/rioblocks/bentokit"
        );
        assert_eq!(normalize_cwd("/home/igorls/dev/x"), "/home/igorls/dev/x");
    }

    #[test]
    fn labels_are_final_segment() {
        assert_eq!(label_from_cwd(r"P:\agent-portal"), "agent-portal");
        assert_eq!(label_from_cwd("/home/igorls/dev/meshguard"), "meshguard");
    }
}
