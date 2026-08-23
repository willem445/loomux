# The external identity: `loomux` → `Orrerix` (#1153 phase 5)

Phase 4 renamed what sits on **our** disk. Phase 3 renamed what **agents** match on.
This phase renames what **strangers** type and download: the npm package, the command
it installs, the app's own name, and the filename of every release asset.

It is the last build phase of the rename, and it is the first one where the thing
being renamed is already published under the old name to people we cannot reach.

## The two axes, and why one of them did not move

The single most expensive misreading available in this phase is that "the app's name"
is one string. It is two, they are set by different config fields, and Tauri uses each
for different files **inside the same path**.

| | Config field | Value here | What it names |
| --- | --- | --- | --- |
| **Product** | `productName` | `Loomux` → **`Orrerix`** | the macOS bundle `Orrerix.app`; the Windows install dir `%LOCALAPPDATA%\Orrerix`; the Add/Remove key `…\Uninstall\Orrerix`; every asset filename `Orrerix_<version>_<arch>.<ext>` |
| **Main binary** | `mainBinaryName` | *unset* → `loomux` | the executable itself: `…\Orrerix\loomux.exe`, `Orrerix.app/Contents/MacOS/loomux` |

`mainBinaryName` is unset, and the Tauri v2 config schema says what that means:

> Overrides app's main binary filename. By default, Tauri uses the output binary from
> `cargo` […]

Cargo's output is the `loomux` crate in `src-tauri/Cargo.toml`, which this phase does
**not** rename — the crate name is internal, it is what `-p loomux`,
`target/release/loomux.pdb` and `symbolicate.yml` all refer to, and nothing outside the
repo ever sees it.

So `Orrerix.app` contains an executable called `loomux`, and that is correct rather
than an oversight. Two consequences fall straight out of it, and #1294 is what happens
when they are missed:

- **The Windows install probe needs both names in one path** — the product for the
  directory, the binary for the file. `%LOCALAPPDATA%\Orrerix\Orrerix.exe` exists
  nowhere.
- **The macOS running-app probe needs the binary name.** `pgrep -x` matches the process
  name case-sensitively, and the process is `loomux`. A probe spelled `Loomux` matched
  nothing there for as long as it existed, which meant `update` was free to delete a
  running `/Applications` bundle. Windows had been getting away with the same confusion
  only because `tasklist`'s `IMAGENAME` filter and NTFS are case-insensitive — and
  `Orrerix` vs `loomux` no longer differ merely by case, so the rename would have taken
  that accident away too.

`npm/bin/orrerix.js` keeps the two as separate arrays (`PRODUCT_NAMES`, `EXE_NAMES`) and
crosses them where a path needs both.

## Release assets: the fallback that was not needed, and why that is the design

Every asset filename starts with the product name, so #1153 changed the prefix of every
asset from the first post-rename release onward. The obvious response is a fallback
list — try `Orrerix_*`, then `Loomux_*`.

All three resolvers already avoid needing one, because none of them ever matched the
prefix. `install.ps1` matches `*-setup.exe`; `install.sh` matches `_x64\.dmg$`; the npm
launcher matches `-setup\.exe$`, `_aarch64\.dmg$`, `_amd64\.AppImage$`. Every one is an
**end-anchored suffix**, and a suffix is indifferent to the brand in both directions:
a post-rename launcher installs a pre-rename release, and a launcher nobody has updated
installs a post-rename one.

That property is worth naming rather than leaving as a coincidence of three inline
regexes, because it is the thing that makes a rename cost nothing here — so the npm
launcher's patterns are now one pure `assetPattern(platform, arch)`, pinned by a test
that resolves the same five assets under both spellings. A fallback list would have been
strictly worse: something to keep in sync, on a path nobody exercises until a user on an
old pin needs it.

