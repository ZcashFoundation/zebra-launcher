#Requires -Version 7

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
#   - PowerShell 7 (pwsh). Windows PowerShell 5.1 turns a native command's
#     stderr into terminating errors under $ErrorActionPreference = 'Stop'.
#
# The tag must resolve to the pinned commit (ZEBRAD_COMMIT, or -Commit), so a
# moved tag cannot change what gets built.
#
# Development builds: -Ref builds from any branch, tag or full commit SHA of
# the Zebra repository instead (for example `-Ref main`), the same way. The
# result is NOT a verified release: nothing pins the commit, and the
# `--version` smoke test only checks that the binary is zebrad. See
# docs/SIDECARS.md, "Development builds from a Zebra git ref".
#
# Usage: scripts/build-zebrad.ps1 [-Version X.Y.Z] [-Commit SHA] [-Target TRIPLE]
#                                 [-Out PATH] [-SrcDir DIR] [-FetchOnly] [-DryRun]
#        scripts/build-zebrad.ps1 -Ref REF [-Target TRIPLE] [-Out PATH]
#                                 [-SrcDir DIR] [-FetchOnly] [-DryRun]
#   -Version    zebrad version without a leading "v" (default: ZEBRAD_VERSION)
#   -Commit     full 40-hex Zebra commit that tag v<version> must point to
#               (default: ZEBRAD_COMMIT when the version is ZEBRAD_VERSION;
#               required for any other version)
#   -Ref        DEVELOPMENT BUILD: build from this Zebra branch, tag or full
#               40-hex commit instead of a verified release; not combinable
#               with -Version or -Commit
#   -Target     Rust target triple (default: the host)
#   -Out        where to install the binary
#               (default: src-tauri\binaries\zebra-launcher-zebrad-<target>.exe)
#   -SrcDir     where Zebra is checked out; cloned at tag v<version> (or
#               fetched at -Ref) if missing, kept afterwards (default: a
#               temporary directory, removed on exit). An existing checkout
#               must be at the tag (or at what -Ref names in it).
#   -FetchOnly  stop after checking out and checking the source (needs -SrcDir)
#   -DryRun     print the commands instead of running them (nothing is cloned)

[CmdletBinding()]
param(
    [string]$Version,
    [string]$Commit,
    [string]$Ref,
    [string]$Target,
    [string]$Out,
    [string]$SrcDir,
    [switch]$FetchOnly,
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

# Runs a native command whose failure the caller handles: neither its stderr
# nor its exit code raises a PowerShell error. Returns its exit code, its
# trimmed stdout and its trimmed stderr.
function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments)
    if (-not (Get-Command $Exe -ErrorAction SilentlyContinue)) { throw "$Exe is not on PATH" }
    $ErrorActionPreference = 'Continue'
    $output = & $Exe @Arguments 2>&1
    $exitCode = $LASTEXITCODE
    $stdout = @($output | Where-Object { $_ -isnot [System.Management.Automation.ErrorRecord] })
    $stderr = @($output | Where-Object { $_ -is [System.Management.Automation.ErrorRecord] })
    [pscustomobject]@{
        ExitCode = $exitCode
        Stdout = ($stdout -join "`n").Trim()
        Stderr = ($stderr -join "`n").Trim()
    }
}

# Runs git in the Zebra checkout and returns its trimmed stdout, or throws
# "${Failure}: <git's stderr>" if git fails.
function Invoke-Git {
    param([string[]]$Arguments, [string]$Failure)
    $result = Invoke-Native git (@('-C', $SrcDir) + $Arguments)
    if ($result.ExitCode -ne 0) { throw "${Failure}: $($result.Stderr)" }
    $result.Stdout
}

$isSha = $false
if ($Ref) {
    # A development build: no version, and the commit is whatever the ref
    # names when it is fetched.
    if ($Version -or $Commit) { throw '-Ref cannot be combined with -Version or -Commit' }
    if ($Ref -match '^-|\.\.|/$|\.lock$' -or $Ref -cnotmatch '^[A-Za-z0-9][A-Za-z0-9._/+-]*$') {
        throw "'$Ref' is not a usable git ref (allowed: letters, digits and ._/+-)"
    }
    $isSha = $Ref -cmatch '^[0-9a-f]{40}$'
    $label = $Ref
} else {
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
    $label = $tag
}
# Without -SrcDir the checkout is a temporary directory, removed on exit.
if ($FetchOnly -and -not $SrcDir) { throw '-FetchOnly needs -SrcDir' }

# Fetching needs no Rust toolchain.
$hostTriple = ''
if (-not $FetchOnly) {
    $rustcInfo = Invoke-Native rustc @('-vV')
    if ($rustcInfo.ExitCode -ne 0 -or $rustcInfo.Stdout -notmatch '(?m)^host: (\S+)') {
        throw "rustc -vV failed: $($rustcInfo.Stderr)"
    }
    $hostTriple = $Matches[1]
}
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

