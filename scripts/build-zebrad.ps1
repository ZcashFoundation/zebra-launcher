# Builds zebrad from a Zebra release tag and installs it as the Tauri sidecar
# on Windows. The Windows counterpart of scripts/build-zebrad.sh; see that
# script for why the sidecar is built from source and how the build mirrors
# Zebra's own release workflow.
#
# Build dependencies (RocksDB and the zcash FFI are compiled and linked
# statically, and some crates generate protobuf code):
#   - Visual Studio 2022 Build Tools with the "Desktop development with C++"
#     workload (MSVC, Windows SDK, CMake).
#   - LLVM for libclang (bindgen): `choco install llvm` or
#     `winget install LLVM.LLVM`. GitHub's windows-2025 runner ships it in
#     C:\Program Files\LLVM; LIBCLANG_PATH is pointed there when unset.
#   - protoc on PATH: `choco install protoc` or
#     `winget install protobuf`, or arduino/setup-protoc in CI.
#   - rustup and git. The toolchain named in Zebra's rust-toolchain.toml is
#     installed on demand.
#
# The tag must resolve to the pinned commit (ZEBRAD_COMMIT, or -Commit), so a
# moved tag cannot change what gets built.
#
# Usage: scripts/build-zebrad.ps1 [-Version X.Y.Z] [-Commit SHA] [-Target TRIPLE]
#                                 [-Out PATH] [-SrcDir DIR] [-DryRun]
#   -Version  zebrad version without a leading "v" (default: ZEBRAD_VERSION)
#   -Commit   full 40-hex Zebra commit that tag v<version> must point to
#             (default: ZEBRAD_COMMIT when the version is ZEBRAD_VERSION;
#             required for any other version)
#   -Target   Rust target triple (default: the host)
#   -Out      where to install the binary
#             (default: src-tauri\binaries\zebra-launcher-zebrad-<target>.exe)
#   -SrcDir   where Zebra is checked out; cloned at tag v<version> if missing,
#             kept afterwards (default: a temporary directory, removed on exit)
#   -DryRun   print the commands instead of running them (nothing is cloned)

[CmdletBinding()]
param(
    [string]$Version,
    [string]$Commit,
    [string]$Target,
    [string]$Out,
    [string]$SrcDir,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ZebraGit = 'https://github.com/ZcashFoundation/zebra'
$repoRoot = Split-Path -Parent $PSScriptRoot

# Prints a native command, then runs it unless this is a dry run.
function Invoke-Step {
    param([string]$Exe, [string[]]$Arguments)
    Write-Host "+ $Exe $($Arguments -join ' ')"
    if (-not $DryRun) {
        & $Exe @Arguments
        if ($LASTEXITCODE -ne 0) { throw "$Exe failed with exit code $LASTEXITCODE" }
    }
}

if (-not $Version) {
    $versionFile = Join-Path $repoRoot 'ZEBRAD_VERSION'
    if (-not (Test-Path $versionFile)) { throw "no -Version given and $versionFile is missing" }
    $Version = (Get-Content -Raw $versionFile).Trim()
}
if ($Version -match '^[vV]') { throw "version '$Version' must not start with 'v'" }
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$') {
    throw "version '$Version' is not of the form X.Y.Z"
}
$tag = "v$Version"

# ZEBRAD_COMMIT pins the commit of ZEBRAD_VERSION, so it only applies to it.
if (-not $Commit) {
    $versionFile = Join-Path $repoRoot 'ZEBRAD_VERSION'
    $pinnedVersion = if (Test-Path $versionFile) { (Get-Content -Raw $versionFile).Trim() } else { '' }
    if ($Version -ne $pinnedVersion) {
        throw "version $Version is not ZEBRAD_VERSION ($pinnedVersion); pass -Commit"
    }
    $commitFile = Join-Path $repoRoot 'ZEBRAD_COMMIT'
    if (-not (Test-Path $commitFile)) { throw "$commitFile is missing; pass -Commit" }
    $Commit = (Get-Content -Raw $commitFile).Trim()
}
if ($Commit -cnotmatch '^[0-9a-f]{40}$') {
    throw "commit '$Commit' is not a full 40-character lowercase hex SHA"
}

$hostTriple = ((rustc -vV) | Select-String '^host: (.+)$').Matches[0].Groups[1].Value
if (-not $Target) { $Target = $hostTriple }
$exe = if ($Target -like '*-windows-*') { '.exe' } else { '' }
if (-not $Out) {
    $Out = Join-Path $repoRoot "src-tauri\binaries\zebra-launcher-zebrad-$Target$exe"
}

$tempRoot = $null
if (-not $SrcDir) {
    $tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("zebrad-build-" + [guid]::NewGuid())
    $SrcDir = Join-Path $tempRoot 'zebra'
}
# Absolute, so --remap-path-prefix matches the paths rustc sees.
$SrcDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($SrcDir)
$Out = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)

