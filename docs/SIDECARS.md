# The bundled zebrad channels

Zeal ships three zebrads as Tauri sidecars, so a node on any supported channel
works without a runtime download:

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
