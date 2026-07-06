---
name: release
description: Cut a loomux release — version bump across all four files (including Cargo.lock), tag, CI publish, and npm trusted-publishing verification.
---

# Cutting a loomux release

Releases are tag-driven: pushing a `v*` tag runs `.github/workflows/release.yml`,
which builds installers for Windows / macOS (arm64 + x64) / Linux, creates the
GitHub release, and then publishes the `loomux-desktop` npm launcher.

## 1. Bump the version — four files, in one PR

The version lives in **four** places that must stay in lockstep:

| File | Field |
| --- | --- |
| `package.json` | `version` |
| `src-tauri/tauri.conf.json` | `version` |
| `src-tauri/Cargo.toml` | `[package] version` |
| `src-tauri/Cargo.lock` | the `loomux` package entry |

**Cargo.lock is the one that gets missed** (it happened: the 0.5.0 bump PR
#89 needed follow-up #90). After editing Cargo.toml, run
`cargo check --locked` in `src-tauri/` — it will fail if the lock is stale;
regenerate with plain `cargo check` and commit the lock change.

`npm/package.json` also carries the version, but the publish job overwrites it
from the tag (`npm version "${GITHUB_REF_NAME#v}"`) — keep it in lockstep
anyway so the tree reads consistently.

Commit as `chore(release): bump version to X.Y.Z`, PR to `main`, and stop —
**the human merges** (as always in this repo).

## 2. Tag (human-gated)

After the bump PR is merged, tagging is the human's call — confirm before
pushing a tag, since it publishes immediately:

```sh
git checkout main && git pull
git tag vX.Y.Z
git push origin vX.Y.Z
```

## 3. Watch and verify

- `gh run list --workflow release.yml` / `gh run watch` — four build jobs
  (matrix) then `publish-npm`.
- npm auth is **trusted publishing (OIDC)** — no `NPM_TOKEN` secret exists;
  if publish fails with an auth error, the fix is in npm's trusted-publisher
  config for the repo, not in secrets.
- Verify: `npm view loomux-desktop version` shows X.Y.Z, and the GitHub
  release has all asset types (`-setup.exe`/`.msi`, both `.dmg`s, `.AppImage`
  + `.deb` + `.rpm`).
- Builds are unsigned; the macOS "damaged app" caveat in the release body and
  README is expected, not a regression.
