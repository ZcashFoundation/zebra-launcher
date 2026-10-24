# Releasing

How to cut a Zebra Launcher release with
[`release.yml`](../.github/workflows/release.yml). The goal: another maintainer can cut a
release candidate from a clean clone by following this file alone. The pipeline design
is in [ARCHITECTURE.md](ARCHITECTURE.md#release-pipeline), and signing and key
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
   - add a `CHANGELOG.md` section whose `## ` heading starts with the exact version,
     optionally as `v0.2.0` or `[0.2.0]` (for example `## 0.2.0 - 2026-11-02`). The
     draft's release notes are taken from that section
     ([`changelog-section.sh`](../scripts/changelog-section.sh)); `## 0.2.0-rc.1` is a
     different version and never supplies the notes for `0.2.0`. Without a section the
     notes say so in one line and the run shows a warning.
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
   upload to the draft. In each, `cargo tauri bundle` runs with a throwaway updater key,
   and a separate step re-signs the updater artifacts with the real one.
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
7. **Check the updater signatures** (releases with the updater): see
   [Before publishing a release with the updater](#before-publishing-a-release-with-the-updater).
8. **Publish as a prerelease, then rehearse the update.** Draft assets cannot be
   downloaded anonymously, so the rehearsal needs the release published; as a
   prerelease it does not move "latest", so no user is offered it yet:

   ```sh
   gh release edit "$TAG" --draft=false --prerelease
   ```

   Then follow [Update rehearsal](#update-rehearsal) on every target. If it fails, do
   not promote the release; fix forward with the next rc.
9. **Promote** (final releases only; an rc stays a prerelease):

   ```sh
   gh release edit "$TAG" --prerelease=false --latest          # vX.Y.Z on the current line
   gh release edit "$TAG" --prerelease=false --latest=false    # hotfix of an older line
   ```

   Becoming "latest" is what makes `latest.json` live for users. Then check:
   `curl -sL https://github.com/ZcashFoundation/zebra-launcher/releases/latest/download/latest.json | jq .version`.
10. **Announce** the release in the usual Zcash Foundation channels, with the release
   link.

### Smoke-test checklist

<!-- smoke-checklist:start -->
- [ ] `sha256sum -c SHA256SUMS`, `cosign verify-blob` and `gh attestation verify` pass
      for every asset; `latest.json` lists `linux-x86_64`, `linux-aarch64`,
      `darwin-aarch64`, `darwin-x86_64` and `windows-x86_64` with this version.
- [ ] `for s in *.sig; do base64 -d < "$s" | sed -n 3p; done` prints, for every
      signature, a trusted comment ending in `version:X.Y.Z` (this version).
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
- [ ] **Updater** (releases that ship it): every `.sig` names this version (see below), and
      the [update rehearsal](#update-rehearsal) from the previous rc to this release
      works on every target; the `.deb` shows the release link instead of installing.
<!-- smoke-checklist:end -->

## The updater gate (`UPDATER_ENABLED`)

The repository variable `UPDATER_ENABLED` says whether releases ship the updater:

- unset or anything but `false` (the default once the updater key exists): `bundle_sign` must produce
  updater artifacts and `.sig` files, `manifest` must assemble a `latest.json` with all
  five platform keys, and `scripts/check-versions.sh` (in the `verify` job) fails while
  `plugins.updater.pubkey` in `src-tauri/tauri.conf.json` is still the placeholder
  `REPLACE_WITH_MINISIGN_PUBLIC_KEY`.
- `false`: only for releases cut before the updater key exists (a signing rehearsal). No
  `latest.json` is required and the placeholder key is accepted. Such a
  release cannot update itself; its users install the next one by hand.

The `verify` job decides this once, in its `updater` step (`require_updater`, from the
variable and `bundle.createUpdaterArtifacts`), and runs `check-versions.sh` after it with
`UPDATER_ENABLED` set to that decision, so the public-key check and the artifacts always
agree.

To turn the updater on: run the key ceremony
([SECURITY.md](SECURITY.md#updater-key-custody-and-rotation)), merge the public key into
`tauri.conf.json`, store the private key and password as `release` secrets, then delete
the variable (or set it to `true`).

## Before publishing a release with the updater

`requireSignedVersion` makes every installed launcher refuse a signature that does not
name the version `latest.json` announces. Check the draft before publishing; for the
first signed release this is also the check that tauri-cli writes the version (the
tauri-cli 2.12 source does; see [SECURITY.md](SECURITY.md#what-the-updater-verifies)):

```sh
VERSION=0.2.0   # without the leading v
jq -r '.platforms | to_entries[] | "\(.key) \(.value.signature)"' latest.json |
  while read -r key sig; do
    comment="$(printf '%s' "$sig" | base64 -d | sed -n 3p)"
    case "$comment" in
      *"version:${VERSION}") echo "ok   $key" ;;
      *) echo "FAIL $key: $comment" ;;
    esac
  done
```

The third line of a decoded signature is its trusted comment, for example
`trusted comment: timestamp:1793000000<TAB>file:zebra-launcher_0.2.0_x86_64.AppImage<TAB>version:0.2.0`.
Every key must print `ok`.

## Update rehearsal

Each release is rehearsed by updating to it from the previous rc on all five targets.
The "from" app must look for the new release instead of `releases/latest`, which a
prerelease never becomes, so it is a build of the previous rc's tag with a
**build-time** endpoint override. Release builds ignore runtime overrides by design;
the override is baked in with `--config`, or with the `TAURI_CONFIG` environment
variable, which takes the same JSON.

For a rehearsal from `v0.2.0-rc.2` to `v0.2.0-rc.3`, save as `rehearsal.json`:

```json
{
  "bundle": { "createUpdaterArtifacts": false },
  "plugins": {
    "updater": {
      "endpoints": [
        "https://github.com/ZcashFoundation/zebra-launcher/releases/download/v0.2.0-rc.3/latest.json"
      ]
    }
  }
}
```

`endpoints` replaces the configured list (arrays are not merged); everything else,
including the public key, comes from the tag's `tauri.conf.json`. The "from" build
needs no updater artifacts of its own, hence `createUpdaterArtifacts: false` (and no
signing key). On each target, with the sidecar in place as for any local build
([DEVELOPMENT.md](DEVELOPMENT.md#release-build)), check out `v0.2.0-rc.2` and run:

```sh
cargo tauri build --config rehearsal.json
# or: TAURI_CONFIG="$(cat rehearsal.json)" cargo tauri build
```

Then, with `v0.2.0-rc.3` published as a prerelease:

1. Install the rehearsal build, start it, let the node reach Running, and set updates
   to Notify. Within a minute (or with *Check for updates*) it offers `0.2.0-rc.3`.
2. Install it. The zebrad log shows the graceful shutdown (`stopping zebrad`), the
   launcher relaunches as `0.2.0-rc.3` (About), and the node reaches Running within
   5 minutes with no recovery screen.
3. On Debian, the `.deb` build shows the release link instead of an Install button.

The refusals are checked with a debug bundle (`cargo tauri build --debug --config
rehearsal.json`), which reads `LAUNCHER_UPDATE_ENDPOINT` and accepts `http://`, pointed
at a local copy of `latest.json` served from a scratch directory (for example with
`python3 -m http.server`):

- a `.sig` of another file, or one changed byte in the signature: the update fails
  with a signature error and zebrad keeps running;
- a `latest.json` whose `version` is lower than the running one: "up to date";
- a `latest.json` that announces a higher version with an older release's `url` and
  `signature`: refused (the signed version does not match);
- updates Off: the local server's access log stays empty for the whole session.

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
2. Backstop: [`zebrad-watch.yml`](../.github/workflows/zebrad-watch.yml) (daily at
   05:17 UTC and on dispatch) compares Zebra's latest `vX.Y.Z` release that has signed
   binaries with `ZEBRAD_VERSION` and opens a bump PR (`bump/zebrad-<ver>`) with a
   GitHub App token, since PRs opened with `GITHUB_TOKEN` do not trigger CI. It needs
   the `ZEBRA_LAUNCHER_APP_ID` and `ZEBRA_LAUNCHER_APP_PRIVATE_KEY` secrets; without
   them it opens a `release-blocker` issue instead. See
   [SIDECARS.md](SIDECARS.md#bumping-zebrad_version).

GitHub pauses scheduled workflows after 60 days without repository activity, which
is why the watcher is only the backstop. If the Actions tab shows **zebrad watch** as
disabled, re-enable it and run it once by hand.

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
| `TAURI_SIGNING_PRIVATE_KEY` | `release` secret | updater `.sig` files (minisign); only the "Sign the updater artifacts" step sees it, never `cargo tauri bundle` ([SECURITY.md](SECURITY.md#release-pipeline-downloads)) | offline escrow, two custodians (SECURITY.md) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | `release` secret | decrypts the key above | escrowed with the key |
| `APPLE_CERTIFICATE` | `release` secret | Developer ID Application `.p12`, base64 | Apple Developer account holder reissues |
| `APPLE_CERTIFICATE_PASSWORD` | `release` secret | `.p12` password | set when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | `release` secret | codesign identity name | from the certificate |
| `APPLE_API_ISSUER` | `release` secret | notarization (App Store Connect issuer ID) | App Store Connect admin |
| `APPLE_API_KEY` | `release` secret | notarization (API key ID) | App Store Connect admin reissues |
| `APPLE_API_PRIVATE_KEY` | `release` secret | `.p8` contents; the job writes it to `APPLE_API_KEY_PATH` | App Store Connect admin reissues (download once) |
| `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` | `release` secrets | `azure/login` OIDC; identifiers, not credentials | Azure subscription owner |
| `AZURE_ARTIFACT_SIGNING_ENDPOINT`, `AZURE_ARTIFACT_SIGNING_ACCOUNT`, `AZURE_ARTIFACT_SIGNING_PROFILE` | `release` variables | Artifact Signing account and certificate profile | Azure subscription owner |
| `UPDATER_ENABLED` | repository variable | optional override; see below | — |

Whether a release must ship updater artifacts (the `.sig` files and `latest.json`) follows
`bundle.createUpdaterArtifacts` in [`tauri.conf.json`](../src-tauri/tauri.conf.json):
`release.yml`'s `verify` job reads it once and passes the result to `bundle_sign`,
`collect-bundles.sh --require-updater-sigs` and `manifest`. Leave `UPDATER_ENABLED` unset
normally. Set it to `false` only to cut a release without `latest.json` although the config
enables updater artifacts (for example a signing rehearsal before the updater ships); the
run then warns. `true` only asserts that the config enables them, and fails otherwise.

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
