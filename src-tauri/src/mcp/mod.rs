//! Local MCP server — read-only session search for other agents.
//!
//! **Primary:** HTTP on `127.0.0.1` inside the GUI process ([`http`]) — same
//! AppState, FTS, and WormDB owner as the board.
//! **Secondary:** stdio framing for headless `portal-mcp` / CI.

mod http;

pub use http::{start as start_http, McpHttpRuntime, McpHttpServer};

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use portal_core::adapter::SessionLocator;
use portal_core::search::{self, SearchQuery};
use serde_json::{json, Value};

use crate::state::AppState;

pub const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "agent-portal";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Dispatch one JSON-RPC message.
/// - `None` → notification (no response body)
/// - `Some(value)` → full JSON-RPC response object (`result` or `error`)
pub fn dispatch_rpc(state: &AppState, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    // Notifications (no id)
    if id.is_none() || id.as_ref().is_some_and(|v| v.is_null()) {
        if method == "notifications/initialized" || method == "initialized" {
            eprintln!("mcp: client initialized");
        }
        return None;
    }

    let result = match method {
        "initialize" => Ok(initialize_result()),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list()),
        "tools/call" => handle_tools_call(state, msg.get("params")),
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        "" => Err(rpc_error(-32600, "missing method")),
        other => Err(rpc_error(-32601, &format!("method not found: {other}"))),
    };

    Some(match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err(err) => json!({ "jsonrpc": "2.0", "id": id, "error": err }),
    })
}

/// Run the MCP stdio loop until stdin closes. Logs go to stderr only.
pub fn run_stdio(state: Arc<AppState>) -> io::Result<()> {
    if let Err(e) = refresh_board(&state) {
        eprintln!("mcp: initial board refresh failed: {e}");
    }

    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = io::stdout();

    loop {
        let msg = match read_message(&mut stdin) {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                eprintln!("mcp: read error: {e}");
                break;
            }
        };
        if let Some(response) = dispatch_rpc(&state, &msg) {
            write_message(&mut stdout, &response)?;
        }
    }
    Ok(())
}

fn refresh_board(state: &AppState) -> Result<(), String> {
    let mut board = state.registry.board(&state.env);
    let _ = state.index.apply_generated_titles(&mut board);
    state.cache_installations(board.lanes.iter().filter_map(|lane| {
        lane.agent
            .installation
            .clone()
            .map(|inst| (lane.agent.id.clone(), inst))
    }));
    let _ = state.index.store_board(&board);
    state.publish_board(board);
    Ok(())
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION
        },
        "instructions": "Agent Portal local search: find coding-agent sessions across all installed agents (metadata + FTS content). Read-only. Use search_sessions, then get_session_preview for full IR when needed."
    })
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "search_sessions",
                "description": "Global search across coding-agent sessions (titles, projects, paths, transcript content). Optionally scope to one project via projectId (exact) or project (substring).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "q": { "type": "string", "description": "Search query. Empty returns recent sessions." },
                        "limit": { "type": "integer", "description": "Max hits (1-100, default 30)", "minimum": 1, "maximum": 100 },
                        "agentIds": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional filter to agent ids (e.g. claude-code, codex, grok-build)"
                        },
                        "projectId": {
                            "type": "string",
                            "description": "Exact project scope: cwd_normalized (e.g. p:/agent-portal) or native project key. Prefer this for 'current project only'."
                        },
                        "project": {
                            "type": "string",
                            "description": "Loose substring on project label / path / key (e.g. agent-portal). Use projectId when you need exact identity."
                        },
                        "includeContent": { "type": "boolean", "description": "Include FTS content matches (default true)" },
                        "includeSemantic": { "type": "boolean", "description": "Include WormDB semantic matches (default true when sidecar up)" }
                    },
                    "required": ["q"]
                }
            },
            {
                "name": "search_index_status",
                "description": "FTS/WormDB index coverage and sidecar health (docs indexed vs board sessions, wormdb up).",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "list_agents",
                "description": "List registered agents and whether they are detected on this machine.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "get_session_preview",
                "description": "Full-parse one session into canonical IR (timeline turns). Heavy — use after search_sessions. Optional maxTurns truncates the timeline for context budgets.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agentId": { "type": "string" },
                        "nativeId": { "type": "string" },
                        "storePath": { "type": "string", "description": "Optional store path from search hit" },
                        "maxTurns": { "type": "integer", "description": "If set, keep only the last N non-meta turns" }
                    },
                    "required": ["agentId", "nativeId"]
                }
            },
            {
                "name": "refresh_board",
                "description": "Re-scan all agent stores and refresh the hot board + install cache. Use when search looks stale.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "ensure_wormdb_sidecar",
                "description": "Start or probe the local WormDB sidecar (127.0.0.1). Returns bound port. Needed for semantic path dogfood.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "wormdb_status",
                "description": "WormWire STATUS against the local sidecar (raw UTF-8 diagnostics).",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "reset_semantic_index",
                "description": "Drop WormDB portal-sessions namespace + local semantic_state and kick a full reindex with the current embedder settings.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "set_semantic_indexing_paused",
                "description": "Pause or resume background semantic embedding. Search still works on already-indexed sessions. Body: {\"paused\": true|false}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "paused": { "type": "boolean", "description": "true = pause, false = resume" }
                    },
                    "required": ["paused"]
                }
            }
        ]
    })
}

