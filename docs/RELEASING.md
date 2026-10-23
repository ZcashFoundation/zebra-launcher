# Releasing

> Skeleton. The pipeline lands in M9 (unsigned bundles) and M10 (signed releases);
> sections marked TODO are filled in then. The goal (M12): another maintainer can cut a
> release candidate from a clean clone by following this file alone.

How the pipeline works is summarised in [ARCHITECTURE.md](ARCHITECTURE.md#release-pipeline-m9-m10).

## Versions

- Launcher version: `[workspace.package] version` in [`Cargo.toml`](../Cargo.toml), the
  only place it lives. The launcher has its own semver, independent of zebrad.
- Bundled zebrad: [`ZEBRAD_VERSION`](../ZEBRAD_VERSION).
- Release candidates are tagged `vX.Y.Z-rc.N` and published as prereleases, so
  `latest.json` (the updater endpoint) does not move.

## zebrad bump SLA

A launcher release with the new zebrad ships **within 14 days of each zebrad release**.
On Mainnet, zebrad halts 56 days after its estimated release height, so a slipped bump
eventually stops every user's node.

Triggers:

1. Primary: the Zebra release checklist includes bumping the launcher.
2. Backstop: `zebrad-watch.yml` (daily and on dispatch) compares Zebra's latest
   `vX.Y.Z` release with `ZEBRAD_VERSION` and opens a bump PR with a GitHub App token.

Bump PR checklist:

- [ ] `ZEBRAD_VERSION` updated; `bundle.yml` green on all five targets (Tier-3 macOS and
      Windows zebrad builds included).
- [ ] The previous release's generated config validates against the new zebrad.
- [ ] Log fixtures re-captured if zebrad's wording changed (see
      [DEVELOPMENT.md](DEVELOPMENT.md#log-fixtures)).
- [ ] Grafana dashboards re-vendored with `docs/observability/sync-dashboards.sh`.
- [ ] TODO(M9): further checks.

## Release-day runbook

<!-- TODO(M10): exact commands, links to the workflows, expected durations. -->

1. TODO(M10): prepare the release PR (version bump, changelog).
2. TODO(M10): push the tag `vX.Y.Z-rc.N`; `release.yml` creates the draft release.
3. TODO(M10): approve the `release` environment (required reviewer, self-review blocked).
4. TODO(M10): verify the draft assets: `spctl -a -vv` and `codesign -dv` (macOS),
   `Get-AuthenticodeSignature` (Windows), `cosign verify-blob`,
   `gh attestation verify`, and `latest.json` lists all five platform keys.
5. TODO(M11): rehearse the update rc.N → rc.N+1 on every target.
6. TODO(M10): smoke-test, then publish. Publishing makes `latest.json` live.
7. TODO(M10): announce.

## Bad-release runbook

<!-- TODO(M10): owner, contact path, exact commands. -->

- **On-call owner:** TODO(M10).
- **Stop the rollout:** mark the release as prerelease, or set `make_latest` on the
  previous good release, so `releases/latest/download/latest.json` moves back.
  Updaters with `allowDowngrades = false` will not downgrade users already on the bad
  version; they need the fix-forward release.
- **Fix forward:** hotfix branch and tag. The rc rehearsal may be skipped only with an
  explicit, recorded sign-off. TODO(M10): who signs off and where it is recorded.
- **Afterwards:** TODO(M10): post-mortem template.

## Key ceremony

The minisign updater key is generated, escrowed and rotated as described in
[SECURITY.md](SECURITY.md#updater-key-custody-and-rotation). TODO(M10): link the ceremony
record.

## Cost and timing

Estimated with warm sidecar caches: 120–180 runner minutes, 40–60 min wall clock.
TODO(M9): replace with measured times.
