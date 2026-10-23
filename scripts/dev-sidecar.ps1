# Installs fake-zebrad as the sidecar for the host, so `cargo tauri dev` and
# `cargo tauri build` work without downloading or building the real zebrad.
# Never ship a bundle built this way. macOS/Linux: scripts/dev-sidecar.sh.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$hostTriple = ((rustc -vV) | Select-String '^host: (.+)$').Matches[0].Groups[1].Value
$out = Join-Path $root "src-tauri/binaries/zebra-launcher-zebrad-$hostTriple.exe"

cargo build --manifest-path (Join-Path $root 'Cargo.toml') -p launcher-core --bin fake-zebrad
if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $out) | Out-Null
Copy-Item -Force (Join-Path $targetDir 'debug/fake-zebrad.exe') $out
Write-Host "installed fake-zebrad ($(& $out --version)) at $out"
