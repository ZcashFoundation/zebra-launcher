# The bundled zebrad

Zebra Launcher ships zebrad as a Tauri sidecar. Tauri looks for it at
`src-tauri/binaries/zebra-launcher-zebrad-<target triple>` (with `.exe` on
Windows). That directory is git-ignored, so the binary always comes from one of
the scripts below and is never committed. The `zebra-launcher-` prefix keeps the
Linux `.deb` from shadowing a system `/usr/bin/zebrad`.

The zebrad version is pinned in [`ZEBRAD_VERSION`](../ZEBRAD_VERSION), written
without a leading `v` (for example `7.0.0`). The Zebra commit that tag
`v<ver>` points to is pinned next to it in [`ZEBRAD_COMMIT`](../ZEBRAD_COMMIT),
as the full 40-character lowercase SHA. A Git tag can be moved, but a commit SHA
cannot, so every script checks the release against the commit and not just the
tag. The two files describe one release and change together (see
[Bumping ZEBRAD_VERSION](#bumping-zebrad_version)).

The scripts read both files by default. `ZEBRAD_COMMIT` applies only to the
version in `ZEBRAD_VERSION`. To build any other version, pass its commit as well
(`--commit` or `-Commit`). The build scripts refuse to run without one, and
`fetch-zebrad-linux.sh` still checks the tag but does not pin the commit.

## Where each platform's zebrad comes from

| Target | Source | Script |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | Zebra's release tarball | `scripts/fetch-zebrad-linux.sh` |
| `aarch64-unknown-linux-gnu` | Zebra's release tarball | `scripts/fetch-zebrad-linux.sh` |
| `aarch64-apple-darwin` | built from the Zebra tag | `scripts/build-zebrad.sh` |
| `x86_64-apple-darwin` | built from the Zebra tag | `scripts/build-zebrad.sh` |
| `x86_64-pc-windows-msvc` | built from the Zebra tag | `scripts/build-zebrad.ps1` |

Zebra publishes binaries for Linux only (Zebra's ADR 0008), so macOS and
Windows binaries are built from source.

**Linux.** `fetch-zebrad-linux.sh` downloads `zebrad-<ver>-<target>.tar.gz`,
`SHA256SUMS` and `SHA256SUMS.sigstore.json` from the Zebra release and refuses
to install anything unless all of these checks pass:

1. The cosign signature on `SHA256SUMS` is valid and was made by Zebra's
   `zfnd-release-binaries.yml` workflow running for tag `v<ver>` at the pinned
   commit (`--certificate-github-workflow-sha`).
2. The tarball matches its line in the signed `SHA256SUMS`.
3. The tarball's GitHub build-provenance attestation is valid, comes from the
   same workflow, and was built from `refs/tags/v<ver>` (`--source-ref`) at the
   pinned commit (`--source-digest`). This check needs
   `gh` 2.49 or newer and a logged-in `gh` (or `GH_TOKEN`). With an older `gh`,
   or none, the script says it skipped this check. If `gh` is new enough, the
   check is required.
4. The extracted binary prints `zebrad <ver>` for `--version`.

**macOS and Windows.** `build-zebrad.sh` and `build-zebrad.ps1` clone Zebra at
tag `v<ver>`, or reuse an existing checkout with `--src-dir`. They refuse to
build unless HEAD is the tag and the tag resolves to exactly the pinned commit.
Then they build zebrad the way Zebra's release workflow builds its Linux
binaries:

- the toolchain pinned in Zebra's `rust-toolchain.toml`, through `RUSTUP_TOOLCHAIN`
- `cargo build --locked --release -p zebrad --features default-release-binaries`
- `SOURCE_DATE_EPOCH` set from the tag's commit time
- build paths remapped out of the binary
- symbols stripped

Both scripts take `--dry-run` (`-DryRun` in PowerShell), which prints the
commands without running them. The build dependencies are listed at the top of
each script. On macOS that is Xcode command line tools, `brew install protobuf
cmake` and rustup. On Windows it is the VS 2022 C++ build tools, LLVM, protoc
and rustup.

**CI.** [`zebrad-sidecars.yml`](../.github/workflows/zebrad-sidecars.yml) runs
the scripts on native runners for all five targets. Its first job reads
`ZEBRAD_VERSION` and `ZEBRAD_COMMIT`, or the `version` and `commit` inputs, and
passes both to every script. It smoke-tests each binary
(`--version` and `generate`), attests its provenance with
`actions/attest-build-provenance`, and uploads it as the workflow artifact
`zebrad-<target triple>`. You can run it by hand (Actions → zebrad sidecars →
Run workflow), and the bundle and release workflows call it. Workflow
artifacts do not keep the executable bit, so run `chmod +x` after downloading
one. The CI binaries are **unsigned**. macOS and Windows code signing happens
only in the release environment, when the Tauri bundler signs the app and its
sidecars.

To check that a sidecar artifact came from this workflow:

```sh
gh attestation verify zebra-launcher-zebrad-aarch64-apple-darwin \
  --repo ZcashFoundation/zebra-launcher \
  --signer-workflow ZcashFoundation/zebra-launcher/.github/workflows/zebrad-sidecars.yml
```

## Local development without zebrad

`scripts/dev-sidecar.sh` (or `scripts/dev-sidecar.ps1` on Windows) builds the
workspace's `fake-zebrad` test binary and installs it as the host's sidecar.
After that, `cargo tauri dev` and `cargo tauri build` work without the real
node. Never ship a bundle built this way. Replace the fake with the real
zebrad first:

```sh
scripts/fetch-zebrad-linux.sh             # Linux host
scripts/build-zebrad.sh                   # macOS host (takes a while)
```

## Verifying a Zebra tarball by hand

You need [cosign](https://docs.sigstore.dev/cosign/system_config/installation/)
and, for step 3, `gh` 2.49 or newer.

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

## Bumping ZEBRAD_VERSION

1. Wait for the Zebra release to be published with its Linux assets: the two
   tarballs, `SHA256SUMS` and `SHA256SUMS.sigstore.json`.
2. Update `ZEBRAD_VERSION` and `ZEBRAD_COMMIT` together, in the same commit.
   Never change one without the other.
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
3. Update `VERSION` in `crates/launcher-core/src/bin/fake-zebrad.rs` so the
   fake reports the same version.
4. Check the new Linux release locally:
   `scripts/fetch-zebrad-linux.sh --out /tmp/zebrad-check/zebrad`. The cosign
   and attestation checks fail if `ZEBRAD_COMMIT` is not the commit that
   Zebra's release workflow ran at.
5. Run the **zebrad sidecars** workflow from your branch with an empty
   `version` input. All five jobs must pass. The macOS and Windows jobs build
   with the new tag's toolchain, and the first run after a bump takes 30–60
   minutes without a cache.
6. Read Zebra's changelog for config or CLI changes the launcher depends on,
   such as the config schema or log lines that the status parser reads. Then
   open the PR.
