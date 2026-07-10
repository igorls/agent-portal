//! Open the user's terminal in a workspace running a resume command.
//! Fire-and-forget: the spawned terminal owns the process; the activity log
//! keeps the exact command line for copy-paste recovery.

use crate::error::Result;
use crate::migration::types::CommandSpec;

pub fn launch_in_terminal(spec: &CommandSpec) -> Result<()> {
    imp::launch(spec)
}

#[cfg(windows)]
mod imp {
    use std::process::Command;

    use crate::error::{PortalError, Result};
    use crate::migration::types::CommandSpec;

    /// Windows Terminal first (new tab in the current window), classic
    /// console as fallback. The resume command always goes through pwsh:
    /// `codex` is an npm .cmd shim and `claude` lives in ~/.local/bin —
    /// CreateProcess can't exec those directly, a shell can.
    pub fn launch(spec: &CommandSpec) -> Result<()> {
        let command_line = spec.pwsh_line();

        let wt = Command::new("wt")
            .args([
                "-w",
                "0",
                "nt",
                "-d",
                &spec.cwd,
                "pwsh",
                "-NoExit",
                "-Command",
                &command_line,
            ])
            .spawn();
        if wt.is_ok() {
            return Ok(());
        }

        Command::new("cmd")
            .args([
                "/c",
                "start",
                "Agent Portal",
                "/d",
                &spec.cwd,
                "pwsh",
                "-NoExit",
                "-Command",
                &command_line,
            ])
            .spawn()
            .map_err(|e| PortalError::Other(format!("failed to open a terminal: {e}")))?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::process::Command;

    use crate::error::{PortalError, Result};
    use crate::migration::types::CommandSpec;

    pub fn launch(spec: &CommandSpec) -> Result<()> {
        let script = format!(
            "tell application \"Terminal\"\nactivate\ndo script \"cd {} && {}\"\nend tell",
            spec.cwd.replace('"', "\\\""),
            spec.display().replace('"', "\\\"")
        );
        Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn()
            .map_err(|e| PortalError::Other(format!("failed to open Terminal.app: {e}")))?;
        Ok(())
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::process::Command;

    use crate::error::{PortalError, Result};
    use crate::migration::types::CommandSpec;

    pub fn launch(spec: &CommandSpec) -> Result<()> {
        let shell_cmd = format!("cd '{}' && {}; exec $SHELL", spec.cwd, spec.display());
        for terminal in [
            std::env::var("TERMINAL").unwrap_or_default(),
            "x-terminal-emulator".to_string(),
            "gnome-terminal".to_string(),
            "konsole".to_string(),
            "xterm".to_string(),
        ] {
            if terminal.is_empty() {
                continue;
            }
            if Command::new(&terminal)
                .args(["-e", "sh", "-c", &shell_cmd])
                .spawn()
                .is_ok()
            {
                return Ok(());
            }
        }
        Err(PortalError::Other("no terminal emulator found".to_string()))
    }
}
