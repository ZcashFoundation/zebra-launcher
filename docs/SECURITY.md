# Security

> Partly a skeleton: sections marked TODO are filled in during M10 (signing, key
> ceremony). The updater sections are M11.

## Reporting a vulnerability

TODO: the Zcash Foundation's disclosure address and policy.

## Threat model summary

What the launcher protects:

- **The user's node.** zebrad's RPC and metrics bind to loopback only and RPC uses cookie
  authentication. The launcher refuses to save a config that changes this.
- **The webview.** Strict CSP (with `'wasm-unsafe-eval'` for the Leptos UI); capabilities
  list only the launcher's own commands; the Grafana iframe is limited to loopback URLs,
  sandboxed, and has no IPC access.
- **What runs on the user's machine.** Installers, the app and the bundled zebrad are
  signed on macOS and Windows; every release asset has a keyless cosign signature and a
  build provenance attestation; updates are verified with minisign before zebrad is
  stopped.
- **The release pipeline.** Untrusted dependency code compiles in unprivileged jobs
  without secrets; bundling and signing run in the `release` environment (required
  reviewer, no restored caches); actions are pinned by SHA.

Out of scope: TODO(M10): for example a compromised user account or OS, and Grafana
security (user-run).

TODO(M10): table of assets, threats and mitigations.

## Release pipeline downloads

What the privileged `bundle_sign` job of
[`release.yml`](../.github/workflows/release.yml) downloads and runs, and how each item is
checked:

| What | From | Checked by |
| --- | --- | --- |
| GitHub Actions | `uses:` lines | pinned by commit SHA |
| rustup-init (Linux container) | static.rust-lang.org | sha256 in [`setup-tauri`](../.github/actions/setup-tauri/action.yml) |
| Rust toolchain | static.rust-lang.org | rustup's signed channel manifest |
| tauri-cli (and trunk, in `build` only) | their GitHub releases | sha256 per platform in [`.github/tool-versions.env`](../.github/tool-versions.env) |
| Artifact Signing client (Windows) | nuget.org | sha256 in `release.yml` |
| zebrad sidecars | `zebrad-sidecars.yml` artifacts | cosign and attestations on Linux, built from the pinned commit elsewhere ([SIDECARS.md](SIDECARS.md)) |
| AppRun, linuxdeploy (Linux AppImage) | tauri-apps/binary-releases, fetched by tauri-bundler | **nothing**; they never see the updater key (below) |
| linuxdeploy-plugin-appimage (Linux AppImage) | the moving `continuous` release of linuxdeploy/linuxdeploy-plugin-appimage, fetched by tauri-bundler | **nothing**; if the download fails, linuxdeploy uses its built-in copy; never sees the updater key |
| NSIS 3.13 and nsis_tauri_utils.dll (Windows) | tauri-apps/binary-releases, tauri-apps/nsis-tauri-utils, fetched by tauri-bundler | SHA-1 hard-coded in tauri-bundler |
| WebView2 bootstrapper | not downloaded at bundle time | the installer fetches it from Microsoft on the user's machine (default `downloadBootstrapper` mode) |

The tauri-bundler rows are from the tauri-bundler 2.10.4 source, the version tauri-cli
2.12.4 pins; re-check them when `TAURI_CLI_VERSION` changes. macOS bundling downloads
nothing.

The unverified Linux AppImage tools run inside `cargo tauri bundle`, so the updater key
is never in that process's environment, on any OS:

- `cargo tauri bundle` gets a throwaway minisign key generated in the job. (With
  `bundle.createUpdaterArtifacts`, tauri-cli signs the updater artifacts itself and fails
  without a key.) Its `.sig` files only record which files to sign.
