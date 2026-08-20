# Minimal MCP stdio smoke test for portal-mcp.
# Sends initialize, tools/list, tools/call search_index_status, search_sessions.

$ErrorActionPreference = "Stop"
$bin = Join-Path $PSScriptRoot "..\target\debug\portal-mcp.exe"
if (-not (Test-Path $bin)) {
    Write-Host "Building portal-mcp..."
    Push-Location (Join-Path $PSScriptRoot "..")
    cargo build -p portal-tauri --bin portal-mcp
    Pop-Location
}

function Send-McpMessage($stdinWriter, $obj) {
    $json = $obj | ConvertTo-Json -Compress -Depth 20
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
    $header = "Content-Length: $($bytes.Length)`r`n`r`n"
    $stdinWriter.Write($header)
    $stdinWriter.Write($json)
    $stdinWriter.Flush()
}

function Read-McpMessage($stdoutReader) {
    $contentLength = $null
    while ($true) {
        $line = $stdoutReader.ReadLine()
        if ($null -eq $line) { return $null }
        if ($line -eq "") { break }
        if ($line -match '^Content-Length:\s*(\d+)') {
            $contentLength = [int]$Matches[1]
        }
    }
    if ($null -eq $contentLength) { throw "missing Content-Length" }
    $buf = New-Object char[] $contentLength
    $read = 0
    while ($read -lt $contentLength) {
        $n = $stdoutReader.Read($buf, $read, $contentLength - $read)
        if ($n -le 0) { throw "unexpected EOF" }
        $read += $n
    }
    $text = -join $buf
    return $text | ConvertFrom-Json
}

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = (Resolve-Path $bin).Path
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.CreateNoWindow = $true
if ($env:AGENT_PORTAL_DATA_DIR) {
    $psi.Environment["AGENT_PORTAL_DATA_DIR"] = $env:AGENT_PORTAL_DATA_DIR
}

$p = [System.Diagnostics.Process]::Start($psi)
$stdin = $p.StandardInput
$stdout = $p.StandardOutput

try {
    Send-McpMessage $stdin @{
        jsonrpc = "2.0"
        id      = 1
        method  = "initialize"
        params  = @{
            protocolVersion = "2024-11-05"
            capabilities    = @{}
            clientInfo      = @{ name = "mcp-smoke"; version = "0.1" }
        }
    }
    $init = Read-McpMessage $stdout
    Write-Host "initialize:" ($init.result.serverInfo | ConvertTo-Json -Compress)

    Send-McpMessage $stdin @{
        jsonrpc = "2.0"
        method  = "notifications/initialized"
    }

    Send-McpMessage $stdin @{
        jsonrpc = "2.0"
        id      = 2
        method  = "tools/list"
        params  = @{}
    }
    $tools = Read-McpMessage $stdout
    $names = @($tools.result.tools | ForEach-Object { $_.name })
    Write-Host "tools:" ($names -join ", ")

    Send-McpMessage $stdin @{
        jsonrpc = "2.0"
        id      = 3
        method  = "tools/call"
        params  = @{
            name      = "refresh_board"
            arguments = @{}
        }
    }
    $ref = Read-McpMessage $stdout
    Write-Host "refresh_board:" $ref.result.content[0].text

    Send-McpMessage $stdin @{
        jsonrpc = "2.0"
        id      = 4
        method  = "tools/call"
        params  = @{
            name      = "search_index_status"
            arguments = @{}
        }
    }
    $st = Read-McpMessage $stdout
    Write-Host "search_index_status:" $st.result.content[0].text

    Send-McpMessage $stdin @{
        jsonrpc = "2.0"
        id      = 5
        method  = "tools/call"
        params  = @{
            name      = "search_sessions"
            arguments = @{ q = ""; limit = 5 }
        }
    }
    $search = Read-McpMessage $stdout
    $preview = $search.result.content[0].text
    if ($preview.Length -gt 500) { $preview = $preview.Substring(0, 500) + "…" }
    Write-Host "search_sessions (recent):" $preview

    Write-Host "OK — MCP smoke passed"
}
finally {
    try { $stdin.Close() } catch {}
    if (-not $p.HasExited) {
        $p.Kill()
        $p.WaitForExit(3000) | Out-Null
    }
    $err = $p.StandardError.ReadToEnd()
    if ($err) { Write-Host "stderr:`n$err" }
}
