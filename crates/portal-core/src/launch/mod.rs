//! Open the user's terminal in a workspace running a resume/launch command.
//! Fire-and-forget: the spawned terminal owns the process; the activity log
//! keeps the exact command line for copy-paste recovery.
//!
//! Which interactive shell runs the command is user-configurable
//! ([`crate::settings::LaunchShell`]).

use crate::error::Result;
use crate::migration::types::CommandSpec;
use crate::settings::LaunchShell;

pub fn launch_in_terminal(spec: &CommandSpec, shell: LaunchShell) -> Result<()> {
    imp::launch(spec, shell)
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::process::Command;

    use crate::error::{PortalError, Result};
    use crate::migration::types::CommandSpec;
    use crate::settings::LaunchShell;

    #[derive(Clone, Copy)]
    enum WinShell {
        Pwsh,
        PowerShell,
        Cmd,
        Bash,
    }

    /// Windows Terminal first (new tab in the current window), classic
    /// console as fallback. The agent command always goes through a shell:
    /// `codex` is an npm .cmd shim and `claude` lives in ~/.local/bin —
    /// CreateProcess can't exec those directly, a shell can.
    pub fn launch(spec: &CommandSpec, shell: LaunchShell) -> Result<()> {
        let shell = resolve(shell)?;
        if try_windows_terminal(spec, shell).is_ok() {
            return Ok(());
        }
        try_cmd_start(spec, shell)
    }

    fn resolve(shell: LaunchShell) -> Result<WinShell> {
        match shell {
            LaunchShell::Auto => {
                if which("pwsh").is_some() {
                    Ok(WinShell::Pwsh)
                } else if which("powershell").is_some() {
                    Ok(WinShell::PowerShell)
                } else if which("bash").is_some() {
                    Ok(WinShell::Bash)
                } else {
                    Ok(WinShell::Cmd)
                }
            }
            LaunchShell::Pwsh => require("pwsh", WinShell::Pwsh),
            LaunchShell::PowerShell => require("powershell", WinShell::PowerShell),
            LaunchShell::Cmd => Ok(WinShell::Cmd),
            LaunchShell::Bash => require("bash", WinShell::Bash),
            LaunchShell::Zsh | LaunchShell::Fish => Err(PortalError::Other(
                "zsh/fish are not available as Windows launch shells — pick Auto, PowerShell, cmd, or Bash"
                    .into(),
            )),
        }
    }

    fn require(bin: &str, shell: WinShell) -> Result<WinShell> {
        if which(bin).is_some() {
            Ok(shell)
        } else {
            Err(PortalError::Other(format!(
                "shell '{bin}' was not found on PATH"
            )))
        }
    }

    fn which(bin: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            for candidate in [
                dir.join(bin),
                dir.join(format!("{bin}.exe")),
                dir.join(format!("{bin}.cmd")),
            ] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    fn try_windows_terminal(spec: &CommandSpec, shell: WinShell) -> Result<()> {
        let mut cmd = Command::new("wt");
        cmd.args(["-w", "0", "nt", "-d", &spec.cwd]);
        append_shell_invocation(&mut cmd, shell, spec);
        cmd.spawn()
            .map(|_| ())
            .map_err(|e| PortalError::Other(format!("Windows Terminal: {e}")))
    }

    fn try_cmd_start(spec: &CommandSpec, shell: WinShell) -> Result<()> {
        // `start "title" /d cwd prog args…` — title is required so paths with
        // spaces aren't eaten as the window title.
        let mut args = vec![
            "/c".into(),
            "start".into(),
            "Agent Portal".into(),
            "/d".into(),
            spec.cwd.clone(),
        ];
        match shell {
            WinShell::Pwsh => {
                args.extend([
                    "pwsh".into(),
                    "-NoExit".into(),
                    "-Command".into(),
                    spec.pwsh_line(),
                ]);
            }
            WinShell::PowerShell => {
                args.extend([
                    "powershell".into(),
                    "-NoExit".into(),
                    "-Command".into(),
                    spec.pwsh_line(),
                ]);
            }
            WinShell::Cmd => {
                args.extend(["cmd".into(), "/k".into(), spec.cmd_line()]);
            }
            WinShell::Bash => {
                args.extend([
                    "bash".into(),
                    "--login".into(),
                    "-i".into(),
                    "-c".into(),
                    format!("{}; exec bash", spec.posix_line()),
                ]);
            }
        }
        Command::new("cmd")
            .args(&args)
            .spawn()
            .map_err(|e| PortalError::Other(format!("failed to open a terminal: {e}")))?;
        Ok(())
    }

    fn append_shell_invocation(cmd: &mut Command, shell: WinShell, spec: &CommandSpec) {
        match shell {
            WinShell::Pwsh => {
                cmd.args(["pwsh", "-NoExit", "-Command", &spec.pwsh_line()]);
            }
            WinShell::PowerShell => {
                cmd.args(["powershell", "-NoExit", "-Command", &spec.pwsh_line()]);
            }
            WinShell::Cmd => {
                cmd.args(["cmd", "/k", &spec.cmd_line()]);
            }
            WinShell::Bash => {
                let line = format!("{}; exec bash", spec.posix_line());
                cmd.args(["bash", "--login", "-i", "-c", &line]);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::process::Command;

    use crate::error::{PortalError, Result};
    use crate::migration::types::CommandSpec;
    use crate::settings::LaunchShell;

    pub fn launch(spec: &CommandSpec, shell: LaunchShell) -> Result<()> {
        let shell_bin = resolve_unix_shell(shell)?;
        // Login interactive shell so agent CLIs on PATH via profile still resolve.
        let inner = format!(
            "cd {} && {}; exec {}",
            shell_single_quote(&spec.cwd),
            spec.posix_line(),
            shell_single_quote(&shell_bin)
        );
        let script = format!(
            "tell application \"Terminal\"\nactivate\ndo script {}\nend tell",
            applescript_string(&inner)
        );
        Command::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn()
            .map_err(|e| PortalError::Other(format!("failed to open Terminal.app: {e}")))?;
        Ok(())
    }

    fn resolve_unix_shell(shell: LaunchShell) -> Result<String> {
        match shell {
            LaunchShell::Auto => Ok(std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())),
            LaunchShell::Zsh => Ok("/bin/zsh".into()),
            LaunchShell::Bash => Ok("/bin/bash".into()),
            LaunchShell::Fish => Ok("fish".into()),
            LaunchShell::Pwsh => Ok("pwsh".into()),
            LaunchShell::PowerShell | LaunchShell::Cmd => Err(PortalError::Other(
                "Windows shells are not available on macOS — pick Auto, zsh, bash, or fish".into(),
            )),
        }
    }

    fn shell_single_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }

    fn applescript_string(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::process::Command;

    use crate::error::{PortalError, Result};
    use crate::migration::types::CommandSpec;
    use crate::settings::LaunchShell;

    pub fn launch(spec: &CommandSpec, shell: LaunchShell) -> Result<()> {
        let shell_bin = resolve_unix_shell(shell)?;
        let shell_cmd = format!(
            "cd {} && {}; exec {}",
            shell_single_quote(&spec.cwd),
            spec.posix_line(),
            shell_single_quote(&shell_bin)
        );
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
                .args(["-e", &shell_bin, "-lc", &shell_cmd])
                .spawn()
                .is_ok()
            {
                return Ok(());
            }
            // Fallback: many terminals expect `sh -c`
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

    fn resolve_unix_shell(shell: LaunchShell) -> Result<String> {
        match shell {
            LaunchShell::Auto => Ok(std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())),
            LaunchShell::Bash => Ok("/bin/bash".into()),
            LaunchShell::Zsh => Ok("/bin/zsh".into()),
            LaunchShell::Fish => Ok("fish".into()),
            LaunchShell::Pwsh => Ok("pwsh".into()),
            LaunchShell::PowerShell | LaunchShell::Cmd => Err(PortalError::Other(
                "Windows shells are not available on Linux — pick Auto, bash, zsh, or fish".into(),
            )),
        }
    }

    fn shell_single_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
