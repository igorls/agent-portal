# Copy a WormDB release binary into src-tauri/binaries for bundling.
# Build for shipping with a portable CPU baseline (required for clean Windows machines):
#   cd P:\wormdb
#   zig build -Doptimize=ReleaseSmall -Dcpu=baseline
# Usage:
#   .\scripts\vendor-wormdb.ps1
#   .\scripts\vendor-wormdb.ps1 -Source P:\wormdb\zig-out\bin\wormdb.exe
#   $env:WORMDB_SRC = "..."; .\scripts\vendor-wormdb.ps1

param(
    [string]$Source = $env:WORMDB_SRC
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$DestDir = Join-Path $Root "src-tauri\binaries"
$Triple = "x86_64-pc-windows-msvc"

if (-not $Source) {
    $candidates = @(
        "P:\wormdb\zig-out\bin\wormdb.exe",
        (Join-Path $env:USERPROFILE "src\wormdb\zig-out\bin\wormdb.exe"),
        (Join-Path $env:USERPROFILE "wormdb\zig-out\bin\wormdb.exe")
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) {
            $Source = $c
            break
        }
    }
}

if (-not $Source -or -not (Test-Path -LiteralPath $Source)) {
    Write-Error @"
No WormDB binary found.
Build WormDB (ReleaseSmall) and pass -Source path\to\wormdb.exe
or set WORMDB_SRC.
"@
}

New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
$destTriple = Join-Path $DestDir "wormdb-$Triple.exe"
$destPlain = Join-Path $DestDir "wormdb.exe"
Copy-Item -LiteralPath $Source -Destination $destTriple -Force
Copy-Item -LiteralPath $Source -Destination $destPlain -Force

# Dev: also next to cargo debug/release outputs when present.
foreach ($cfg in @("debug", "release")) {
    $out = Join-Path $Root "target\$cfg"
    if (Test-Path $out) {
        Copy-Item -LiteralPath $Source -Destination (Join-Path $out "wormdb.exe") -Force
    }
}

Write-Host "Vendored WormDB:"
Write-Host "  $destTriple"
Write-Host "  $destPlain"
Get-Item $destTriple | Format-List Name, Length, LastWriteTime