try {
    # 1. Check out the release tag, and check that it is the pinned commit.
    if (Test-Path (Join-Path $SrcDir '.git')) {
        Write-Host "Using the existing checkout in $SrcDir"
    } else {
        Invoke-Step git @('-c', 'advice.detachedHead=false', 'clone', '--depth', '1', '--branch', $tag, $ZebraGit, $SrcDir)
    }
    if (Test-Path (Join-Path $SrcDir '.git')) {
        $head = (git -C $SrcDir rev-parse HEAD).Trim()
        $tagCommit = (git -C $SrcDir rev-parse "refs/tags/$tag^{commit}")
        if ($LASTEXITCODE -ne 0) { throw "$SrcDir has no tag $tag" }
        $tagCommit = $tagCommit.Trim()
        if ($head -ne $tagCommit) { throw "$SrcDir is at $head, not at $tag ($tagCommit)" }
        if ($tagCommit -cne $Commit) { throw "$tag is commit $tagCommit, but the pinned commit is $Commit" }
        Write-Host "$tag is the pinned commit $Commit"
    } else {
        Write-Host "+ check that $tag and HEAD are the pinned commit $Commit"
    }

    # 2. Use the toolchain Zebra pins for this release.
    $toolchainFile = Join-Path $SrcDir 'rust-toolchain.toml'
    if (Test-Path $toolchainFile) {
        $match = Select-String -Path $toolchainFile -Pattern '^\s*channel\s*=\s*"([^"]+)"' | Select-Object -First 1
        if (-not $match) { throw "no channel in $toolchainFile" }
        $toolchain = $match.Matches[0].Groups[1].Value
        $sourceDateEpoch = (git -C $SrcDir log -1 --pretty=%ct).Trim()
    } elseif ($DryRun) {
        $toolchain = "<channel from $toolchainFile>"
        $sourceDateEpoch = "<commit time of $tag>"
    } else {
        throw "$toolchainFile is missing"
    }
    $env:RUSTUP_TOOLCHAIN = $toolchain
    Write-Host "+ `$env:RUSTUP_TOOLCHAIN = '$toolchain'"
    $installed = $false
    if (-not $DryRun) {
        & rustup run $toolchain rustc --version *> $null
        $installed = ($LASTEXITCODE -eq 0)
    }
    if (-not $installed) {
        Invoke-Step rustup @('toolchain', 'install', $toolchain, '--profile', 'minimal')
    }
    Invoke-Step rustup @('target', 'add', '--toolchain', $toolchain, $Target)

    # libclang for bindgen: default to the LLVM that the runner (or the LLVM
    # installer) puts in Program Files.
    if (-not $env:LIBCLANG_PATH) {
        $llvmBin = Join-Path $env:ProgramFiles 'LLVM\bin'
        if (Test-Path (Join-Path $llvmBin 'libclang.dll')) { $env:LIBCLANG_PATH = $llvmBin }
    }
    Write-Host "+ `$env:LIBCLANG_PATH = '$env:LIBCLANG_PATH'"
    if (-not $DryRun) {
        if (-not $env:LIBCLANG_PATH) { throw 'libclang.dll not found: install LLVM or set LIBCLANG_PATH' }
        if (-not (Get-Command protoc -ErrorAction SilentlyContinue)) { throw 'protoc is not on PATH' }
    }

    # 3. Build like Zebra's release workflow does.
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
    $env:SOURCE_DATE_EPOCH = $sourceDateEpoch
    $env:RUSTFLAGS = "--remap-path-prefix=$SrcDir=/zebra --remap-path-prefix=$cargoHome=/cargo"
    $env:CARGO_PROFILE_RELEASE_STRIP = 'symbols'
    Write-Host "+ `$env:SOURCE_DATE_EPOCH = '$env:SOURCE_DATE_EPOCH'"
    Write-Host "+ `$env:RUSTFLAGS = '$env:RUSTFLAGS'"
    Write-Host "+ `$env:CARGO_PROFILE_RELEASE_STRIP = 'symbols'"
    Invoke-Step cargo @(
        'build', '--manifest-path', (Join-Path $SrcDir 'Cargo.toml'),
        '--target-dir', (Join-Path $SrcDir 'target'),
        '--locked', '--release', '-p', 'zebrad', '--bin', 'zebrad',
        '--features', 'default-release-binaries', '--target', $Target
    )

    # 4. Install. Cargo already stripped symbols (CARGO_PROFILE_RELEASE_STRIP);
    #    MSVC keeps debug info in a separate .pdb that is not shipped.
    $built = Join-Path $SrcDir "target\$Target\release\zebrad$exe"
    $tmp = "$Out.tmp$exe"
    Write-Host "+ Copy-Item $built $tmp"
    if (-not $DryRun) {
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Out) | Out-Null
        Copy-Item -Force $built $tmp
    }

    # 5. Smoke test, when the host can run the target.
    if ($DryRun) {
        Write-Host "+ $tmp --version  # must print 'zebrad $Version'"
        Write-Host "+ Move-Item $tmp $Out"
        Write-Host "dry run: zebrad $Version for $Target would be installed at $Out"
        return
    }
    if ($Target -eq $hostTriple) {
        $reported = (& $tmp --version | Select-Object -First 1)
        if ($reported -notmatch ('^zebrad ' + [regex]::Escape($Version) + '($|[ +])')) {
            Remove-Item -Force $tmp
            throw "binary reports '$reported', expected 'zebrad $Version'"
        }
    } else {
        Write-Host "Skipping the --version smoke test: $Target does not run on $hostTriple"
    }
    Move-Item -Force $tmp $Out

    Write-Host "zebrad $Version ($Target): built from $tag ($Commit) with $toolchain, installed at $Out"
} finally {
    if ($tempRoot -and (Test-Path $tempRoot)) { Remove-Item -Recurse -Force $tempRoot }
}