The suffixes stay tight enough to exclude the assets that share the family's *shape*.
`Orrerix_1.3.0_x64.pdb.zip` (release.yml's "House style" note) ends `.zip`, so it matches
neither `-setup.exe` nor `_x64.dmg`, and the test asserts that a release carrying only
the symbols zip resolves to nothing on all three platforms.

One asset name is written by hand and does have to be kept in step: that same
`.pdb.zip`, built by a `release.yml` step rather than by the bundler. Its **source** path
stays `target/release/loomux.pdb` — that is the cargo axis, not the product one.

`install.sh` is the one script that could not stay name-free. It always resolves
`/releases/latest`, so it straddles the rename: it now installs whichever `.app` the
mounted disk image actually carries, rather than a hardcoded bundle name.

## Side by side, and why nothing here hides it

**Installing Orrerix does not replace an existing Loomux install. Both apps end up on
the machine.**

This is a direct consequence of the product rename, not a bug and not something the
launcher can paper over. `tauri-bundler`'s NSIS template defines

```
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}"
```

and finds an existing install by reading that key. A renamed product has no key to find,
so it installs fresh: a second entry in Add/Remove Programs, a second install directory,
a second Start-menu shortcut. macOS is the same story with `/Applications/<product>.app`.
Linux is unaffected — an AppImage is one file, and the one on `PATH` is replaced.

The alternatives were considered and rejected:

- **Keep `productName` as `Loomux`** — defeats the phase. The taskbar entry, the
  Start-menu name and the Add/Remove entry are the most visible names the product has.
- **Have the launcher uninstall the old app** — running someone's uninstaller without
  asking is a much worse thing to do than leaving an app they installed on their disk.
  It is also unbounded: nothing tells us the old install is not the one they are
  deliberately keeping.

So the design is to make it **visible and harmless** instead:

- The launcher **prefers the new install and still finds the old one**. Its accepted
  sets are ordered current-spelling-first, so plain `orrerix` launches Orrerix when both
  are present, while `orrerix update` can still read a Loomux install's version — which
  is what keeps #816's downgrade guard armed. A launcher that saw only the new spelling
  would report "nothing installed" for every pre-rename user, and `updateBaseline` treats
  that as safe to order against the launcher's own version: the guard disarmed by a
  rename.
- **Nothing is deleted, anywhere.** `install.sh` reports a leftover `~/.local/bin/loomux`
  rather than removing it. The launcher's AppImage cache is renamed but never moved,
  because a cached AppImage may be the running process.
- **The docs say it out loud**, with the uninstall commands, in
  `docs/getting-started.md` — under its own heading, because a user who finds two apps
  and no explanation concludes the installer is broken.

No user data is duplicated: both builds read the same profile root, which moved in
phase 4 and is not keyed on the product name.

## What this phase deliberately does not touch

| | Why |
| --- | --- |
| The GitHub repo slug (`willem445/loomux`) | A human button, coordinated separately, and its new value is not recorded anywhere this branch can read — #1153 calls the repo rename "a human-only action" and never names the target. Changing it speculatively would break things *before* the rename for no gain. Which of the ~73 in-tree occurrences are free to lag and which are not is classified in the runbook below; `test/reposlug.test.ts` is what stops a partial rename from shipping. |
| npm trusted-publishing config | A human button on npmjs.com, and a security-relevant one. `release.yml` already reads `PKG` out of `package.json` rather than hardcoding it, so the workflow needed no edit for the package rename. It does need the runbook's step 2 to have happened first. |
| The bundle identifier `dev.loomux.app` | It keys the WebView2 user-data folder and the macOS bundle ID. Moving it orphans every user's webview profile, and no one outside the repo ever sees it. |
| Cargo crate names (`loomux`, `loomux_lib`, `loomux-engine`, `loomux-server`) | Internal. `symbolicate.yml`, `ci.yml`'s E2E exe path, and the `.pdb` filename all name the cargo axis, and none of them is an external identity. |
| Internal prose still saying "Loomux" in `src/` comments | Phase 2's surface, not this one. |

## Where `loomux-desktop` actually went

Not "frozen at its last published version" — **fully unpublished**, by hand, on
2026-08-08. Measured against the registry rather than remembered:

```
$ curl -s https://registry.npmjs.org/loomux-desktop | …
dist-tags: null
versions: 0
unpublished: { when: 2026-08-08T03:47:28.064Z, count: 11 }   # every version
```

Three consequences the rest of this note depends on:

- **There is nothing to deprecate.** `npm deprecate` acts on published versions and
  there are none, so the "we can always deprecate it later" fallback does not exist.
  The stronger action was already taken.
- **The trusted-publisher binding went with the package.** A binding is per-package;
  unpublishing removed the only one this project had. So step 2 below is not
  *re-pointing* an existing binding, it is creating the first one.
- **Nothing is installable under either name right now.** `loomux-desktop` is gone and
  `orrerix` does not exist yet (`registry HTTP 404`), which is exactly why the runbook
  needs a publish step rather than only a binding step.

A user who ran `npm install -g loomux-desktop` before the unpublish still has the
`loomux` command on their machine — an unpublish removes the package from the registry,
not from anyone's disk. That is why the docs still tell them to uninstall it, and why
the self-launch shim still refuses `loomux` as well as `orrerix`.

## The human runbook

Four steps, strictly ordered. Step 2 is the one that is easy to leave out, and without
it step 4 fails.

### 1. Rename the GitHub repo

`willem445/loomux` → the new slug. GitHub redirects almost everything, but not quite
everything — from GitHub's own docs on renaming a repository:

> All existing information, **with the exception of project site URLs**, is
> automatically redirected to the new name

So the ~73 in-tree occurrences of the slug fall into three classes, and only the first
is free:

| Class | Sites | Does the rename break it? |
| --- | --- | --- |
| `github.com/willem445/loomux/…` links | 57, in docs and design notes | **No.** Redirected. Sweep at leisure. |
| `willem445.github.io/loomux/…` links, plus `docs/_config.yml`'s `baseurl: /loomux` | 14 + 1 | **Yes, immediately.** Project site URLs are the documented exception, so every one of these 404s the moment the repo is renamed. They move in the same change. |
| `npm/package.json`'s `repository.url` (and `homepage`, `bugs`) | 3 | **Yes, at publish time.** See step 3. |

GitHub's docs carry one further exception — *"GitHub will not redirect calls to an
action hosted by a renamed repository"* — which does not apply here: no workflow in
this repo `uses:` an action hosted in it. Checked, not assumed.

`test/reposlug.test.ts` fails until every one of those sites names the same slug, so a
half-done rename cannot ship quietly. That test is the reason this list does not have
to be remembered.

### 2. Publish `orrerix` to npm once, by hand

**Trusted publishing cannot create a package — it can only be attached to one that
already exists.** From `npm trust`'s own prerequisites:

> The package you're configuring must already exist on the npm registry

and npm/cli#8544, *"Allow publishing initial version with OIDC"*, is still open. So
the first `orrerix` version has to be published with ordinary credentials:

```sh
npm login                     # your own account; 2FA must be on (npm trust requires it)
git checkout main && git pull # a clean tree at the version you are about to release
cd npm && npm publish --access public
```

**Do this once the bump PR for the next stable release has merged**, so the version
being hand-published is exactly that release's version. Two reasons, and the second is
the trap:

- `latest` then points at the launcher that matches the release, rather than at a beta
  or a placeholder. (`resolveRelease` prefers the release tagged `v<launcher version>`,
  so a beta launcher on `latest` would hand every new user a beta app.)
- `publish-npm` skips when `npm view "$PKG@$VERSION"` already resolves
  (`release.yml`). Hand-publishing that same version therefore makes that release's
  automatic publish a deliberate no-op — the run stays green and says
  `already published — nothing to do`. Hand-publishing a *different* version instead
  would leave the automatic publish live on a binding that does not exist yet, and the
  release would fail at `npm publish` with `ENEEDAUTH`.

One visible difference: a manual publish generates **no provenance attestation**. npm
generates those automatically only for a trusted-publishing publish, so version one of
`orrerix` will lack the badge and every later version will have it.

### 3. Bind the trusted publisher for `orrerix` to the new slug

On npmjs.com, or `npm trust github orrerix --repository <owner>/<new-slug>`.

**Pass `--repository` explicitly, or fix `npm/package.json` first.** npm falls back to
the manifest when the flag is omitted — *"If a provider is repository-based and the
option is not provided, npm will use the `repository.url` field from your
`package.json`"* — and until step 1's edit lands that field still names the OLD slug,
so an unflagged bind creates exactly the mis-pointed binding this ordering exists to
avoid.

The same field is checked at publish time, from npm's trusted-publishing
troubleshooting:

> To publish from GitHub, your package's `repository.url` field in `package.json` must
> exactly match your GitHub repository.

This is why `repository.url` is **not** in the "redirects, so it can lag" class. A
redirect satisfies a browser; it does not satisfy an exact-match check.

### 4. The first OIDC release

`publish-npm` runs on non-hyphenated tags only, so no beta/RC will exercise the new
binding. Because step 2 hand-published the next stable version, that release's publish
is the intended no-op — **the release after it is the first real OIDC publish, and the
one to watch.** Verify either with `npm view orrerix version`, and the provenance badge
on npmjs.com for the OIDC one.
