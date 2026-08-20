//! In-process MCP over HTTP — loopback only, shares GUI AppState.
//!
//! Endpoints:
//! - `GET  /health` — liveness + bound port
//! - `GET  /mcp`    — human/client discovery (URL, protocol notes)
//! - `POST /mcp`    — single JSON-RPC 2.0 request → JSON-RPC response
//!
//! Optional `Authorization: Bearer <token>` when a token is configured.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::dispatch_rpc;
use crate::state::AppState;

/// Live HTTP MCP listener status (shared with UI / IPC).
#[derive(Debug, Clone, Default)]
pub struct McpHttpRuntime {
    pub running: bool,
    pub port: Option<u16>,
    pub token_required: bool,
}

pub struct McpHttpServer {
    stop: Arc<AtomicBool>,
    /// Join handle of the accept loop (not joined on drop — process exit cleans up).
    _thread: Option<thread::JoinHandle<()>>,
    runtime: Arc<Mutex<McpHttpRuntime>>,
}

impl McpHttpServer {
    pub fn runtime_snapshot(&self) -> McpHttpRuntime {
        self.runtime.lock().unwrap().clone()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // Nudge accept out of blocking by connecting once.
        if let Some(port) = self.runtime.lock().unwrap().port {
            let _ = TcpStream::connect(format!("127.0.0.1:{port}"));
        }
    }
}

