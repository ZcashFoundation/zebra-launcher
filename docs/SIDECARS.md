# The bundled sidecars

Zeal ships three zebrads as Tauri sidecars, so a node on any supported channel
works without a runtime download. From Z1 (proposed, not built yet), Linux
bundles also carry zingod, for the light client server
([The bundled zingod](#the-bundled-zingod-linux)). The zebrads:

| Channel | Sidecar in `src-tauri/binaries/` | Version | Pinned Zebra commit | Release/snapshot date |
| --- | --- | --- | --- | --- |
| Stable | `zeal-zebrad-<target triple>[.exe]` | [`ZEBRAD_VERSION`](../ZEBRAD_VERSION) | [`ZEBRAD_COMMIT`](../ZEBRAD_COMMIT) | [`ZEBRAD_DATE`](../ZEBRAD_DATE): 2026-10-22 |
| Beta | `zeal-zebrad-beta-<target triple>[.exe]` | `7.1.0-alpha.0` | `9889683b9abecd5d28f2de1a49d1a359be11b8eb` | `ZEBRAD_BETA_DATE`: 2026-10-30 |
| Nightly | `zeal-zebrad-nightly-<target triple>[.exe]` | `7.1.0-alpha.0` | `7ae1a827471235e8d14204e2aaa77dc9b61ca25c` | `ZEBRAD_NIGHTLY_DATE`: 2026-11-02 |

The beta values live in `ZEBRAD_BETA_VERSION`, `ZEBRAD_BETA_COMMIT` and
`ZEBRAD_BETA_DATE`; the nightly values use the corresponding
`ZEBRAD_NIGHTLY_*` files. Versions have no leading `v`, commits are full
40-character lowercase SHAs, and dates are ISO `YYYY-MM-DD`. Beta and nightly
can have the same base version while identifying different commits, so the
commit, not the version alone, identifies the binary. Stable remains the
default and recommended channel. Beta is selectable only while its semantic
version is newer than stable's; otherwise it stays visible as unavailable and
disabled without blocking Stable or Nightly. Nightly is the next pinned
development commit. The UI shows the beta and nightly version, short commit,
pin date and a clear warning before either is chosen.

Bundling all three binaries on all five targets makes installers larger. That
is deliberate: channel selection and the accompanying documentation work
offline, with no dependence on a release server or a network connection when a
node is created. The sidecar directory is git-ignored, so binaries always come
from the scripts or CI artifacts below and are never committed. The `zeal-`
prefix keeps the Linux `.deb` from shadowing a system `/usr/bin/zebrad`.

A Git tag can be moved, but a commit SHA cannot, so builds check the source
against the commit and not just the tag. A channel's pin files change together
(see [Bumping the channel pins](#bumping-the-channel-pins)).
[`scripts/check-versions.sh`](../scripts/check-versions.sh), which CI runs,
checks that the pins are well formed and that `fake-zebrad` reports the stable
version. It also checks that the tauri-cli pin (`TAURI_CLI_VERSION` in
[`.github/tool-versions.env`](../.github/tool-versions.env)) has the same minor
version as the `tauri` crate in `Cargo.lock`.

The stable build scripts read `ZEBRAD_VERSION` and `ZEBRAD_COMMIT` by default.
To build another stable version, pass its commit as well (`--commit` or
`-Commit`). The build scripts refuse to run without one, and
`fetch-zebrad-linux.sh` still checks the tag but does not pin an overridden
commit. Beta and nightly always use their exact root pins; workflow inputs
cannot override them.

## Where each platform's zebrad comes from

| Target | Stable and Beta source | Nightly source | Script |
| --- | --- | --- | --- |
| `x86_64-unknown-linux-gnu` | Zebra's verified release tarball | the exact pinned commit | `scripts/fetch-zebrad-linux.sh` (Stable/Beta), `scripts/build-zebrad.sh` (Nightly) |
| `aarch64-unknown-linux-gnu` | Zebra's verified release tarball | the exact pinned commit | `scripts/fetch-zebrad-linux.sh` (Stable/Beta), `scripts/build-zebrad.sh` (Nightly) |
| `aarch64-apple-darwin` | the pinned release tag | the exact pinned commit | `scripts/build-zebrad.sh` |
| `x86_64-apple-darwin` | the pinned release tag | the exact pinned commit | `scripts/build-zebrad.sh` |
| `x86_64-pc-windows-msvc` | the pinned release tag | the exact pinned commit | `scripts/build-zebrad.ps1` |

Zebra publishes release binaries for Linux only (Zebra's ADR 0008), so Stable
and Beta macOS and Windows binaries, and Nightly on every target, are built
from source.

**Linux (Stable and Beta).** `fetch-zebrad-linux.sh` downloads
`zebrad-<ver>-<target>.tar.gz`,
`SHA256SUMS` and `SHA256SUMS.sigstore.json` from the Zebra release and refuses
to install anything unless all of these checks pass:

1. The cosign signature on `SHA256SUMS` is valid and was made by Zebra's
   `zfnd-release-binaries.yml` workflow running for tag `v<ver>` at the pinned
   commit (`--certificate-github-workflow-sha`).
2. The tarball matches its line in the signed `SHA256SUMS`.
3. The tarball's GitHub build-provenance attestation is valid, comes from the
   same workflow, and was built from `refs/tags/v<ver>` (`--source-ref`) at the
   pinned commit (`--source-digest`). This check needs `gh` 2.68.0 or newer,
   the first release with those two flags, and a logged-in `gh` (or
   `GH_TOKEN`). If no `gh` or an older `gh` is on `PATH`, the script says it
   skipped this check and names the minimum version; the cosign and sha256
   checks still apply. CI sets `REQUIRE_ATTESTATION=1`, which turns a skipped
   attestation check into an error before anything is downloaded.
4. The extracted binary prints `zebrad <ver>` for `--version`.

**Source builds.** `build-zebrad.sh` and `build-zebrad.ps1` build Stable and
Beta on macOS and Windows, and Nightly on every target. They clone Zebra or
reuse an existing checkout with `--src-dir`. Stable and Beta check out the
release tag `v<ver>` and require it to resolve to the channel's pinned commit;
Nightly uses the full commit with `--ref` (`-Ref` in PowerShell). Then they
build zebrad the way Zebra's release workflow builds its Linux binaries:

- the toolchain pinned in Zebra's `rust-toolchain.toml`, through `RUSTUP_TOOLCHAIN`
- `cargo build --locked --release -p zebrad --features default-release-binaries`
- `SOURCE_DATE_EPOCH` set from the selected commit's time
- build paths remapped out of the binary
- symbols stripped

Both scripts take `--dry-run` (`-DryRun` in PowerShell), which prints the
commands without running them. The build dependencies are listed at the top of
each script. On macOS that is Xcode command line tools, `brew install protobuf
cmake` and rustup. On Windows it is the VS 2022 C++ build tools, LLVM, protoc,
rustup and PowerShell 7 (`pwsh`; Windows PowerShell 5.1 is not supported).
`build-zebrad.ps1` restores the environment variables it sets for cargo when
it exits, so running it in an open `pwsh` session leaves that session as it
was.

**CI.** The reusable
[`zebrad-sidecars.yml`](../.github/workflows/zebrad-sidecars.yml) workflow has a
`channel` input (`stable`, `beta` or `nightly`, default `stable`) and runs the
scripts on native runners for all five targets. It reads that channel's root
pins, or the `version` and `commit` inputs when they override a stable build,
and passes both values to every script. Every packaged binary is smoke-tested
with `--version` and `generate`, and checked against its channel's expected
version and commit. Every Linux sidecar also passes the glibc-floor check.
Nightly Linux builds run in an Ubuntu 22.04 environment, rather than on a newer
host, so the binary runs in the bundle's Ubuntu 22.04 install smoke container.

The workflow attests provenance with `actions/attest-build-provenance` and
uploads `zebrad-<channel>-<target triple>`. The artifact contains
`zeal-zebrad[-beta|-nightly]-<target triple>[.exe]`, plus build information
where appropriate. The bundle and release workflows invoke the reusable
workflow three times, as `sidecars`, `sidecars-beta` and `sidecars-nightly`
(the stable job keeps its original name). They then invoke the reusable
[`zebrad-docs.yml`](../.github/workflows/zebrad-docs.yml) documentation job
with all three sidecar jobs as prerequisites. You can also run the sidecar
workflow by hand (Actions → zebrad sidecars → Run workflow). Workflow artifacts
do not keep the executable bit, so run `chmod +x` after downloading one. The CI
binaries are **unsigned**. macOS and Windows code signing happens only in the
release environment, when the Tauri bundler signs the app and all its sidecars.

The documentation job generates the Zebra mdBook, default configuration and
changes from each channel's exact pinned commit and uploads an artifact named
`zebra-docs`, containing a `zebra-docs/` directory. Bundle and release jobs
download that artifact into `src-tauri/resources/`, producing:

```text
src-tauri/resources/zebra-docs/<stable|beta|nightly>/
  book/index.html
  config.html
  changes.html
```

These HTML files become Tauri resources. Released documentation therefore
matches the sidecar's source commit and needs no remote network to read. Every
generated book page, Config and Changes page has a per-document CSP permitting
local mdBook assets and search while denying remote origins, frames, objects,
media, workers, forms and base URIs. Small local fallback resources carry the
same policy for development; they are not a substitute for generating and
checking the pinned release resources.

To check that a sidecar artifact came from this workflow:

```sh
gh attestation verify zeal-zebrad-aarch64-apple-darwin \
  --repo ZcashFoundation/zebra-launcher \
  --signer-workflow ZcashFoundation/zebra-launcher/.github/workflows/zebrad-sidecars.yml
```

## Development builds from a Zebra git ref

To try the launcher with an unreleased Zebra, such as `main`, a pull request
or one commit, run the **Bundle** workflow with the `zebrad_ref` input. For
example, a Windows-only build with zebrad from Zebra's `main`:

```sh
gh workflow run bundle.yml -f zebrad_ref=main -f targets=x86_64-pc-windows-msvc --ref <branch>
```

- `--ref <branch>` is the branch of this repository whose workflows and
  launcher code are built.
- `zebrad_ref` is a Zebra branch (`main`), tag (`v7.0.0`), pull request head
  (`refs/pull/<number>/head`) or full 40-character commit SHA. An abbreviated
  SHA cannot be fetched.
- `targets` is a comma-separated list of triples from
  [`.github/targets.json`](../.github/targets.json). Leave it empty for all
  five. Without `zebrad_ref`, it narrows a regular run in the same way.

`gh workflow run` only finds workflows that exist on the repository's default
branch. Until `bundle.yml` is on `main`, the `rebuild` branch carries
[`dev-windows.yml`](../.github/workflows/dev-windows.yml), which calls
`bundle.yml` with `zebrad_ref=main` and `targets=x86_64-pc-windows-msvc`
whenever a line is appended to `.github/dev-build-request` and pushed to
`rebuild`. Delete both files in the pull request that puts `bundle.yml` on
`main`.

The first job resolves `zebrad_ref` to one commit, so every target builds the
same code even if the branch moves during the run. Every selected target then
builds zebrad from source at that commit, Linux included, with
`build-zebrad.sh --ref` or `build-zebrad.ps1 -Ref` and the same flags as a
release build. Custom-ref builds always use the Stable sidecar name and cannot
be combined with a non-Stable channel or version/commit override. Linux
custom-ref and Nightly builds run in an Ubuntu 22.04 environment with clang,
libclang, cmake and protoc installed, so they meet the bundle smoke container's
glibc floor. No Zebra release is downloaded for the custom ref, and no cosign
or Stable-pin source check applies: its `--version` smoke test only checks that
the binary reports `zebrad <something>`, while the build info records the exact
resolved commit.

The run uploads sidecar artifacts named `zebrad-<channel>-<triple>`, containing
the matching `zeal-zebrad[-beta|-nightly]-<triple>[.exe]` and, for a custom
ref, `zebrad-build-info.txt`. Development bundle artifacts are named
`bundles-<triple>-dev-<short commit>` and contain the installers that
`collect-bundles.sh` collects (Windows: `zeal_<ver>_x86_64-setup.exe`), the raw
app and its required channel sidecars, and build information.

`zebrad-build-info.txt` names the channel, ref, resolved commit,
`zebrad --version` output, target and workflow run, under an "UNVERIFIED
DEVELOPMENT BUILD" warning. Each bundle job's summary shows the same, with a
warning that the build is unsigned and unverified. Download an artifact with
`gh run download <run id> -n <artifact>`. On Windows, either run the NSIS
installer, which SmartScreen flags because it is unsigned, or run
`raw/zeal-app.exe` directly, keeping every supplied `zeal-zebrad*.exe` beside
it, because the launcher looks for its sidecars next to itself.

Every packaged Linux sidecar, including a custom-ref or Nightly build, receives
the glibc-floor check. This is what keeps the Nightly and development binaries
compatible with the Ubuntu 22.04 bundle smoke environment rather than assuming
the host runner's newer glibc is acceptable.

**Development builds are never released or signed.** `release.yml` has no
`zebrad_ref` input and always ships the verified Stable, Beta and Nightly pins.
`bundle.yml` uses no secrets and signs nothing. macOS bundles are ad-hoc signed
and Windows binaries are unsigned. The sidecars still get a build-provenance
attestation from `zebrad-sidecars.yml`. Its recorded workflow inputs include
`zebrad_ref`, so it shows where the binary was built, not that Zebra released
it.

You can also run `zebrad-sidecars.yml` alone with `channel` and the ref inputs,
or build locally with `scripts/build-zebrad.sh --ref main` (or
`scripts/build-zebrad.ps1 -Ref main`). Add `--dry-run` (`-DryRun`) to print the
commands without running them.

## Local development without zebrad

`scripts/dev-sidecar.sh` (or `scripts/dev-sidecar.ps1` on Windows) builds the
workspace's `fake-zebrad` test binary and installs it as each of the host's
three channel sidecars. After that, `cargo tauri dev` and `cargo tauri build`
work without real nodes. Never ship a bundle built this way. Replace the fakes
with all three real channel sidecars first; the stable host build starts with:

```sh
scripts/fetch-zebrad-linux.sh             # Linux host, Stable
scripts/build-zebrad.sh                   # macOS host, Stable (takes a while)
```

Build Beta and Nightly from their exact channel pins as described above.

## Verifying a Zebra tarball by hand

You need [cosign](https://docs.sigstore.dev/cosign/system_config/installation/)
and, for step 3, `gh` 2.68.0 or newer.

```sh
VER=7.0.0
COMMIT=e8db87eb69254e0d4eedd058eadd48bdfa09065a   # ZEBRAD_COMMIT
TARGET=x86_64-unknown-linux-gnu
BASE=https://github.com/ZcashFoundation/zebra/releases/download/v$VER
curl -fLO "$BASE/zebrad-$VER-$TARGET.tar.gz"
curl -fLO "$BASE/SHA256SUMS"
curl -fLO "$BASE/SHA256SUMS.sigstore.json"

# 1. The checksum list was signed by Zebra's release workflow for this tag,
#    running at the pinned commit.
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity "https://github.com/ZcashFoundation/zebra/.github/workflows/zfnd-release-binaries.yml@refs/tags/v$VER" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-github-workflow-sha "$COMMIT"
# Expect: Verified OK

# 2. The tarball matches the signed list.
sha256sum -c --ignore-missing SHA256SUMS
# Expect: zebrad-<ver>-<target>.tar.gz: OK

# 3. GitHub's provenance attestation for the tarball.
gh attestation verify "zebrad-$VER-$TARGET.tar.gz" \
  --repo ZcashFoundation/zebra \
  --signer-workflow ZcashFoundation/zebra/.github/workflows/zfnd-release-binaries.yml \
  --source-ref "refs/tags/v$VER" \
  --source-digest "$COMMIT"

# 4. The binary is the expected version.
tar -xzf "zebrad-$VER-$TARGET.tar.gz" zebrad
./zebrad --version
# Expect: zebrad 7.0.0+git.<commit>
```

If you already downloaded the three files, the script runs the same checks on
them: `scripts/fetch-zebrad-linux.sh --from-dir <dir> --target <triple>`.
Changing any byte of `SHA256SUMS` makes step 1 fail. Changing any byte of the
tarball makes step 2 fail.

## Bumping the channel pins

Keep each channel's files atomic: a version, the full commit that identifies
it, and its release or snapshot date. The bump scripts run the version checks
after writing pins:

```sh
scripts/bump-zebrad.sh <ver> <commit>     # Stable: explicit release and full commit
scripts/bump-zebrad-beta.sh               # Beta: discover the newest published prerelease
scripts/bump-zebrad-nightly.sh            # Nightly: discover the current development tip
```

For Stable, update `ZEBRAD_DATE` to the release date in the same change; the
Stable script writes `ZEBRAD_VERSION` and `ZEBRAD_COMMIT`. The Beta and Nightly
scripts need `gh`, take no version arguments, resolve a full commit and its
committer date, and write all three channel files. Beta selects the newest
published prerelease and refuses to replace the pin unless it is semver-newer
than Stable. Nightly reads zebrad's package version at the exact resolved
development commit. None of these scripts commits or pushes.

A Stable bump can make the existing Beta equal to or older than Stable; Beta
then remains visible as unavailable and disabled until a newer prerelease is
available. This is not a fatal state, and Stable and Nightly remain selectable.
Nightly remains a separate, explicitly warned development pin even when its
base version equals Beta's.

After any channel bump:

1. Run `scripts/check-versions.sh` and inspect the complete pin diff, including
   the date. Confirm that Beta's availability rule has the expected result.
2. Run that channel's **zebrad sidecars** workflow on all five targets. Every
   binary must pass the version/commit check, and every Linux binary must pass
   the glibc check.
3. Run **Bundle** so all three channels and the generated `zebra-docs` resources
   are tested together. Review the mdBook, default config and changes from the
   new exact commit; no released page should require an online source.
4. Recheck configuration, CLI and log compatibility. For Stable, also follow
   the fixture and release checklist below. For Beta or Nightly, verify the
   picker metadata and warning, the immutable channel on existing nodes, and
   the browser mock's available and unavailable states.

The existing `zebrad-watch.yml` automation below watches Stable only. The new
Beta and Nightly bump scripts are manual maintenance tools; this design does
not add a bot to move those pins.

## Bumping ZEBRAD_VERSION

[`zebrad-watch.yml`](../.github/workflows/zebrad-watch.yml) automates the
Stable version and commit update. Every day at 05:17 UTC, and when run by
hand, it finds Zebra's highest `vX.Y.Z` release that has
`SHA256SUMS.sigstore.json` (release candidates and releases without signed
binaries are skipped; the `releases/latest` endpoint and the prerelease flag
are not used, because Zebra published `v7.0.0-rc.0` as a non-prerelease). If
that release is newer than `ZEBRAD_VERSION`, it resolves the tag to its commit,
runs [`scripts/bump-zebrad.sh`](../scripts/bump-zebrad.sh) `<ver> <commit>` and
opens a pull request titled `build: bump zebrad to <ver>` from branch
`bump/zebrad-<ver>`, with this checklist in its body. Later runs update the
pull request's body but never rewrite the branch, so you can push fixes to
it. If the pull request was closed without merging, the watcher only warns.
The existing Stable script does not infer `ZEBRAD_DATE`: set that file to the
release date in the watcher's pull request before merging it.

The pull request is pushed with a GitHub App token, because pull requests
opened with `GITHUB_TOKEN` do not trigger CI. It needs two repository
secrets for an App installed on this repository with **Contents** and **Pull
requests** read/write (ZF already runs such an App for the zebra repo):

| Secret | Value |
| --- | --- |
| `ZEAL_APP_ID` | the App's ID |
| `ZEAL_APP_PRIVATE_KEY` | a private key of the App, in PEM form |

Without them, the watcher opens (or updates) an issue titled
`Bump zebrad to <ver>`, labelled `release-blocker`, instead. To try the
watcher without secrets, run it from the Actions tab with `dry_run` checked:
it prints the diff it would commit. Set `version` to force a release, for
example an older one to get a non-empty diff.

To bump by hand, or to finish the watcher's pull request:

1. Wait for the Zebra release to be published with its Linux assets: the two
   tarballs, `SHA256SUMS` and `SHA256SUMS.sigstore.json`.
2. Update `ZEBRAD_VERSION`, `ZEBRAD_COMMIT` and `ZEBRAD_DATE` together, in the
   same commit. Never change the version without its commit, and keep the
   release date in ISO `YYYY-MM-DD` form.
   - Write the new version, without `v`, to `ZEBRAD_VERSION`.
   - Write the commit that the new tag points to, as the full 40-character
     lowercase SHA, to `ZEBRAD_COMMIT`. Zebra's tags are annotated, so the
     tag object's SHA is not the commit's SHA. Resolve the tag to its commit
     with either of these:

     ```sh
     VER=7.0.0
     git ls-remote https://github.com/ZcashFoundation/zebra "refs/tags/v$VER^{}"
     # or, in a Zebra checkout that has the tag:
     git rev-parse "v$VER^{commit}"
     ```

     Check the result against the commit shown on the GitHub release page.
3. Run `scripts/check-versions.sh`. The fake zebrad reads `ZEBRAD_VERSION`
   when it is built, so nothing else carries the version.
   `scripts/bump-zebrad.sh <ver> <commit>` does step 2 and runs the check.
4. Check the new Linux release locally:
   `scripts/fetch-zebrad-linux.sh --out /tmp/zebrad-check/zebrad`. The cosign
   and attestation checks fail if `ZEBRAD_COMMIT` is not the commit that
   Zebra's release workflow ran at.
5. Run the **zebrad sidecars** workflow from your branch with an empty
   `version` input. All five jobs must pass. The macOS and Windows jobs build
   with the new tag's toolchain, and the first run after a bump takes 30–60
   minutes without a cache.
6. Read Zebra's changelog for config or CLI changes the launcher depends on,
   such as the config schema or log lines that the status parser reads.
7. Re-capture `crates/launcher-core/tests/fixtures/zebrad-<ver>-generate.toml`
   with the new zebrad (`zebrad generate -o <file>`, see the fixtures README)
   and update the configuration form's schema in
   `ui/src/config_form/schema.rs`: keys, defaults, kinds and explanations.
   `cargo test -p ui` fails until the schema and the fixture agree, which is
   what catches a key zebrad added, removed or re-defaulted. Then open the PR.

## The bundled zingod (Linux)

> **Proposal, 2026-11-16: Z1 in
> [PUBLIC-SERVICES.md](PUBLIC-SERVICES.md#phases-for-zingod), not built yet.**
> Facts checked at zingod tag `v1.1.0`; paths under `packages/` are zingod's.
> Build times and sizes come from research builds on 2026-11-16.

Zeal also bundles zingod, Zingo Labs' light client indexer, as `zeal-zingod`,
on Linux only. Only the light client server runs it
([PUBLIC-SERVICES.md](PUBLIC-SERVICES.md#zingod-as-the-backend)).

| Sidecar in `src-tauri/binaries/` | Version | Pinned zingod commit | Release date |
| --- | --- | --- | --- |
| `zeal-zingod-<target triple>` | `ZINGOD_VERSION`: `1.1.0` | `ZINGOD_COMMIT`: `bfce8b2b433578054dec82928d2707e523ec75b7` | `ZINGOD_DATE`: 2026-11-10 |

- **The pins** follow the zebrad rules: no leading `v`, a full 40-character
  lowercase commit, an ISO date, and the three files change together.
  `scripts/check-versions.sh` checks their grammar; it uses only bash, grep and
  sed and runs before anything compiles. `fake-zingod` reads `ZINGOD_VERSION`
  with `include_str!` when it is built, as fake-zebrad reads `ZEBRAD_VERSION`,
  and a cargo test checks that its `--version` reports it. A launcher-herd test
  checks that the version is on `TESTED_ZINGOD`.
- **The name.** The `zeal-` prefix keeps the `.deb`'s `/usr/bin/zeal-zingod`
  from shadowing a system `zingod`.
- **The date** is the tag commit's date in UTC; zingod's CHANGELOG heads the
  release 2026-11-09.
- **The tag** `v1.1.0` is lightweight, so `git ls-remote
  https://github.com/zingolabs/zingod refs/tags/v1.1.0` prints the commit
  itself. The build also takes a peeled `^{}` line, in case a later tag is
  annotated.

**Why Zeal builds it.** zingod publishes no binaries: the v1.1.0 release has no
assets, and the `zingod` package is `publish = false` (`Cargo.toml:27-37`). Its
only artifact is the linux/amd64 image `zingodevops/zingod`, which is unsigned,
has no attestation and has the licence label `NOASSERTION`; its revision label
names `0096c5ba`, the parent of the tagged commit, because the release job
commits on the runner. There is nothing to verify the way
`fetch-zebrad-linux.sh` verifies Zebra's tarballs, so Zeal builds every zingod
it ships from the pinned commit, as it builds Nightly zebrad, and never uses
the image. Zeal's attestation says where a binary was built, not that Zingo
Labs released it.

### Targets for zingod

| Target | zingod | Why |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | built from the pinned commit in `ubuntu:22.04` | built and checked in research builds |
| `aarch64-unknown-linux-gnu` | the same, on `ubuntu-24.04-arm` | expected to build, as zebrad's Nightly does there; not built yet |
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | not bundled (Z5) | every Linux-only call has a fallback, but upstream has never built macOS, and it wasn't compiled here |
| `x86_64-pc-windows-msvc` | not bundled | doesn't compile at 1.1.0: `std::os::unix::fs::FileExt` (`packages/zingod-persistence/src/fs/real.rs:7`), `tokio::signal::unix` (`packages/zingod/src/daemon.rs:530`), `MetadataExt` and `statvfs` (`packages/zingod/src/daemon/index_snapshot.rs:13, 224`) and sd-notify 0.5 are used with no `cfg` gate, and jemalloc is untested on MSVC |

jemalloc 5.3.1's configure defaults to 64 KiB pages on aarch64 Linux
(tikv-jemalloc-sys 0.7.1, `jemalloc/configure.ac:1975-1992`), so a binary built
on a runner with 4 KiB pages also runs on kernels with 16 KiB pages, such as
the Raspberry Pi 5's. This is unchecked on hardware.

### How zingod is built

`scripts/build-zingod.sh` works like `build-zebrad.sh`, with `--commit`,
`--src-dir`, `--target`, `--out`, `--vendor-archive` and `--dry-run`:

1. Resolve `refs/tags/v<ZINGOD_VERSION>` with `git ls-remote` (the peeled line
   if there is one, else the tag's own) and refuse to go on unless it equals
   `ZINGOD_COMMIT`. Clone, or reuse `--src-dir`, check out the commit and check
   `git rev-parse HEAD`.
2. Take the toolchain from the checkout's `rust-toolchain.toml` (1.96.0),
   through `RUSTUP_TOOLCHAIN`.
3. Run `cargo fetch --locked` first, so a git source that has gone fails early
   and is named.
4. Run `cargo build --locked --release -p zingod --bin zingod`. There is
   nothing to choose: the package has no `[features]`, and the workspace's are
   test-only. The build sets:
   - `SOURCE_DATE_EPOCH` from the commit's time;
   - `CARGO_PROFILE_RELEASE_STRIP=symbols`, since the workspace has no
     `[profile.release]`: 32.0 MB becomes 24.7 MB;
   - `--remap-path-prefix` for the source, `CARGO_HOME`, `RUSTUP_HOME` and
     `CARGO_TARGET_DIR`. The last one matters: zingod-proto `include!`s
     generated code from `OUT_DIR`, and a build that remapped only the other
     three still held one host path;
   - `AWS_LC_SYS_USE_SYSTEM=0`, so aws-lc-sys never links an AWS-LC it finds on
     the host.
5. Check the result, and install nothing otherwise:
   - `--version` prints `zingod <ZINGOD_VERSION>`;
   - `generate-config -o <tmp>` exits 0, offline;
   - `scripts/check-glibc.sh --floor 2.35` passes;
   - `ldd` names only libc, libm, libgcc_s, the loader and the vdso;
   - no string in the binary contains `/home/`, `/root/`, `/runner/`,
     `/github/` or `/__w/`;
   - the committed licence files equal the ones in the checkout and its
     sources, and the two that aren't committed, jemalloc's `COPYING` and
     AWS-LC's `LICENSE`, are copied from the build's own sources into the
     output ([below](#licences-of-zingod)).

   The build never runs `start`, which would contact a validator.
6. With `--vendor-archive <file>`, run `cargo vendor --locked` into a copy of
   the checkout and pack it as `zingod-src-<ver>.tar.zst`, with its sha256:
   what a rebuild needs if a fork disappears.

The build needs:

- a C compiler, plus `sh` and `make` for jemalloc's configure: AWS-LC 5.12.0
  (through aws-lc-sys 0.47.0), jemalloc 5.3.1 (tikv-jemalloc-sys 0.7.1), zstd
  1.5.7 (zstd-sys 2.1.0) and libsecp256k1 (secp256k1-sys 0.14.1) are vendored
  and linked statically;
- `protoc` with the well-known types: Ubuntu 22.04's `protobuf-compiler` 3.12.4
  and `libprotobuf-dev` built it;
- git and rustup, and `zstd` to pack the source archive.

On Linux it needs no cmake, libclang (bindgen comes only through a macOS-only
dependency), OpenSSL or RocksDB.

- **The glibc floor.** Built inside `ubuntu:22.04`, the highest glibc symbol
  version is `GLIBC_2.34`, and the floor passes. Built on an Ubuntu 24.04 host,
  the binary needs `GLIBC_2.39` (Rust std's weak `pidfd_*` symbols, and
  `__isoc23_*` from the C objects compiled against the newer headers) and fails
  it. So zingod builds in the same container as zebrad's Nightly.
- **Static musl**, which upstream's image ships, is the other route. It wasn't
  tried here; the glibc build passes the floor, so Zeal uses that.
- **Cost.** A clean x86-64 build took 44 s on 32 threads (618 s of CPU, 1.9 GB
  peak memory), and 61 s in the 22.04 container. On a 4-core runner it should
  take a few minutes (unmeasured), against 30 to 60 minutes for zebrad on macOS
  and Windows.
- **Git sources.** The build pulls five repositories, all at pinned revisions
  on one person's forks: `elicbarbieri/zebra` (`ebabeee5`: zebra-chain,
  zebra-network and zebra-test), and patched librustzcash (`605917a3`), orchard
  (`426f5c77`), sapling-crypto (`3c14da61`) and incrementalmerkletree
  (`d5e07689`) (`Cargo.toml:116-119, 157-170`). `--locked` pins them, the
  source archive on each Zeal release keeps a shipped version rebuildable, and
  whether to mirror them as well is PUBLIC-SERVICES.md's open question 22.

### zingod in CI

The reusable [`zingod-sidecars.yml`](../.github/workflows/zingod-sidecars.yml)
workflow takes a `targets` input:

- its first job builds the matrix with `scripts/select-targets.sh --family
  linux`, and an empty matrix skips the build;
- each target builds in `ubuntu:22.04` on `ubuntu-24.04` or `ubuntu-24.04-arm`,
  and caches cargo's registry and git checkouts by commit and target. The bare
  image has no curl, no CA certificates and no `zstd` command, so the job
  installs `ca-certificates curl git zstd binutils build-essential
  protobuf-compiler libprotobuf-dev` with `--no-install-recommends`, then
  rustup through `zebrad-sidecars.yml`'s pinned, checksummed `rustup-init`
  step;
- it runs the checks above. Once, on the x86-64 leg, it runs `cargo deny check
  advisories` over zingod's `Cargo.lock`, with cargo-deny's release binary
  pinned by version and sha256 in `.github/tool-versions.env`
  (`CARGO_DENY_VERSION`), as shellcheck and actionlint are; Zeal's own `deny`
  job uses a Docker action instead. The findings go in the job's summary and
  don't fail it (PUBLIC-SERVICES.md's open question 24);
- it attests provenance with `actions/attest-build-provenance`, for the binary
  and, on x86-64, the source archive;
- it uploads `zingod-<target triple>`, holding `zeal-zingod-<target triple>`,
  `licenses/` with the two copied licence files, and `zingod-build-info.txt`
  (the version, commit, toolchain, protoc version, container, target,
  `SOURCE_DATE_EPOCH`, the binary's and the licence files' sha256, and the
  run), and `zingod-src-<ver>`.

`bundle.yml` and `release.yml` call it once each, as `sidecars-zingod`. Every
Linux job that compiles the `zeal` crate needs it, since tauri-build copies
every `externalBin` at compile time and fails on a missing one: `bundle.yml`'s
Linux `bundle` legs, and both of `release.yml`'s Linux jobs, the unprivileged
`build` (`cargo tauri build --no-bundle`, `.github/workflows/release.yml:346`)
and `bundle_sign` (`:483`). Each adds `sidecars-zingod` to its `needs`,
downloads the artifact, runs `chmod +x` (artifacts lose the executable bit),
checks `--version` against `ZINGOD_VERSION`, adds it to its glibc check (both
release jobs have one) and installs it as
`src-tauri/binaries/zeal-zingod-<target triple>`. Each also installs the
artifact's two licence files into `src-tauri/resources/licenses/zingod/` and
checks them against the sha256 in `zingod-build-info.txt`. The macOS and
Windows jobs don't download it.

`bundle.yml` compiles and bundles in one job, so its runs on `main` never
exercise `release.yml`'s split; the first release candidate that carries
zingod (`vX.Y.Z-rc.N`, RELEASING.md) does. `release.yml`'s `draft` job also
needs `sidecars-zingod`, and uploads `zingod-src-<ver>.tar.zst` and its sha256
to the draft release. `manifest` already needs `draft` and hashes every draft
asset, so `SHA256SUMS` and the release's attestations cover the archive.

- `ci.yml` runs the workflow on pull requests that change the `ZINGOD_*` pins,
  `build-zingod.sh` or the workflow. A job that calls a reusable workflow can't
  have steps, so a `zingod-gate` job diffs the pull request against its base,
  as `real-zebrad`'s gate step does, and outputs `run`. The `zingod-sidecars`
  job has `needs: zingod-gate` and `if: needs.zingod-gate.outputs.run ==
  'true'`. `ci.yml` grants only `contents: read`, and the called jobs declare
  `id-token: write` and `attestations: write`, so the calling job grants all
  three itself.
- The workflow has an `attest` input, true by default, that both attestation
  steps follow. `ci.yml` passes false: a fork's pull request gets no OIDC
  token, and a binary built from an unmerged branch never carries an
  attestation. `bundle.yml` and `release.yml` always attest.
- `collect-bundles.sh --raw` includes the binary.
- `smoke-linux.sh` runs `zeal-zingod --version` with the other sidecars, and
  counts a live `zeal-zingod` as left over.
- The CI binaries are unsigned, like the zebrad sidecars; a release's
  `SHA256SUMS` and its cosign bundle cover the Linux bundles.

To check that an artifact came from this workflow, run for a release's tag
(for a `bundle.yml` build, `--source-ref refs/heads/main`):

```sh
gh attestation verify zeal-zingod-x86_64-unknown-linux-gnu \
  --repo ZcashFoundation/zebra-launcher \
  --signer-workflow ZcashFoundation/zebra-launcher/.github/workflows/zingod-sidecars.yml \
  --source-ref "refs/tags/v$ZEAL_VERSION"
```

### Bundling zingod per platform

Tauri merges `tauri.linux.conf.json` into `tauri.conf.json` with JSON Merge
Patch (RFC 7396), which replaces arrays and merges objects (tauri-utils 2.10.2
`src/config/parse.rs`, json-patch 4.2.0). So the Linux file lists every Linux
sidecar again, and adds the licences to `resources`, an object, where they
merge in beside `zebra-docs`:

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "zeal-app",
  "bundle": {
    "externalBin": [
      "binaries/zeal-zebrad",
      "binaries/zeal-zebrad-beta",
      "binaries/zeal-zebrad-nightly",
      "binaries/zeal-zingod"
    ],
    "resources": {
      "resources/licenses/zingod/": "licenses/zingod/"
    }
  }
}
```

- **macOS and Windows** keep `tauri.conf.json`'s three entries and need no
  zingod file, so `dev-sidecar.ps1`, `check-windows.sh` and their bundle and
  test jobs don't change.
- **Every Linux build** of the `zeal` crate needs
  `src-tauri/binaries/zeal-zingod-<triple>`, since tauri-build copies every
  `externalBin` and fails on a missing one, and the committed licence folder.
  So on Linux `scripts/dev-sidecar.sh` also builds `fake-zingod`, a
  launcher-core bin beside `fake-zebrad`, and installs it as
  `zeal-zingod-<host>`; `ci.yml`'s Linux job already runs that script before
  clippy.
- **At runtime**, `sidecar::resolve(paths, Sidecar::Zingod)` finds
  `zeal-zingod` beside the launcher. Inside an AppImage it is first copied to
  `<data>/sidecar/zingod/<launcher version>/`, and the cleanup of other
  versions stays inside that folder.

### Licences of zingod

zingod is Apache-2.0, and Zeal is MIT. zingod's `LICENSE` is only the 13-line
Apache notice ("Copyright 2024 ZingoLabs"), and its tree has no `NOTICE` file.
Three `.proto` files are compiled in (`packages/zingod-proto/build.rs`):

- lightwallet-protocol's `compact_formats.proto`, vendored, MIT ("Copyright (c)
  2019 Electric Coin Company");
- zingod-proto's `service.proto`, a modified copy of lightwallet-protocol's,
  under the Zcash developers' MIT header
  (`packages/zingod-proto/proto/service.proto:1-3`);
- `zebra_indexer.proto`, taken from Zebra's `zebra-rpc/proto/indexer.proto`
  through the `elicbarbieri/zebra` fork, MIT OR Apache-2.0
  (`packages/zingod-proto/proto/zebra_indexer.proto:1-2`).

Four C libraries are linked statically:

| Library | Through | Licence | Its file |
| --- | --- | --- | --- |
| AWS-LC 5.12.0 (`AWSLC_VERSION_NUMBER_STRING`, `aws-lc/include/openssl/base.h:68`) | aws-lc-sys 0.47.0 | the crate's field: `ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND …` | `aws-lc/LICENSE`, copied at build time |
| jemalloc 5.3.1 | tikv-jemalloc-sys 0.7.1 | BSD-2-Clause | `jemalloc/COPYING`, copied at build time |
| zstd 1.5.7 | zstd-sys 2.1.0 | BSD-3-Clause (zstd is BSD-3-Clause OR GPL-2.0; Zeal ships the BSD text) | `zstd/LICENSE`, committed |
| libsecp256k1 | secp256k1-sys 0.14.1 | MIT. The crate's own `LICENSE` is CC0-1.0 and covers only its Rust wrapper | `depend/secp256k1/COPYING`, committed |

`src-tauri/resources/licenses/zingod/` is committed and refreshed by
`bump-zingod.sh`. It holds:

- zingod's notice, with the full Apache-2.0 text;
- lightwallet-protocol's `LICENSE`;
- Zebra's `LICENSE-MIT`, from the `elicbarbieri/zebra` revision the build pins
  (`ebabeee5`), for `zebra_indexer.proto` and the zebra crates linked in;
- zstd's `LICENSE` and libsecp256k1's `COPYING`;
- a `README.txt` that Zeal writes: zingod's name, version and copyright line,
  the source URL and commit, where the release's source archive is, and where
  each `.proto` file comes from.

jemalloc's `COPYING` and AWS-LC's `LICENSE` aren't committed: their copyright
lines carry personal contact addresses, which Zeal keeps out of its
repository. `build-zingod.sh` copies both from the build's own sources into its
output, and every Linux job that compiles Zeal installs them into the folder,
where `.gitignore` names them, and checks their sha256. `smoke-linux.sh` checks
that an installed bundle holds every file. A local build has only the committed
files, which is fine for a build that is never shipped. `bump-zingod.sh` refuses
to commit a licence file that holds an email address, and names it, so a file
that gains one moves to the copied list.

Zeal's bundles carry no third-party licences for zebrad today, nor a list of
the Rust crates linked into either binary; that is a Zeal-wide follow-up
(PUBLIC-SERVICES.md's open question 25). Zeal's `deny.toml` doesn't cover
zingod, since nothing of it links into Zeal; the advisory report and the bump
review stand in for it.

### Local development with zingod

On a Linux host `scripts/dev-sidecar.sh` installs `fake-zingod` as
`zeal-zingod-<host>`, beside the three fake zebrads; on macOS and Windows it
installs nothing for zingod. For the real one, run `scripts/build-zingod.sh
--target x86_64-unknown-linux-gnu --out
src-tauri/binaries/zeal-zingod-x86_64-unknown-linux-gnu` inside `ubuntu:22.04`,
or download a CI build with `gh run download <run id> -n
zingod-x86_64-unknown-linux-gnu`. A build on a newer host fails the glibc
check: it is fine for trying zingod out, and never shipped. Treat the checkout
as downloaded code: build it only through cargo, with `CARGO_TARGET_DIR`
outside the checkout.

### Bumping ZINGOD_VERSION

`scripts/bump-zingod.sh <ver> <commit>` writes `ZINGOD_VERSION` and
`ZINGOD_COMMIT`, refreshes the licence folder from a checkout at the commit,
and runs the version checks; set `ZINGOD_DATE` in the same commit. No watcher
opens these pull requests yet: upstream's release cadence is unknown. Then:

1. Resolve the tag with `git ls-remote https://github.com/zingolabs/zingod
   "refs/tags/v$VER" "refs/tags/v$VER^{}"`. A `^{}` line, if there is one, is
   the commit of an annotated tag. Check the result against the release page.
2. Read zingod's CHANGELOG and diff for:
   - configuration keys: every table refuses unknown keys, so a removed or
     renamed key fails the start;
   - the CLI, the log lines Zeal reads (`Listening`, `Starting`, `Startup
     failed`, `Shutdown complete`, `State changed`), the exit codes and the
     error texts;
   - the shape of `/status`;
   - the index format;
   - the network upgrades the build knows;
   - `rust-toolchain.toml`, `[features]` and `Cargo.lock`'s git sources.
3. Run the **zingod sidecars** workflow on both Linux targets: every check must
   pass. Read the advisory report.
4. Capture the fixtures again with `real_zingod.rs`'s capture mode:
   `zingod-<ver>-generate.toml`, the log lines, `/status` for each state and
   the `LightdInfo` bytes. `cargo test` fails until the writer and the reader
   agree with them. `generate-config` leaves out keys that default to unset, so
   also check the writer's list of such keys (only `cookie_path` at 1.1.0)
   against `packages/zingod/src/config.rs`.
5. Update `TESTED_ZINGOD` and `ZINGOD_KNOWS`.
6. Run `real_zingod.rs` against the Stable zebrad pin (CI does this on the pull
   request), with `PREV_ZINGOD_BIN` set to the previous pin's build, so an
   index the old one wrote must open with the new one. Then run **Bundle**.
7. Before a release lists the new version as tested on a network, do the wallet
   runs there; a first Mainnet listing waits for PUBLIC-SERVICES.md's Mainnet
   gate.