# The build sets these for cargo; they are restored when the script ends, so
# they do not leak into the caller's session.
$buildEnvNames = @(
    'RUSTUP_TOOLCHAIN', 'LIBCLANG_PATH', 'SOURCE_DATE_EPOCH',
    'CARGO_ENCODED_RUSTFLAGS', 'CARGO_PROFILE_RELEASE_STRIP'
)
$savedEnv = @{}
foreach ($name in $buildEnvNames) {
    $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name)
}

try {
    if ($Ref) {
        # 1. Fetch exactly the ref. There is no pinned commit to check against:
        #    the commit is recorded instead.
        if (Test-Path (Join-Path $SrcDir '.git')) {
            Write-Host "Using the existing checkout in $SrcDir"
            $named = Invoke-Git @('rev-parse', '--verify', '-q', "$Ref^{commit}") "$Ref does not name a commit in $SrcDir"
            $head = Invoke-Git @('rev-parse', 'HEAD') "cannot read HEAD in $SrcDir"
            if ($head -ne $named) { throw "$SrcDir is at $head, not at $Ref ($named)" }
        } else {
            Invoke-Step git @('init', '-q', $SrcDir)
            Invoke-Step git @('-C', $SrcDir, 'remote', 'add', 'origin', $ZebraGit)
            # GitHub serves branches, tags and full commit SHAs to a shallow fetch.
            $checkoutRev = 'FETCH_HEAD'
            Write-Host "+ git -C $SrcDir fetch --depth 1 origin $Ref"
            if (-not $DryRun) {
                $fetch = Invoke-Native git @('-C', $SrcDir, 'fetch', '--depth', '1', 'origin', $Ref)
                if ($fetch.Stderr) { Write-Host $fetch.Stderr }
                if ($fetch.ExitCode -ne 0) {
                    if (-not $isSha) { throw "cannot fetch $Ref from $ZebraGit" }
                    # A server may refuse to send a commit by SHA; a full fetch of
                    # the branches and tags has it if any of them reaches it.
                    Write-Host "The shallow fetch of $Ref was refused; fetching all branches and tags"
                    Invoke-Step git @('-C', $SrcDir, 'fetch', '--tags', 'origin', '+refs/heads/*:refs/remotes/origin/*')
                    $checkoutRev = $Ref
                }
            }
            Invoke-Step git @('-C', $SrcDir, '-c', 'advice.detachedHead=false', 'checkout', '--detach', $checkoutRev)
        }
        if (Test-Path (Join-Path $SrcDir '.git')) {
            $Commit = Invoke-Git @('rev-parse', 'HEAD') "cannot read HEAD in $SrcDir"
            if ($isSha -and $Commit -cne $Ref) { throw "$SrcDir is at $Commit, not at $Ref" }
        } else {
            $Commit = "<commit of $Ref>"
        }
        $devBuild = "DEV BUILD: zebrad from $Ref @ $Commit, not a verified release"
        $banner = '#' * 80
        Write-Host $banner
        Write-Host $devBuild
        Write-Host $banner
    } else {
        # 1. Check out the release tag, and check that it is the pinned commit.
        if (Test-Path (Join-Path $SrcDir '.git')) {
            Write-Host "Using the existing checkout in $SrcDir"
        } else {
            Invoke-Step git @('-c', 'advice.detachedHead=false', 'clone', '--depth', '1', '--branch', $tag, $ZebraGit, $SrcDir)
        }
        if (Test-Path (Join-Path $SrcDir '.git')) {
            $head = Invoke-Git @('rev-parse', 'HEAD') "cannot read HEAD in $SrcDir"
            $tagCommit = Invoke-Git @('rev-parse', "refs/tags/$tag^{commit}") "$SrcDir has no tag $tag"
            if ($head -ne $tagCommit) { throw "$SrcDir is at $head, not at $tag ($tagCommit)" }
            if ($tagCommit -cne $Commit) { throw "$tag is commit $tagCommit, but the pinned commit is $Commit" }
            Write-Host "$tag is the pinned commit $Commit"
        } else {
            Write-Host "+ check that $tag and HEAD are the pinned commit $Commit"
        }
    }

    if ($FetchOnly) {
        Write-Host "$label is checked out in $SrcDir (-FetchOnly: not building)"
        return
    }

    # 2. Use the toolchain Zebra pins for this release.
    $toolchainFile = Join-Path $SrcDir 'rust-toolchain.toml'
    if (Test-Path $toolchainFile) {
        $match = Select-String -Path $toolchainFile -Pattern '^\s*channel\s*=\s*"([^"]+)"' | Select-Object -First 1
        if (-not $match) { throw "no channel in $toolchainFile" }
        $toolchain = $match.Matches[0].Groups[1].Value
        $sourceDateEpoch = Invoke-Git @('log', '-1', '--pretty=%ct') "cannot read the commit time in $SrcDir"
    } elseif ($DryRun) {
        $toolchain = "<channel from $toolchainFile>"
        $sourceDateEpoch = "<commit time of $label>"
    } else {
        throw "$toolchainFile is missing"
    }
    $env:RUSTUP_TOOLCHAIN = $toolchain
    Write-Host "+ `$env:RUSTUP_TOOLCHAIN = '$toolchain'"
    $installed = $false
    if (-not $DryRun) {
        $installed = ((Invoke-Native rustup @('run', $toolchain, 'rustc', '--version')).ExitCode -eq 0)
    }
    if (-not $installed) {
        Invoke-Step rustup @('toolchain', 'install', $toolchain, '--profile', 'minimal')
    }
    Invoke-Step rustup @('target', 'add', '--toolchain', $toolchain, $Target)

    # libclang for bindgen: default to the LLVM that the runner (or the LLVM
    # installer) puts in Program Files. ProgramFiles is unset off Windows, where
    # only -DryRun is useful.
    if (-not $env:LIBCLANG_PATH -and $env:ProgramFiles) {
        $llvmBin = Join-Path $env:ProgramFiles 'LLVM\bin'
        if (Test-Path (Join-Path $llvmBin 'libclang.dll')) { $env:LIBCLANG_PATH = $llvmBin }
    }
    Write-Host "+ `$env:LIBCLANG_PATH = '$env:LIBCLANG_PATH'"
    if (-not $DryRun) {
        if (-not $env:LIBCLANG_PATH) { throw 'libclang.dll not found: install LLVM or set LIBCLANG_PATH' }
        if (-not (Test-Path (Join-Path $env:LIBCLANG_PATH 'libclang.dll'))) {
            throw "libclang.dll not found in LIBCLANG_PATH ($env:LIBCLANG_PATH)"
        }
        if (-not (Get-Command protoc -ErrorAction SilentlyContinue)) { throw 'protoc is not on PATH' }
    }

    # 3. Build like Zebra's release workflow does.
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
    $env:SOURCE_DATE_EPOCH = $sourceDateEpoch
    # The flags are separated by 0x1f, not spaces, so paths with spaces survive.
    $rustFlags = @("--remap-path-prefix=$SrcDir=/zebra", "--remap-path-prefix=$cargoHome=/cargo")
    $env:CARGO_ENCODED_RUSTFLAGS = $rustFlags -join [char]0x1f
    $env:CARGO_PROFILE_RELEASE_STRIP = 'symbols'
    Write-Host "+ `$env:SOURCE_DATE_EPOCH = '$env:SOURCE_DATE_EPOCH'"
    Write-Host "+ `$env:CARGO_ENCODED_RUSTFLAGS = '$($rustFlags -join "' + [char]0x1f + '")'"
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

    # 5. Smoke test, when the host can run the target. A development build has
    #    no expected version: the test only checks that the binary runs and is
    #    zebrad, and records what it reports.
    if ($Ref) {
        if ($DryRun) {
            Write-Host "+ $tmp --version  # must print 'zebrad <anything>' (not compared with ZEBRAD_VERSION)"
            Write-Host "+ Move-Item $tmp $Out"
            Write-Host "dry run: zebrad from $Ref for $Target would be installed at $Out"
            Write-Host $devBuild
            return
        }
        if ($Target -eq $hostTriple) {
            $versionString = ((Invoke-Native $tmp @('--version')).Stdout -split "`n")[0]
            if ($versionString -notmatch '^zebrad \S') {
                Remove-Item -Force $tmp
                throw "binary reports '$versionString', expected 'zebrad <version>'"
            }
        } else {
            Write-Host "Skipping the --version smoke test: $Target does not run on $hostTriple"
            $versionString = "unknown (not run: $Target does not run on $hostTriple)"
        }
        Move-Item -Force $tmp $Out
        Write-Host "zebrad ($Target): built from $Ref ($Commit) with $toolchain, installed at $Out"
        Write-Host "zebrad --version: $versionString"
        Write-Host $banner
        Write-Host $devBuild
        Write-Host $banner
        return
    }
    if ($DryRun) {
        Write-Host "+ $tmp --version  # must print 'zebrad $Version'"
        Write-Host "+ Move-Item $tmp $Out"
        Write-Host "dry run: zebrad $Version for $Target would be installed at $Out"
        return
    }
    if ($Target -eq $hostTriple) {
        $reported = ((Invoke-Native $tmp @('--version')).Stdout -split "`n")[0]
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
    foreach ($name in $buildEnvNames) {
        if ($null -eq $savedEnv[$name]) {
            Remove-Item -Path "Env:$name" -ErrorAction SilentlyContinue
        } else {
            Set-Item -Path "Env:$name" -Value $savedEnv[$name]
        }
    }
    if ($tempRoot -and (Test-Path $tempRoot)) { Remove-Item -Recurse -Force $tempRoot }
}
