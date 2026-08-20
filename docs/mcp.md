# Agent Portal MCP

Local, **read-only** session search for other coding agents. Tools share the
same hot board, FTS index (`search.db`), and WormDB sidecar as the GUI.

## Primary: HTTP (in-process with the app)

When Agent Portal is running, MCP is served on **loopback only**:

| | |
| --- | --- |
| Default URL | `http://127.0.0.1:17432/mcp` |
| Health | `GET http://127.0.0.1:17432/health` |
| Discovery | `GET http://127.0.0.1:17432/mcp` |
| RPC | `POST http://127.0.0.1:17432/mcp` with a JSON-RPC 2.0 body |
| Auth | Optional `Authorization: Bearer <token>` (Settings) |
| Bind | **127.0.0.1 only** — never all interfaces |

Toggle / port / token: **Settings → Local MCP (HTTP)**.  
Runtime status: `mcp_http_status` IPC (also shown in Settings).

### Why HTTP here

- **Single owner** of WormDB + FTS + board (no second process fighting the lock).
- Multiple clients (Claude, Codex, Grok, scripts) hit the **same live index**.
- Matches what you see in `Ctrl+K`.

### Example: initialize + search

```powershell
# Health
curl http://127.0.0.1:17432/health

# JSON-RPC tools/list
curl -s -X POST http://127.0.0.1:17432/mcp `
  -H "Content-Type: application/json" `
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'

curl -s -X POST http://127.0.0.1:17432/mcp `
  -H "Content-Type: application/json" `
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_sessions","arguments":{"q":"wormdb","limit":5}}}'
```

If a bearer token is set:

```powershell
curl -s -X POST http://127.0.0.1:17432/mcp `
  -H "Content-Type: application/json" `
  -H "Authorization: Bearer YOUR_TOKEN" `
  -d '...'
```

### Client config sketch (HTTP)

Clients that support URL-based MCP:

```json
{
  "mcpServers": {
    "agent-portal": {
      "url": "http://127.0.0.1:17432/mcp"
    }
  }
}
```

(Exact field names vary by client — some use `type: "http"` + `url`.)

### Smoke

```powershell
# App must be running with MCP enabled
pwsh -File scripts/mcp-http-smoke.ps1
```

---

## Secondary: stdio binary (`portal-mcp`)

Headless process for CI / machines without the GUI:

```powershell
cargo build -p portal-tauri --bin portal-mcp
pnpm mcp
pnpm mcp:smoke
```

Uses the same tool handlers. **Does not** share a live WormDB instance with the
GUI (sidecar lock). Prefer HTTP when the app is open.

---

## Tools

| Tool | Purpose |
| --- | --- |
| `search_sessions` | Global meta + FTS (`q`, `limit`, `agentIds`, `projectId` exact, `project` substring, `includeContent`) |
| `search_index_status` | FTS coverage + WormDB port / HNSW flag |
| `list_agents` | Registered adapters + detection |
| `get_session_preview` | Canonical IR (`maxTurns` optional) |
| `refresh_board` | Re-scan agent stores |
| `ensure_wormdb_sidecar` | Start/probe loopback WormDB |
| `wormdb_status` | WormWire `STATUS` |

No migrate / write / undo in v1.