fn handle_tools_call(state: &AppState, params: Option<&Value>) -> Result<Value, Value> {
    let params = params.ok_or_else(|| rpc_error(-32602, "missing params"))?;
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| rpc_error(-32602, "missing tool name"))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let text = match name {
        "search_sessions" => tool_search_sessions(state, &args)?,
        "search_index_status" => tool_search_index_status(state)?,
        "list_agents" => tool_list_agents(state)?,
        "get_session_preview" => tool_get_session_preview(state, &args)?,
        "refresh_board" => tool_refresh_board(state)?,
        "ensure_wormdb_sidecar" => tool_ensure_wormdb(state)?,
        "wormdb_status" => tool_wormdb_status(state)?,
        "reset_semantic_index" => tool_reset_semantic(state)?,
        "set_semantic_indexing_paused" => tool_set_semantic_paused(state, &args)?,
        other => {
            return Ok(tool_error(&format!("unknown tool: {other}")));
        }
    };
    Ok(tool_text(text))
}

fn tool_search_sessions(state: &AppState, args: &Value) -> Result<String, Value> {
    let q = args
        .get("q")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .clamp(1, 100) as u32;
    let agent_ids: Vec<String> = args
        .get("agentIds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let project = args
        .get("project")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let project_id = args
        .get("projectId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let include_content = args
        .get("includeContent")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let include_semantic = args
        .get("includeSemantic")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let board = state
        .hot_board()
        .ok_or_else(|| rpc_error(-32000, "no board loaded — call refresh_board"))?;
    let query = SearchQuery {
        q,
        limit,
        agent_ids,
        project,
        project_id,
        include_content,
        include_semantic,
    };
    let port = state
        .wormdb
        .as_ref()
        .and_then(|w| w.port())
        .or_else(|| state.ensure_wormdb().ok());
    let cfg = state.settings.load();
    let embedder = portal_core::search::embedder_from_settings(&cfg);
    let resp = search::search_sessions_full(
        &board,
        &query,
        state.search.as_ref(),
        port,
        Some(embedder.as_ref()),
    );
    serde_json::to_string_pretty(&resp).map_err(|e| rpc_error(-32000, &e.to_string()))
}

fn tool_search_index_status(state: &AppState) -> Result<String, Value> {
    let docs = state
        .search
        .as_ref()
        .and_then(|st| st.count_docs().ok())
        .unwrap_or(0);
    let board_sessions = state
        .hot_board()
        .map(|b| {
            b.lanes
                .iter()
                .flat_map(|l| l.projects.iter())
                .map(|p| p.sessions.len() as u32)
                .sum::<u32>()
        })
        .unwrap_or(0);
    let mut semantic_chunks = state
        .search
        .as_ref()
        .and_then(|st| st.count_semantic_sessions().ok())
        .unwrap_or(0);
    let mut hnsw_live = false;
    // Do not ensure_wormdb() on status — can block seconds while spawning.
    let port = state.wormdb.as_ref().and_then(|s| s.port());
    let semantic_enabled = state
        .wormdb
        .as_ref()
        .map(|sc| sc.binary_path().exists())
        .unwrap_or(false);
    if let Some(p) = port {
        if let Ok(client) = portal_core::search::MemClient::connect(p) {
            if let Ok(st) = client.stats() {
                semantic_chunks = st.vectors as u32;
                hnsw_live = st
                    .hnsw
                    .as_ref()
                    .and_then(|h| h.get("live").or_else(|| h.get("nodes")))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    > 0;
            }
        }
    }
    let cfg = state.settings.load();
    let emb = portal_core::search::embedder_from_settings(&cfg);
    let prog = state.semantic_progress();
    let status = json!({
        "contentDocs": docs,
        "boardSessions": board_sessions,
        "contentEnabled": state.search.is_some(),
        "semanticEnabled": semantic_enabled,
        "semanticChunks": semantic_chunks,
        "semanticSessions": state.search.as_ref().and_then(|s| s.count_semantic_sessions().ok()).unwrap_or(0),
        "hnswLive": hnsw_live,
        "wormdbPort": port,
        "wormdbBinary": state.wormdb.as_ref().map(|s| s.binary_path().display().to_string()),
        "embedder": emb.id(),
        "semanticError": prog.last_error,
        "semanticActive": prog.active,
        "semanticCurrent": prog.current,
        "semanticPaused": state.semantic_paused() || prog.paused,
        "semanticPending": prog.sessions_pending,
        "semanticEtaSecs": prog.eta_secs,
        "semanticRatePerMin": prog.rate_sessions_per_min,
        "semanticDoneWindow": prog.sessions_done_window,
        "namespace": portal_core::search::MEMORY_NAMESPACE,
    });
    Ok(status.to_string())
}

fn tool_reset_semantic(state: &AppState) -> Result<String, Value> {
    let cfg = state.settings.load();
    match crate::semantic_index::reset_semantic_index(state, &cfg) {
        Ok(()) => Ok(format!(
            "Semantic index cleared; reindexing with embedder {}",
            cfg.embedder_id()
        )),
        Err(e) => Err(rpc_error(-32000, &e)),
    }
}

fn tool_set_semantic_paused(state: &AppState, args: &Value) -> Result<String, Value> {
    let paused = args
        .get("paused")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| rpc_error(-32602, "paused (boolean) required"))?;
    state.set_semantic_paused(paused);
    Ok(if paused {
        "Semantic indexing paused (search still uses already-indexed sessions).".into()
    } else {
        "Semantic indexing resumed.".into()
    })
}

fn tool_list_agents(state: &AppState) -> Result<String, Value> {
    let descriptors = state.registry.descriptors(&state.env);
    serde_json::to_string_pretty(&descriptors).map_err(|e| rpc_error(-32000, &e.to_string()))
}

fn tool_get_session_preview(state: &AppState, args: &Value) -> Result<String, Value> {
    let agent_id = args
        .get("agentId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| rpc_error(-32602, "agentId required"))?
        .to_string();
    let native_id = args
        .get("nativeId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| rpc_error(-32602, "nativeId required"))?
        .to_string();
    let store_path = args
        .get("storePath")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let max_turns = args
        .get("maxTurns")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    let adapter = state
        .adapter(&agent_id)
        .ok_or_else(|| rpc_error(-32000, &format!("unknown agent '{agent_id}'")))?;
    let inst = state
        .installation(&agent_id)
        .ok_or_else(|| rpc_error(-32000, &format!("agent '{agent_id}' not detected")))?;
    let mut session = adapter
        .read_session(
            &inst,
            &SessionLocator {
                native_id,
                store_path: store_path.map(Into::into),
            },
        )
        .map_err(|e| rpc_error(-32000, &e.to_string()))?;

    if let Some(max) = max_turns {
        // Keep last N non-meta turns for context budget.
        let mut kept: Vec<_> = session
            .timeline
            .into_iter()
            .filter(|t| !t.is_meta)
            .collect();
        if kept.len() > max {
            kept = kept.split_off(kept.len() - max);
        }
        session.timeline = kept;
    }

    // Strip raw source lines if any slipped through for smaller payloads.
    for t in &mut session.timeline {
        t.raw = None;
    }

    serde_json::to_string_pretty(&session).map_err(|e| rpc_error(-32000, &e.to_string()))
}

fn tool_refresh_board(state: &AppState) -> Result<String, Value> {
    refresh_board(state).map_err(|e| rpc_error(-32000, &e))?;
    let n = state
        .hot_board()
        .map(|b| {
            b.lanes
                .iter()
                .flat_map(|l| l.projects.iter())
                .map(|p| p.sessions.len())
                .sum::<usize>()
        })
        .unwrap_or(0);
    Ok(json!({ "ok": true, "sessions": n }).to_string())
}

fn tool_ensure_wormdb(state: &AppState) -> Result<String, Value> {
    match state.ensure_wormdb() {
        Ok(port) => Ok(json!({ "ok": true, "port": port, "host": "127.0.0.1" }).to_string()),
        Err(e) => Ok(json!({ "ok": false, "error": e.to_string() }).to_string()),
    }
}

fn tool_wormdb_status(state: &AppState) -> Result<String, Value> {
    let port = match state.wormdb.as_ref().and_then(|s| s.port()) {
        Some(p) => p,
        None => match state.ensure_wormdb() {
            Ok(p) => p,
            Err(e) => {
                return Ok(json!({ "ok": false, "error": e.to_string() }).to_string());
            }
        },
    };
    match portal_core::search::WormWireClient::connect(port) {
        Ok(mut c) => match c.status() {
            Ok(bytes) => Ok(json!({
                "ok": true,
                "port": port,
                "status": String::from_utf8_lossy(&bytes),
            })
            .to_string()),
            Err(e) => Ok(json!({ "ok": false, "port": port, "error": e.to_string() }).to_string()),
        },
        Err(e) => Ok(json!({ "ok": false, "port": port, "error": e.to_string() }).to_string()),
    }
}

fn tool_text(text: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    })
}

fn tool_error(msg: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": msg }],
        "isError": true
    })
}

fn rpc_error(code: i64, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

fn write_message(out: &mut impl Write, value: &Value) -> io::Result<()> {
    let payload = serde_json::to_vec(value)?;
    write!(out, "Content-Length: {}\r\n\r\n", payload.len())?;
    out.write_all(&payload)?;
    out.flush()?;
    Ok(())
}

fn read_message(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = input.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
        // Ignore Content-Type and other headers.
    }
    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut buf = vec![0u8; len];
    input.read_exact(&mut buf)?;
    let value: Value =
        serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(value))
}
