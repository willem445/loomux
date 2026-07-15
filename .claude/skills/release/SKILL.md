---
name: release
description: Cut a loomux release — version bump across all five files (including Cargo.lock and package-lock.json), bump PR, human-gated tag, CI publish, release notes, and npm trusted-publishing verification. Covers stable and beta/RC (pre-release) tags.
---

# Cutting a loomux release

Releases are tag-driven: pushing a `v*` tag runs `.github/workflows/release.yml`,
which builds installers for Windows / macOS (arm64 + x64) / Linux, creates the
GitHub release, and then publishes the `loomux-desktop` npm launcher.

**The workflow runs from the tag's commit, not from main.** Any fix to
`release.yml` only takes effect for a tag that points at (or after) the fixed
commit — re-running a failed job re-runs the old workflow. This bit us in
v0.7.1: fixing the publish step required moving the tag (see step 5).

## 1. Bump the version — five files, in one PR

The version lives in **five** places that must stay in lockstep:

| File | Field |
| --- | --- |
| `package.json` | `version` |
| `package-lock.json` | `version` (both top-level and `packages[""].version`) |
| `src-tauri/tauri.conf.json` | `version` |
| `src-tauri/Cargo.toml` | `[package] version` |
| `src-tauri/Cargo.lock` | the `loomux` package entry |

