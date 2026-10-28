# Installs fake-zebrad as the sidecar for the host, so `cargo tauri dev` and
# `cargo tauri build` work without downloading or building the real zebrad.
# Never ship a bundle built this way. macOS/Linux: scripts/dev-sidecar.sh.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$hostTriple = ((rustc -vV) | Select-String '^host: (.+)$').Matches[0].Groups[1].Value
$out = Join-Path $root "src-tauri/binaries/zebra-launcher-zebrad-$hostTriple.exe"

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

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $out) | Out-Null
Copy-Item -Force $built $out
Write-Host "installed fake-zebrad ($(& $out --version)) at $out"
