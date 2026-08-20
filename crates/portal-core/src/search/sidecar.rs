//! WormDB sidecar process + minimal WormWire client (loopback only).
//!
//! Semantic indexing (mem_*) will use this once embeddings are wired.
//! Shipping the process lifecycle now so HNSW-capable backend is available.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::{PortalError, Result};

const WW_MAGIC: [u8; 2] = [0x57, 0x57]; // "WW"
const CMD_DEL: u8 = 0x03;
const CMD_STATUS: u8 = 0x04;
const CMD_EXEC: u8 = 0x09;
const RESP_OK: u8 = 0x00;
const RESP_VALUE: u8 = 0x01;
const RESP_ERR: u8 = 0x03;

/// Managed local WormDB child (127.0.0.1 only).
pub struct WormdbSidecar {
    inner: Mutex<Option<SidecarInner>>,
    data_dir: PathBuf,
    binary: PathBuf,
    preferred_port: u16,
}

struct SidecarInner {
    /// None when we attached to another process's sidecar (client-only).
    child: Option<Child>,
    port: u16,
}

impl WormdbSidecar {
    pub fn new(app_data_dir: &Path, binary: PathBuf, preferred_port: u16) -> Self {
        Self {
            inner: Mutex::new(None),
            data_dir: app_data_dir.join("wormdb"),
            binary,
            preferred_port,
        }
    }

    pub fn binary_path(&self) -> &Path {
        &self.binary
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Ensure sidecar is reachable; returns bound port.
    ///
    /// If another Portal process already owns the data dir, **attach as a
    /// client** using the port recorded in the lock file (no second spawn).
    pub fn ensure_running(&self) -> Result<u16> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(inner) = guard.as_mut() {
            if let Some(child) = inner.child.as_mut() {
                match child.try_wait() {
                    Ok(None) => return Ok(inner.port),
                    Ok(Some(_)) | Err(_) => {
                        *guard = None;
                    }
                }
            } else {
                // Client-only attach: probe STATUS still works.
                if WormWireClient::connect(inner.port)
                    .and_then(|mut c| c.status())
                    .is_ok()
                {
                    return Ok(inner.port);
                }
                *guard = None;
            }
        }

        std::fs::create_dir_all(&self.data_dir)?;
        let lock_path = self.data_dir.join("portal-sidecar.lock");

        // Attach to existing owner if lock is live and port answers.
        if let Some((owner_pid, port)) = read_lock(&lock_path) {
            if port != 0 {
                // Prefer port reachability over PID — child wormdb can outlive owner.
                if WormWireClient::connect(port)
                    .and_then(|mut c| c.status())
                    .is_ok()
                {
                    *guard = Some(SidecarInner { child: None, port });
                    return Ok(port);
                }
            }
            if pid_alive(owner_pid) && port == 0 {
                return Err(PortalError::Other(format!(
                    "WormDB lock held by pid {owner_pid} (legacy lock without port); cannot attach"
                )));
            }
            // Stale / unreachable — clear lock before spawn.
            let _ = std::fs::remove_file(&lock_path);
        }

        if !self.binary.exists() {
            return Err(PortalError::Other(format!(
                "WormDB binary not found at {}",
                self.binary.display()
            )));
        }

        let port = if self.preferred_port == 0 {
            pick_free_port()?
        } else {
            self.preferred_port
        };

        // Writable cwd + loopback-only config (wormdb defaults to 0.0.0.0 and
        // ./wormdb.json relative to cwd — both are hostile under Program Files).
        let config_path = self.data_dir.join("portal-sidecar.json");
        let _ = std::fs::write(
            &config_path,
            r#"{
  "server": {
    "bind_address": "127.0.0.1"
  }
}
"#,
        );
        let log_path = self.data_dir.join("sidecar-stderr.log");
        let stderr_file = std::fs::File::create(&log_path).ok();