**The lockfiles are what get missed** (Cargo.lock: the 0.5.0 bump PR #89
needed follow-up #90; package-lock.json: the 0.8.0 bump PR #220 needed
follow-up #224). After editing Cargo.toml, run plain `cargo check` in
`src-tauri/` to regenerate the lock, then `cargo check --locked` to prove
it's consistent, and commit the lock change. After editing the root
`package.json`, run `npm install --package-lock-only` to regenerate
`package-lock.json` — verify the diff touches only the version fields (no
dependency churn) before committing it.

`npm/package.json` also carries the version, but the publish job overwrites it
from the tag (`npm version "${GITHUB_REF_NAME#v}"`) — keep it in lockstep
anyway so the tree reads consistently.

CI has a mechanical backstop for this (#274): the "Check version
consistency" step (`node scripts/check-versions.js`) checks all seven
version fields across these six files (the five above plus
`npm/package.json`) and fails the build if any disagree, so a missed
lockfile bump can't merge silently again. Run `npm run check:versions`
locally before opening the bump PR if you want the same check without
waiting on CI.

Commit as `chore(release): bump version to X.Y.Z`, PR to `main`, wait for CI,
and stop — **the human merges** (as always in this repo).

PowerShell note: multi-line PR/issue bodies via `gh` break on inline quoting —
pipe a single-quoted here-string into `--body-file -` instead.

## 2. Tag (human-gated)

After the bump PR is merged, tagging is the human's call — confirm before
pushing a tag, since it publishes immediately:

```sh
git checkout main && git pull
git tag vX.Y.Z
git push origin vX.Y.Z
```

## 3. Watch the workflow

`gh run list --workflow release.yml` then watch the run — `create-release`,
four `build` matrix legs, `promote`, and `publish-npm`, in that dependency
order.

- `create-release` creates the draft release once and hands its id to every
  `build` leg (`releaseId` input) and to `promote`. This closed #282: legs
  used to independently look up-or-create the release, which raced (~3% per
  upstream tauri-apps/tauri-action#914) into two drafts for one tag when legs
  started near-simultaneously. It hit v0.9.0's first live run for real — 5/9
  assets went public, 4 were stranded on a stray draft recovered by hand.
  `create-release` is also idempotent: if a release for the tag already
  exists (e.g. a "Re-run all jobs" after a partial failure), it reuses that
  release's id instead of spawning a second draft.
- `promote` verifies the release's own **asset count** (expects 9) before
  flipping it public, and refuses — leaving the release in draft — if short.
  If `promote` fails with "Only N/9 assets" in the logs, don't just re-run
  it: check `gh api repos/OWNER/REPO/releases` for a stray duplicate release
  on the same tag first. If it's a genuinely missing/failed matrix leg
  instead, re-run that leg, then re-run `promote`.
- npm auth is **trusted publishing (OIDC)** — no `NPM_TOKEN` secret exists; if
  publish fails with an *auth* error, the fix is in npm's trusted-publisher
  config for the repo, not in secrets.
- The publish step installs a **pinned npm version** (see the comment in
  `release.yml`). Do not switch it back to `@latest` casually: npm 12.0.0
  shipped missing its own `sigstore` bundle, and trusted publishing
  auto-enables provenance, so every publish died with `MODULE_NOT_FOUND:
  sigstore` (upstream npm/cli#9722; our un-pin tracker is #186). If publish
  fails with a MODULE_NOT_FOUND inside npm's own tree, suspect the npm
  version before suspecting the repo.
- The **Docs workflow also fires on `v*` tags** and deploys the docs site.
  It deploys to the `github-pages` **environment**, whose deployment policy
  must allow tags matching `v*` — a repo-settings toggle only the human can
  grant (Settings → Environments → github-pages → deployment branches/tags).
  If the Docs deploy is rejected with an environment-protection error, that
  policy is the cause; Pages itself must also be enabled
  (`build_type: workflow`).
- Known flake: `pty::tests::direct_spawn_selection` on macOS (#183) — a
  platform job can fail on it while the others pass. Re-run the failed job
  before diagnosing anything else.

## 4. Release notes

The workflow creates the GitHub release with a generic download blurb. Write
real notes after the assets are up:

- Match the previous release's voice and structure (`gh release view
  vPREV --json body`): H1 `# loomux vX.Y.Z`, a one-line theme, `## ✨
  Highlights` with emoji H3s per feature, reliability/fixes sections as
  warranted, and always the closing *unsigned installers* footer (macOS
  "damaged app" / Windows SmartScreen note — expected, not a regression).
- Scale to the release: hotfixes get "The fix" up top and a short "Also in
  this release".
- **Apply notes to the canonical `release_id` (from `create-release`'s job
  output), never by tag lookup.** Find it in the release run's logs — the
  `create-release` job logs "Created release `<id>` for vX.Y.Z" (or
  "Reusing existing release `<id>`..."), and `promote`'s "Verify asset
  count" step logs the same id again. Apply with:
  ```sh
  gh api -X PATCH repos/OWNER/REPO/releases/RELEASE_ID -F body=@notes.md
  ```
  **Why not `gh release edit vX.Y.Z --notes-file -`:** that resolves the
  release by tag, and a tag-based lookup is exactly the ambiguity that let
  v0.9.0's incident go wrong (#282) — two draft releases existed for the
  same tag, and only pinning every operation to the one true `release_id`
  (not the tag) keeps assets, promotion, *and* notes from ever drifting onto
  the wrong one. `create-release`'s idempotence guard and the concurrency
  group make a stray duplicate far less likely now, but "never by tag" costs
  nothing and closes the class of bug for good.
  - `promote` also warns (non-fatally, right before it flips the release
    public) if the release body is still empty at that point, so a
    notes-less publish is loud in the run log instead of silently going out
    with just the generic download blurb.

## 5. Verify — the release isn't done until all of these pass

- `npm view loomux-desktop version` → X.Y.Z.
- The GitHub release has **9 assets**: `-setup.exe` + `.msi`, both `.dmg`s,
  `.AppImage` + `.deb` + `.rpm`, and the two `.app.tar.gz` bundles.
- The release run's conclusion is `success` (not just "the assets exist" —
  publish-npm is the last job and can fail after the assets upload).

## If publish-npm fails after the assets are up

Re-running the failed job re-uses the tag's workflow. If the fix needs a
workflow change:

1. PR the `release.yml` fix; human merges.
2. **Move the tag** — this deletes a published tag, so it needs the human's
   explicit go-ahead (permission rules will rightly block it otherwise):
   ```sh
   git push origin :refs/tags/vX.Y.Z
   git tag -f vX.Y.Z origin/main
   git push origin vX.Y.Z
   ```
3. The workflow re-runs from the fixed commit; installers rebuild identically
   and re-attach to the existing release, and hand-written notes survive.

## 6. Beta / RC (pre-release) tags

A tag with a hyphenated suffix — `vX.Y.Z-beta`, `vX.Y.Z-rc1`, anything
`contains(tag, '-')` — is a pre-release. `release.yml` detects this from the
tag string alone (no separate input); every job downstream re-checks the
same `contains(github.ref_name, '-')` condition, so nothing extra needs
setting up beyond pushing the right tag.

What's different from a stable release:

- **No MSI.** WiX rejects a non-numeric pre-release version identifier
  ("optional pre-release identifier in app version must be numeric-only ...
  for msi target"), so the Windows build leg passes `--bundles nsis` to
  `tauri build` for these tags — NSIS only, MSI skipped. The asset count
  `promote` checks for is **8**, not 9 (stable minus the `.msi`).
- **`prerelease: true`** on the GitHub release, set at creation and never
  flipped back.
- **`make_latest: false`** when `promote` publishes it — a beta must never
  become the release GitHub's `latest` API resolves, since the README's
  `install.sh` / `install.ps1` one-liners resolve `latest` and must keep
  landing users on the newest **stable** build.
- **`publish-npm` doesn't run at all** (job-level `if`) — the npm launcher
  (`npx loomux-desktop`) stays pinned to the latest stable version; there's
  no npm-side "prerelease" concept to keep a beta installable-but-not-default,
  so the simplest correct behavior is not publishing it.
- Release notes still apply to the canonical `release_id` exactly as in step
  4 — pre-release doesn't change how or where notes get written, only the
  content (skip mentioning the `.msi` download).

Stable tags (no hyphen) are unaffected by any of the above — same asset
count (9), same `make_latest` (default `true`), same npm publish.
