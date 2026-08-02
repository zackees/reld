param(
    [string]$Profile = "debug",
    [string]$Target = ""
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$targetRoot = if ($env:CARGO_TARGET_DIR) {
    if ([IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) {
        $env:CARGO_TARGET_DIR
    } else {
        Join-Path $repoRoot $env:CARGO_TARGET_DIR
    }
} else {
    Join-Path $repoRoot "target"
}
$binDir = if ($Target) {
    Join-Path (Join-Path $targetRoot $Target) $Profile
} else {
    Join-Path $targetRoot $Profile
}
$sourceBin = Join-Path $binDir "reld.exe"

if (-not (Test-Path -LiteralPath $sourceBin)) {
    throw "Build reld before installing driver shims: $sourceBin is missing"
}

Copy-Item -LiteralPath $sourceBin -Destination (Join-Path $binDir "ld.reld.exe") -Force
Copy-Item -LiteralPath $sourceBin -Destination (Join-Path $binDir "ld64.reld.exe") -Force