- The "Sign the updater artifacts" step is the only step with `TAURI_SIGNING_PRIVATE_KEY`
  and its password. It runs only the sha256-pinned cargo-tauri,
  [`scripts/sign-updater-artifacts.sh`](../scripts/sign-updater-artifacts.sh) calling
  `cargo tauri signer sign --private-key-path <file> --app-version <version>` for each
  of those files (Linux AppImage and `.deb`, macOS `.app.tar.gz`, Windows NSIS
  `-setup.exe`). The key file is written with mode 0600 and deleted afterwards, and the
  password goes in the environment, not on the command line. `--app-version` writes the
  trusted comment the bundler writes (`timestamp:…\tfile:…\tversion:<version>`), which
  the updater's `requireSignedVersion` check reads. The script checks every new
  signature for that version and for the key ID of `plugins.updater.pubkey`.

This keeps the key from those tools, not the AppImage: a compromised AppImage tool can
still tamper with the AppImage it builds, which is then signed.

The platform signing stays inside `cargo tauri bundle`, so its credentials do share
the bundler's environment: macOS codesign and notarization (macOS bundling downloads
nothing) and the Windows `signCommand` through Azure OIDC (the only Windows downloads,
NSIS and `nsis_tauri_utils.dll`, are checked against SHA-1 hashes hard-coded in
tauri-bundler; SHA-1 is collision-prone but not known to allow a second preimage).

The unprivileged `build` job has no secrets, but what it compiles is what gets signed.
Besides the first table's Actions, rustup-init, toolchain, tauri-cli and trunk rows, it
runs two tools on the frontend that is embedded in the app:

| What | From | Checked by |
| --- | --- | --- |
| wasm-bindgen CLI, at the `wasm-bindgen` version in `Cargo.lock` | its GitHub release, installed by `setup-tauri` | sha256 per platform in [`.github/tool-versions.env`](../.github/tool-versions.env); `check-versions.sh` checks the pin against `Cargo.lock` |
| wasm-opt (binaryen `version_131`, trunk 0.22.0's default) | its GitHub release, installed by `setup-tauri` | sha256 per platform in `.github/tool-versions.env` |

trunk itself would otherwise download both, without a hash check. `setup-tauri` puts the
pinned ones first on `PATH` and sets `TRUNK_OFFLINE=true` and `TRUNK_TOOLS_WASM_OPT`: in
offline mode trunk downloads nothing and accepts a tool only at exactly the version it
requires, so the build fails rather than fetch a different one.

## Code signing

- macOS: Apple Developer ID, hardened runtime, notarization; sidecars are signed by the
  Tauri bundler. TODO(M10): team ID and certificate holder.
- Windows: Azure Artifact Signing through GitHub OIDC, for installer, app and sidecar.
  TODO(M10): identity and subscription owner.
- Linux: no platform signing; cosign and attestations only.

## Updater key custody and rotation

The updater uses a long-lived minisign key, because Tauri's updater has no keyless mode.
Its public key is compiled into every build from `plugins.updater.pubkey` in
[`src-tauri/tauri.conf.json`](../src-tauri/tauri.conf.json); the private key signs the
updater artifacts in the `release` environment.

- **Ceremony:** two maintainers, together, on an offline machine (no network, booted
  from a clean live image), with the same tauri-cli version that the release workflow
  pins (`TAURI_CLI_VERSION`):

  ```sh
  cargo tauri signer generate -w zebra-launcher-updater.key
  ```

  It asks for a password; always set one (never `--ci`, which leaves the key
  unencrypted). It writes `zebra-launcher-updater.key` (the encrypted private key) and
  `zebra-launcher-updater.key.pub` (the public key, base64). Record the public key's
  SHA-256 in this file, then copy the `.pub` contents into `plugins.updater.pubkey`
  in a reviewed PR. TODO(M10): date, participants, public key fingerprint.
- **The placeholder:** until that PR merges, `pubkey` is `REPLACE_WITH_MINISIGN_PUBLIC_KEY`.
  A build with the placeholder rejects every update (the key does not decode), and
  `scripts/check-versions.sh` fails the release unless the repository variable
  `UPDATER_ENABLED` is `false`. Replace it before the first release with the updater.
- **Custody:** the encrypted key and its password are escrowed offline, separately, with
  two custodians; the CI copy lives only in the `release` environment's secrets
  `TAURI_SIGNING_PRIVATE_KEY` (the key file's contents) and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. TODO(M10): where escrow copies are kept.
- **Rotation:** TODO(M10): schedule and steps, including shipping the new public key in a
  release signed by the old one.
- **Compromise:** TODO(M10): runbook (revoke, ship a key rotation, notify users). Updates
  are opt-in and every asset also carries a keyless cosign signature for out-of-band
  checks.

## What the updater verifies

The updater (`tauri-plugin-updater` 2.14) fetches `latest.json` over HTTPS from the
configured endpoint and treats it as untrusted: it is not signed. For the entry
matching this platform it then checks, before anything is installed:

1. **Newer only.** The announced version must be greater than the running one
   (`allowDowngrades = false`); otherwise there is no update.
2. **Signature.** The downloaded bytes must carry a valid minisign signature (the
   `.sig` file's content, embedded in `latest.json`) from the public key compiled into
   the app. minisign's global signature also covers the signature's trusted comment.
3. **Signed version.** With `requireSignedVersion = true`, the trusted comment must
   contain `version:<v>` and `<v>` must equal the version `latest.json` announces.
   This stops a tampered `latest.json` from pairing a new version number with an older,
   validly signed artifact (a downgrade). Signatures without a version are refused.

The plugin checks the signature when the download finishes, the launcher checks it
again (on a blocking thread) right before it stops zebrad, and the plugin checks once
more inside `install`. A package that fails is discarded and zebrad is never stopped
for it.

`requireSignedVersion` puts a requirement on how artifacts are signed: the signing tool
must write the version into the trusted comment. Verified in the tauri-cli 2.12.0 and
2.12.4 sources: `cargo tauri build` / `cargo tauri bundle` sign updater artifacts with
the bundle version (`timestamp:…\tfile:…\tversion:<version>`); `cargo tauri signer sign`
does so only with `--app-version <version>`, so never sign by hand without it. The
first signed release still has to confirm it on real assets (see
[RELEASING.md](RELEASING.md#before-publishing-a-release-with-the-updater)).

Not covered by the signature: `latest.json` itself (notes, date, URLs; only the TLS
connection to GitHub protects them), and anything that happens before the user opted
into updates (Off mode makes no requests). The UI therefore treats the release notes as
untrusted text: it shows a plain-text excerpt (no HTML, no links), and the only page it
opens about an update is the fixed releases page (`open_path("releases_page")`), never
a URL from the manifest.

## Rollback policy

Roll forward only. The updater never installs an older version (`allowDowngrades =
false`, and the signed version check above), so a bad release is fixed by a newer one:
move "latest" back to the previous good release to stop the rollout, then ship a patch
release ([RELEASING.md](RELEASING.md#bad-release-runbook)). A user who wants to go
back reinstalls a previous installer from the releases page by hand, which the
post-update recovery screen links to.

## Exception to Zebra ADR 0008

Zebra's ADR 0008 prefers keyless signing. The minisign updater key is a documented
exception: Tauri's updater verifies only minisign signatures with a public key compiled
into the app. Everything else (cosign signatures, attestations) is keyless.
TODO(M10): link ADR 0008 and record the maintainers' sign-off.

## Verifying a download

TODO(M10): exact commands and identities, tested against a real release.

- **Checksums:** compare with `SHA256SUMS` from the release.
- **cosign:** `cosign verify-blob SHA256SUMS --bundle SHA256SUMS.sigstore.json
  --certificate-identity <release workflow identity>
  --certificate-oidc-issuer https://token.actions.githubusercontent.com`.
- **Attestation:** `gh attestation verify <file> --repo ZcashFoundation/zebra-launcher`.
- **macOS:** `spctl -a -vv "/Applications/Zebra Launcher.app"` reports a notarized
  Developer ID.
- **Windows:** `Get-AuthenticodeSignature <installer>` reports `Valid`.

The bundled Linux zebrad is verified at build time against Zebra's own signatures
(`cosign verify-blob` and `gh attestation verify`); macOS and Windows zebrad are built
from the Zebra tag in this repo's CI and attested.
