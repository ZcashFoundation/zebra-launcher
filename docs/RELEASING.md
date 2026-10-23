# Releasing

How to cut a Zebra Launcher release with
[`release.yml`](../.github/workflows/release.yml). The goal: another maintainer can cut a
release candidate from a clean clone by following this file alone. The pipeline design
is in [ARCHITECTURE.md](ARCHITECTURE.md#release-pipeline-m9-m10), and signing and key
custody are in [SECURITY.md](SECURITY.md).

## Versions

- Launcher version: `[workspace.package] version` in [`Cargo.toml`](../Cargo.toml), the
  only place it lives. The launcher has its own semver, independent of zebrad.
- Bundled zebrad: [`ZEBRAD_VERSION`](../ZEBRAD_VERSION).
- Tags are `vX.Y.Z`, or `vX.Y.Z-rc.N` for release candidates. `release.yml` refuses a tag
  that differs from the workspace version. Release candidates are created as
  prereleases, so `latest.json` (the updater endpoint) does not move.

## Before the tag

1. Check that [`bundle.yml`](../.github/workflows/bundle.yml) passed on `main` in the last
   week on all five targets. To run it now: `gh workflow run bundle.yml --ref main`.
2. Open the release PR:
   - set `[workspace.package] version` in `Cargo.toml`, then run
     `cargo update --workspace` so `Cargo.lock` matches;
   - add a `CHANGELOG.md` section whose `## ` heading contains the version (for example
     `## 0.2.0 - 2026-11-02`). The draft's release notes are taken from that section.
3. Merge the PR.

## Release-day runbook

Expect 40–60 minutes from tag to draft with warm zebrad caches, longer if the macOS or
Windows zebrad builds are cold.

1. **Tag** the merged commit:

   ```sh
   git switch main && git pull --ff-only
   git tag -s v0.2.0 -m "Zebra Launcher 0.2.0"
   git push origin v0.2.0
   ```

2. **Wait for the unprivileged jobs.** `verify`, `draft` (creates the draft release),
   `sidecars` and `build` run without secrets.
3. **Approve the `release` environment.** In the workflow run, use *Review deployments*,
   then select `release`. The approver must not be the person who pushed the tag;
   self-review is blocked. The five `bundle_sign` jobs then package, sign, notarize and
   upload to the draft.
4. **Wait for `manifest` and `summary`.** `manifest` adds `latest.json`, `SHA256SUMS`,
   `SHA256SUMS.sigstore.json` and attestations. The `summary` job's page shows the
   checklist below and the draft's link.
5. **Download the draft**, then verify it:

   ```sh
   TAG=v0.2.0
   gh release download "$TAG" --repo ZcashFoundation/zebra-launcher --dir "rel-$TAG"
   cd "rel-$TAG"
   sha256sum -c SHA256SUMS
   cosign verify-blob SHA256SUMS --bundle SHA256SUMS.sigstore.json \
     --certificate-identity "https://github.com/ZcashFoundation/zebra-launcher/.github/workflows/release.yml@refs/tags/$TAG" \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com
   for f in zebra-launcher_* latest.json; do
     gh attestation verify "$f" --repo ZcashFoundation/zebra-launcher \
       --signer-workflow ZcashFoundation/zebra-launcher/.github/workflows/release.yml
   done
   jq '{version, platforms: (.platforms | keys)}' latest.json   # five keys, this version
   ```

6. **Smoke-test** each OS with the checklist below.
7. **Rehearse the update** (once the updater ships, M11): on every target, update from
   the previous rc to this one, using a build with the build-time endpoint override.
8. **Publish:**

   ```sh
   gh release edit "$TAG" --draft=false --latest          # vX.Y.Z on the current line
   gh release edit "$TAG" --draft=false --prerelease      # vX.Y.Z-rc.N
   gh release edit "$TAG" --draft=false --latest=false    # hotfix of an older line
   ```

   Publishing is what makes `latest.json` live. Then check:
   `curl -sL https://github.com/ZcashFoundation/zebra-launcher/releases/latest/download/latest.json | jq .version`.
9. **Announce** the release in the usual Zcash Foundation channels, with the release
   link.

### Smoke-test checklist

<!-- smoke-checklist:start -->
- [ ] `sha256sum -c SHA256SUMS`, `cosign verify-blob` and `gh attestation verify` pass
      for every asset; `latest.json` lists `linux-x86_64`, `linux-aarch64`,
      `darwin-aarch64`, `darwin-x86_64` and `windows-x86_64` with this version.
- [ ] **Linux x86_64** (Ubuntu 22.04 and 24.04): the AppImage starts after `chmod +x`,
      the node reaches Running, and About shows "Zebra Launcher X.Y.Z (zebrad A.B.C)".
- [ ] **Linux deb** (Debian 12): `sudo apt install ./zebra-launcher_X.Y.Z_amd64.deb`
      installs, the app starts, and the update setting is Notify-only.
- [ ] **Linux aarch64**: the AppImage starts and the node reaches Running.
- [ ] **macOS Apple Silicon and Intel**: the dmg opens; after copying to Applications,
      `spctl -a -vv "/Applications/Zebra Launcher.app"` reports
      `source=Notarized Developer ID`, and
      `codesign -dv --verbose=4 "/Applications/Zebra Launcher.app/Contents/MacOS/zebra-launcher-zebrad"`
      shows the Developer ID authority and `flags=0x10000(runtime)`. The app opens
      without an "unidentified developer" block and the node reaches Running.
- [ ] **Windows 11**: `Get-AuthenticodeSignature .\zebra-launcher_X.Y.Z_x86_64-setup.exe`
      is `Valid`; the installer runs per-user without a UAC prompt;
      `Get-AuthenticodeSignature` is `Valid` for `zebra-launcher.exe` and
      `zebra-launcher-zebrad.exe` in `%LOCALAPPDATA%\Zebra Launcher`; the node reaches
      Running.
- [ ] **Every OS**: with close-to-tray on, closing the window keeps the node running;
      tray Quit leaves no `zebra-launcher-zebrad` process.
- [ ] **Updater (M11 and later)**: the update from the previous rc to this rc works on
      every target.
<!-- smoke-checklist:end -->

## Bad-release runbook

- **Owner:** the maintainer who approved the `release` deployment owns the release for
  72 hours after publishing, unless they hand it over in the release's PR or thread.
- **Stop the rollout.** The updater reads
  `releases/latest/download/latest.json`, so move "latest" back to the previous good
  release:

  ```sh
  gh release edit v0.2.0 --prerelease   # the bad release leaves "latest"
  gh release edit v0.1.3 --latest       # and the previous good one takes it back
  curl -sL https://github.com/ZcashFoundation/zebra-launcher/releases/latest/download/latest.json | jq .version
  ```

  Do not delete the bad release: installer links and attestations point at it. Users
  who already updated stay on the bad version (`allowDowngrades = false`) until a fix
  ships.
- **Fix forward.** Branch from the bad tag, fix, bump the patch version (`0.2.1`), and
  release it through this runbook. The rc rehearsal may be skipped only with a second
  maintainer's sign-off, recorded as an approving review on the hotfix PR that states
  "rc rehearsal skipped" and why. The deployment approver must still be someone other
  than the tagger.
- **Afterwards:** open an issue titled `Post-mortem: vX.Y.Z` with the timeline, impact,
  root cause, why the pipeline and checklist did not catch it, and follow-up tasks.

## zebrad bump SLA

A launcher release with the new zebrad ships **within 14 days of each zebrad release**.
On Mainnet, zebrad halts 56 days after its estimated release height, so a slipped bump
eventually stops every user's node.

Triggers:

1. Primary: the Zebra release checklist includes bumping the launcher.
2. Backstop: `zebrad-watch.yml` (daily and on dispatch) compares Zebra's latest
   `vX.Y.Z` release with `ZEBRAD_VERSION` and opens a bump PR with a GitHub App token.

Bump PR checklist (the steps are in [SIDECARS.md](SIDECARS.md#bumping-zebrad_version)):

- [ ] `ZEBRAD_VERSION` and fake-zebrad's `VERSION` updated.
- [ ] `bundle.yml` green on all five targets (Tier-3 macOS and Windows zebrad builds
      included); it checks that each bundled zebrad reports `ZEBRAD_VERSION`.
- [ ] The previous release's generated config validates against the new zebrad.
- [ ] Log fixtures re-captured if zebrad's wording changed (see
      [DEVELOPMENT.md](DEVELOPMENT.md#log-fixtures)).
- [ ] Grafana dashboards re-vendored with `docs/observability/sync-dashboards.sh`.

After the bump merges, cut a patch release with this runbook within the 14 days.

## Secrets inventory

All signing material lives in the GitHub environment **`release`** (required reviewer,
self-review blocked, deployments limited to `v*` tags), so only `bundle_sign` can read
it, and only after approval. Custody, escrow and rotation are in
[SECURITY.md](SECURITY.md#updater-key-custody-and-rotation).

| Name | Kind | Used for | Backup / reissue |
| --- | --- | --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | `release` secret | updater `.sig` files (minisign) | offline escrow, two custodians (SECURITY.md) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | `release` secret | decrypts the key above | escrowed with the key |
| `APPLE_CERTIFICATE` | `release` secret | Developer ID Application `.p12`, base64 | Apple Developer account holder reissues |
| `APPLE_CERTIFICATE_PASSWORD` | `release` secret | `.p12` password | set when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | `release` secret | codesign identity name | from the certificate |
| `APPLE_API_ISSUER` | `release` secret | notarization (App Store Connect issuer ID) | App Store Connect admin |
| `APPLE_API_KEY` | `release` secret | notarization (API key ID) | App Store Connect admin reissues |
| `APPLE_API_PRIVATE_KEY` | `release` secret | `.p8` contents; the job writes it to `APPLE_API_KEY_PATH` | App Store Connect admin reissues (download once) |
| `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` | `release` secrets | `azure/login` OIDC; identifiers, not credentials | Azure subscription owner |
| `AZURE_ARTIFACT_SIGNING_ENDPOINT`, `AZURE_ARTIFACT_SIGNING_ACCOUNT`, `AZURE_ARTIFACT_SIGNING_PROFILE` | `release` variables | Artifact Signing account and certificate profile | Azure subscription owner |
| `UPDATER_ENABLED` | repository variable | set to `false` only for releases cut before the updater exists; then no `latest.json` is required | — |

The Azure app registration has one federated credential, with subject
`repo:ZcashFoundation/zebra-launcher:environment:release`, and the *Artifact Signing
Certificate Profile Signer* role on the certificate profile. It has no client secret.
cosign signatures and attestations are keyless (GitHub OIDC) and need no secret.

## Key ceremony

The minisign updater key is generated, escrowed and rotated as described in
[SECURITY.md](SECURITY.md#updater-key-custody-and-rotation).

## Cost and timing

Estimated with warm sidecar caches: 120–180 runner minutes, 40–60 min wall clock. Replace
with the times measured on the first real runs.