        let mut cmd = Command::new(&self.binary);
        cmd.arg("--port")
            .arg(port.to_string())
            .arg("--data")
            .arg(&self.data_dir)
            .arg("--config")
            .arg(&config_path)
            .current_dir(&self.data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        if let Some(f) = stderr_file {
            cmd.stderr(Stdio::from(f));
        } else {
            cmd.stderr(Stdio::null());
        }
        // Windows: don't open a console window for the sidecar.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| PortalError::Other(format!("spawn wormdb: {e}")))?;

        // Cold start can exceed 10s on large WAL / HNSW data dirs — budget generously.
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut last_err = String::new();
        while Instant::now() < deadline {
            match WormWireClient::connect_timeout(port, Duration::from_millis(750)) {
                Ok(mut c) => match c.status() {
                    Ok(_) => {
                        let _ = write_lock(&lock_path, std::process::id(), port);
                        *guard = Some(SidecarInner {
                            child: Some(child),
                            port,
                        });
                        return Ok(port);
                    }
                    Err(e) => last_err = e.to_string(),
                },
                Err(e) => last_err = e.to_string(),
            }
            if let Ok(Some(status)) = child.try_wait() {
                let tail = read_log_tail(&log_path, 800);
                return Err(PortalError::Other(format_sidecar_exit(
                    &status,
                    &last_err,
                    &self.binary,
                    &log_path,
                    &tail,
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        let tail = read_log_tail(&log_path, 800);
        Err(PortalError::Other(format!(
            "wormdb sidecar on :{port} not ready after 90s: {last_err}{}",
            if tail.is_empty() {
                format!(" (see {})", log_path.display())
            } else {
                format!("; log: {tail}")
            }
        )))
    }

    pub fn port(&self) -> Option<u16> {
        self.inner.lock().unwrap().as_ref().map(|i| i.port)
    }

    pub fn is_running(&self) -> bool {
        let mut guard = self.inner.lock().unwrap();
        if let Some(inner) = guard.as_mut() {
            if let Some(child) = inner.child.as_mut() {
                return matches!(child.try_wait(), Ok(None));
            }
            // Attached client: consider running if we still have a port.
            return true;
        }
        false
    }

    /// Best-effort stop (only kills child if we own it).
    pub fn stop(&self) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(mut inner) = guard.take() {
            if let Some(mut child) = inner.child.take() {
                let _ = child.kill();
                let _ = child.wait();
                let lock_path = self.data_dir.join("portal-sidecar.lock");
                if let Some((pid, _)) = read_lock(&lock_path) {
                    if pid == std::process::id() {
                        let _ = std::fs::remove_file(&lock_path);
                    }
                }
            }
        }
    }
}

/// Lock format: `pid port` (space-separated). Older locks were pid-only.
fn read_lock(path: &Path) -> Option<(u32, u16)> {
    let contents = std::fs::read_to_string(path).ok()?;
    let mut parts = contents.split_whitespace();
    let pid: u32 = parts.next()?.parse().ok()?;
    let port: u16 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    if port == 0 {
        // Legacy pid-only lock — cannot attach; treat as unusable if dead only.
        return Some((pid, 0));
    }
    Some((pid, port))
}

fn write_lock(path: &Path, pid: u32, port: u16) -> std::io::Result<()> {
    std::fs::write(path, format!("{pid} {port}"))
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        // OpenProcess with limited query; null handle => not alive.
        use std::ptr;
        extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() || h == ptr::null_mut() {
                return false;
            }
            CloseHandle(h);
            true
        }
    }
    #[cfg(not(windows))]
    {
        // kill(pid, 0) exists via libc; approximate with /proc.
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}

impl Drop for WormdbSidecar {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pick_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| PortalError::Other(format!("bind free port: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| PortalError::Other(e.to_string()))?
        .port();
    drop(listener);
    Ok(port)
}

fn read_log_tail(path: &Path, max_chars: usize) -> String {
    let Ok(s) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let t = s.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.chars().count() <= max_chars {
        return t.to_string();
    }
    t.chars()
        .rev()
        .take(max_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn format_sidecar_exit(
    status: &std::process::ExitStatus,
    last_err: &str,
    binary: &Path,
    log_path: &Path,
    log_tail: &str,
) -> String {
    let mut msg = format!("wormdb exited before ready: {status} ({last_err})");
    msg.push_str(&format!("; binary={}", binary.display()));
    if !log_tail.is_empty() {
        msg.push_str(&format!("; log: {log_tail}"));
    } else {
        msg.push_str(&format!("; log={}", log_path.display()));
    }
    // Windows: 0xC0000005 = STATUS_ACCESS_VIOLATION (native crash).
    #[cfg(windows)]
    {
        if let Some(code) = status.code() {
            let u = code as u32;
            if u == 0xC000_0005 {
                msg.push_str(
                    ". Access violation: often antivirus quarantine of the bundled wormdb.exe, \
                     a native-CPU build on a machine without those instructions, or a corrupt \
                     install. Reinstall with the NSIS setup.exe, allow the binary in Defender, \
                     and confirm wormdb.exe sits next to the app under Program Files.",
                );
            } else if u == 0xC000_0135 {
                msg.push_str(
                    ". Missing DLL (often Universal CRT / VC++ runtime). Install the \
                     latest Visual C++ Redistributable (x64) and retry.",
                );
            } else if u == 0xC000_001D {
                msg.push_str(
                    ". Illegal instruction: this wormdb.exe was built for a newer CPU. \
                     Rebuild/vendor with `zig build -Doptimize=ReleaseSmall -Dcpu=baseline`.",
                );
            }
        }
    }
    msg
}

/// Resolve a **bundled** wormdb binary. Never falls back to bare PATH names
/// (`wormdb` / `wormdb.exe` without a directory) or hard-coded repo paths.
///
/// Order:
/// 1. Explicit override path (if it exists)
/// 2. `PORTAL_WORMDB` absolute/relative path (debug override only)
/// 3. Each directory in `extra_dirs` (resource dir, `src-tauri/binaries`, …)
/// 4. Directory of the current executable (+ `resources/` sibling)
///
/// Returns the first existing candidate, or a diagnostic path that does not
/// exist (caller checks `.exists()`).
pub fn resolve_wormdb_binary(override_path: Option<&str>, extra_dirs: &[PathBuf]) -> PathBuf {
    if let Some(p) = override_path {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return pb;
        }
    }
    if let Ok(p) = std::env::var("PORTAL_WORMDB") {
        let pb = PathBuf::from(p.trim());
        if !pb.as_os_str().is_empty() && pb.is_file() {
            return pb;
        }
    }

    let mut dirs: Vec<PathBuf> = extra_dirs.to_vec();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
            dirs.push(dir.join("resources"));
            // macOS app bundle: Contents/MacOS → Contents/Resources
            if let Some(contents) = dir.parent() {
                dirs.push(contents.join("Resources"));
            }
        }
    }

    for dir in &dirs {
        if let Some(found) = find_wormdb_in_dir(dir) {
            return found;
        }
    }

    // Diagnostic only — must not be a bare PATH lookup name.
    if let Some(first) = extra_dirs.first() {
        return first.join(wormdb_file_name());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join(wormdb_file_name());
        }
    }
    PathBuf::from(format!("(bundled)/{}", wormdb_file_name()))
}

fn wormdb_file_name() -> &'static str {
    if cfg!(windows) {
        "wormdb.exe"
    } else {
        "wormdb"
    }
}

/// Look for a wormdb binary in `dir` (exact names + Tauri target-triple suffixes).
fn find_wormdb_in_dir(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let exact = [
        wormdb_file_name(),
        "wormdb",
        "wormdb.exe",
        // Vendored next to tauri.conf without rename (dev tree).
        "wormdb-x86_64-pc-windows-msvc.exe",
        "wormdb-aarch64-pc-windows-msvc.exe",
        "wormdb-x86_64-apple-darwin",
        "wormdb-aarch64-apple-darwin",
        "wormdb-x86_64-unknown-linux-gnu",
        "wormdb-aarch64-unknown-linux-gnu",
    ];
    for name in exact {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    // Any wormdb-* sibling (future triples).
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            let name = ent.file_name();
            let s = name.to_string_lossy();
            if !s.starts_with("wormdb") {
                continue;
            }
            let p = ent.path();
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

pub struct WormWireClient {
    stream: TcpStream,
}

impl WormWireClient {
    pub fn connect(port: u16) -> Result<Self> {
        Self::connect_timeout(port, Duration::from_millis(500))
    }

    /// Longer connect budget for startup wait loops.
    pub fn connect_timeout(port: u16, connect_budget: Duration) -> Result<Self> {
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}")
            .parse()
            .map_err(|e| PortalError::Other(format!("wormdb addr: {e}")))?;
        let mut stream = TcpStream::connect_timeout(&addr, connect_budget)
            .map_err(|e| PortalError::Other(format!("wormdb connect: {e}")))?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
        stream
            .write_all(&WW_MAGIC)
            .map_err(|e| PortalError::Other(format!("wormdb magic: {e}")))?;
        Ok(Self { stream })
    }

    pub fn status(&mut self) -> Result<Vec<u8>> {
        self.request(CMD_STATUS, &[])
    }

    /// EXEC mem_* or other procedures. Args are raw byte slices.
    pub fn exec(&mut self, name: &str, args: &[&[u8]]) -> Result<Vec<u8>> {
        let mut payload = Vec::new();
        write_len_bytes(&mut payload, name.as_bytes());
        payload.extend_from_slice(&(args.len() as u32).to_be_bytes());
        for a in args {
            write_len_bytes(&mut payload, a);
        }
        self.request(CMD_EXEC, &payload)
    }

    /// DEL key (0x03). Missing keys succeed.
    pub fn del(&mut self, key: &[u8]) -> Result<()> {
        let mut payload = Vec::new();
        write_len_bytes(&mut payload, key);
        let _ = self.request(CMD_DEL, &payload)?;
        Ok(())
    }

    fn request(&mut self, cmd: u8, payload: &[u8]) -> Result<Vec<u8>> {
        let mut frame = Vec::with_capacity(5 + payload.len());
        frame.push(cmd);
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        self.stream
            .write_all(&frame)
            .map_err(|e| PortalError::Other(format!("wormdb write: {e}")))?;

        let mut hdr = [0u8; 5];
        self.stream
            .read_exact(&mut hdr)
            .map_err(|e| PortalError::Other(format!("wormdb read hdr: {e}")))?;
        let code = hdr[0];
        let len = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        if len > 16 * 1024 * 1024 {
            return Err(PortalError::Other("wormdb response too large".into()));
        }
        let mut body = vec![0u8; len];
        if len > 0 {
            self.stream
                .read_exact(&mut body)
                .map_err(|e| PortalError::Other(format!("wormdb read body: {e}")))?;
        }
        match code {
            RESP_OK | RESP_VALUE => Ok(body),
            RESP_ERR => Err(PortalError::Other(format!(
                "wormdb err: {}",
                String::from_utf8_lossy(&body)
            ))),
            other => Err(PortalError::Other(format!(
                "wormdb unexpected response code {other}"
            ))),
        }
    }
}

fn write_len_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_never_returns_bare_path_name() {
        let p = resolve_wormdb_binary(None, &[]);
        // Must not be a bare PATH entry like "wormdb.exe" with no parent.
        let s = p.to_string_lossy();
        assert!(
            s.contains('/') || s.contains('\\') || s.starts_with("(bundled)"),
            "unexpected bare path: {s}"
        );
    }

    #[test]
    fn resolve_finds_file_in_extra_dir() {
        let tmp =
            std::env::temp_dir().join(format!("portal-wormdb-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let fake = tmp.join(wormdb_file_name());
        std::fs::write(&fake, b"not-a-real-binary").unwrap();
        let p = resolve_wormdb_binary(None, &[tmp.clone()]);
        assert_eq!(p, fake);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