impl Drop for McpHttpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Bind `127.0.0.1:port` (0 = ephemeral) and serve until process exit / stop.
pub fn start(
    state: Arc<AppState>,
    port: u16,
    token: Option<String>,
) -> Result<McpHttpServer, String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("MCP HTTP bind 127.0.0.1:{port}: {e}"))?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("MCP HTTP set_nonblocking: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("MCP HTTP local_addr: {e}"))?
        .port();

    let stop = Arc::new(AtomicBool::new(false));
    let runtime = Arc::new(Mutex::new(McpHttpRuntime {
        running: true,
        port: Some(bound),
        token_required: token.as_ref().is_some_and(|t| !t.is_empty()),
    }));

    let stop_t = stop.clone();
    let runtime_t = runtime.clone();
    let token = token.filter(|t| !t.is_empty());

    let handle = thread::Builder::new()
        .name("mcp-http".into())
        .spawn(move || {
            eprintln!("mcp-http: listening on http://127.0.0.1:{bound}/mcp");
            // Accept timeout so we can notice stop flag.
            let _ = listener.set_nonblocking(true);
            while !stop_t.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let token = token.clone();
                        // Handle request on a short-lived worker so accept stays hot.
                        let _ =
                            thread::Builder::new()
                                .name("mcp-http-req".into())
                                .spawn(move || {
                                    if let Err(e) =
                                        handle_connection(stream, &state, token.as_deref())
                                    {
                                        eprintln!("mcp-http: request error: {e}");
                                    }
                                });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => {
                        if stop_t.load(Ordering::SeqCst) {
                            break;
                        }
                        eprintln!("mcp-http: accept error: {e}");
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
            if let Ok(mut rt) = runtime_t.lock() {
                rt.running = false;
            }
            eprintln!("mcp-http: stopped");
        })
        .map_err(|e| format!("spawn mcp-http: {e}"))?;

    Ok(McpHttpServer {
        stop,
        _thread: Some(handle),
        runtime,
    })
}

fn handle_connection(
    mut stream: TcpStream,
    state: &AppState,
    token: Option<&str>,
) -> Result<(), String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0usize;
    // Read until headers complete; then body by Content-Length.
    loop {
        if got >= buf.len() {
            return Err("request too large".into());
        }
        let n = stream
            .read(&mut buf[got..])
            .map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        got += n;
        if let Some(header_end) = find_header_end(&buf[..got]) {
            let header_text = std::str::from_utf8(&buf[..header_end])
                .map_err(|e| format!("headers utf8: {e}"))?;
            let (method, path, content_length, auth) = parse_request_line_and_headers(header_text)?;
            let body_start = header_end;
            let mut body = buf[body_start..got].to_vec();
            while body.len() < content_length {
                let mut chunk = vec![0u8; (content_length - body.len()).min(64 * 1024)];
                let n = stream
                    .read(&mut chunk)
                    .map_err(|e| format!("read body: {e}"))?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            body.truncate(content_length);

            if let Some(expected) = token {
                if !auth_ok(auth.as_deref(), expected) {
                    write_http(
                        &mut stream,
                        401,
                        "application/json",
                        br#"{"error":"unauthorized"}"#,
                    )?;
                    let _ = stream.shutdown(Shutdown::Both);
                    return Ok(());
                }
            }

            match (method.as_str(), path.as_str()) {
                ("GET", "/health") | ("GET", "/health/") => {
                    let body = json!({
                        "ok": true,
                        "service": "agent-portal-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    });
                    write_json(&mut stream, 200, &body)?;
                }
                ("GET", "/mcp") | ("GET", "/mcp/") => {
                    let body = json!({
                        "name": "agent-portal",
                        "transport": "http+json-rpc",
                        "protocolVersion": super::PROTOCOL_VERSION,
                        "endpoints": {
                            "rpc": "POST /mcp",
                            "health": "GET /health"
                        },
                        "auth": if token.is_some() { "bearer" } else { "none" },
                        "instructions": "POST a JSON-RPC 2.0 body (initialize, tools/list, tools/call). Same tools as stdio MCP."
                    });
                    write_json(&mut stream, 200, &body)?;
                }
                ("POST", "/mcp") | ("POST", "/mcp/") => {
                    let msg: Value =
                        serde_json::from_slice(&body).map_err(|e| format!("json body: {e}"))?;
                    match dispatch_rpc(state, &msg) {
                        None => {
                            // Notification — empty 204
                            write_http(&mut stream, 204, "text/plain", b"")?;
                        }
                        Some(response) => {
                            write_json(&mut stream, 200, &response)?;
                        }
                    }
                }
                ("OPTIONS", _) => {
                    write_http(&mut stream, 204, "text/plain", b"")?;
                }
                _ => {
                    write_http(
                        &mut stream,
                        404,
                        "application/json",
                        br#"{"error":"not found"}"#,
                    )?;
                }
            }
            let _ = stream.shutdown(Shutdown::Both);
            return Ok(());
        }
    }
    Ok(())
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn parse_request_line_and_headers(
    headers: &str,
) -> Result<(String, String, usize, Option<String>), String> {
    let mut lines = headers.lines();
    let request_line = lines.next().ok_or("empty request")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    // Strip query string
    let path = path.split('?').next().unwrap_or(&path).to_string();

    let mut content_length = 0usize;
    let mut auth = None;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.split_once(':') {
            if rest.0.eq_ignore_ascii_case("authorization") {
                auth = Some(rest.1.trim().to_string());
            }
        }
    }
    Ok((method, path, content_length, auth))
}

fn auth_ok(auth_header: Option<&str>, expected: &str) -> bool {
    let Some(h) = auth_header else {
        return false;
    };
    let h = h.trim();
    if let Some(rest) = h
        .strip_prefix("Bearer ")
        .or_else(|| h.strip_prefix("bearer "))
    {
        return rest.trim() == expected;
    }
    // Also accept raw token for simple curl tests.
    h == expected
}

fn write_json(stream: &mut TcpStream, status: u16, value: &Value) -> Result<(), String> {
    let payload = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    write_http(stream, status, "application/json", &payload)
}

fn write_http(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Headers: Authorization, Content-Type\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .map_err(|e| format!("write header: {e}"))?;
    if !body.is_empty() {
        stream
            .write_all(body)
            .map_err(|e| format!("write body: {e}"))?;
    }
    stream.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}
