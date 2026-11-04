# Installs fake-zebrad as all three channel sidecars for the host, so
# `cargo tauri dev` and `cargo tauri build` work without downloading or building
# the real zebrad. Never ship a bundle built this way. macOS/Linux:
# scripts/dev-sidecar.sh.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$hostTriple = ((rustc -vV) | Select-String '^host: (.+)$').Matches[0].Groups[1].Value

# Take the binary's path from cargo, which knows about build.target-dir,
# CARGO_TARGET_DIR and CARGO_BUILD_TARGET. Diagnostics still go to stderr.
$messages = cargo build --manifest-path (Join-Path $root 'Cargo.toml') -p launcher-core --bin fake-zebrad `
    --message-format=json-render-diagnostics
if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
$built = $messages |
    ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'fake-zebrad' -and $_.executable } |
    Select-Object -Last 1 -ExpandProperty executable
if (-not $built) { throw 'cargo did not report where it built fake-zebrad' }

$binaryDir = Join-Path $root 'src-tauri/binaries'
New-Item -ItemType Directory -Force -Path $binaryDir | Out-Null
foreach ($channel in @('stable', 'beta', 'nightly')) {
    $suffix = if ($channel -eq 'stable') { '' } else { "-$channel" }
    $out = Join-Path $binaryDir "zeal-zebrad$suffix-$hostTriple.exe"
    Copy-Item -Force $built $out
    Write-Host "installed $channel fake-zebrad ($(& $out --version)) at $out"
}
