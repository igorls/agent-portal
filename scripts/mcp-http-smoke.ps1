# Smoke-test in-process HTTP MCP (app must be running).
$ErrorActionPreference = "Stop"
$base = if ($env:AGENT_PORTAL_MCP_URL) { $env:AGENT_PORTAL_MCP_URL.TrimEnd('/') } else { "http://127.0.0.1:17432" }
$token = $env:AGENT_PORTAL_MCP_TOKEN

function Invoke-McpRpc($method, $params, $id) {
    $body = @{
        jsonrpc = "2.0"
        id      = $id
        method  = $method
        params  = $params
    } | ConvertTo-Json -Compress -Depth 10
    $headers = @{ "Content-Type" = "application/json" }
    if ($token) { $headers["Authorization"] = "Bearer $token" }
    return Invoke-RestMethod -Uri "$base/mcp" -Method Post -Headers $headers -Body $body
}

Write-Host "GET $base/health"
$health = Invoke-RestMethod -Uri "$base/health" -Method Get
Write-Host ($health | ConvertTo-Json -Compress)

Write-Host "initialize"
$init = Invoke-McpRpc "initialize" @{
    protocolVersion = "2024-11-05"
    capabilities    = @{}
    clientInfo      = @{ name = "mcp-http-smoke"; version = "0.1" }
} 1
Write-Host ($init.result.serverInfo | ConvertTo-Json -Compress)

Write-Host "tools/list"
$tools = Invoke-McpRpc "tools/list" @{} 2
Write-Host (($tools.result.tools | ForEach-Object { $_.name }) -join ", ")

Write-Host "search_index_status"
$st = Invoke-McpRpc "tools/call" @{
    name      = "search_index_status"
    arguments = @{}
} 3
Write-Host $st.result.content[0].text

Write-Host "search_sessions q=portal limit=3"
$search = Invoke-McpRpc "tools/call" @{
    name      = "search_sessions"
    arguments = @{ q = "portal"; limit = 3 }
} 4
$preview = $search.result.content[0].text
if ($preview.Length -gt 400) { $preview = $preview.Substring(0, 400) + "…" }
Write-Host $preview

Write-Host "OK — HTTP MCP smoke passed"
