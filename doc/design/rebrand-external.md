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
| The GitHub repo slug (`willem445/loomux`) | A human button, coordinated separately. GitHub serves permanent redirects for a renamed repo on the REST API and on release-asset downloads, so every hardcoded slug keeps working on both sides of it. Changing it speculatively would break things *before* the rename for no gain. |
| npm trusted-publishing config | A human button on npmjs.com, and a security-relevant one. `release.yml` already reads `PKG` out of `package.json` rather than hardcoding it, so the workflow needed no edit at all. |
| The bundle identifier `dev.loomux.app` | It keys the WebView2 user-data folder and the macOS bundle ID. Moving it orphans every user's webview profile, and no one outside the repo ever sees it. |
| Cargo crate names (`loomux`, `loomux_lib`, `loomux-engine`, `loomux-server`) | Internal. `symbolicate.yml`, `ci.yml`'s E2E exe path, and the `.pdb` filename all name the cargo axis, and none of them is an external identity. |
| Internal prose still saying "Loomux" in `src/` comments | Phase 2's surface, not this one. |

## The human runbook

Strictly ordered — step 2 cannot be done before step 1, and step 3 is what actually
publishes.

1. **Rename the GitHub repo** `willem445/loomux` → the new slug. Redirects keep the
   hardcoded slugs in `install.sh`, `install.ps1` and `npm/bin/orrerix.js` working, so
   this can happen before or after this PR merges. Updating those three lines afterwards
   is cosmetic.
2. **Bind the npm trusted publisher for `orrerix` to the new slug** on npmjs.com. The
   existing binding names `loomux-desktop` on the old slug and grants nothing to the new
   package; publishing a *new* package name over OIDC requires the binding to exist
   first. Do this after the repo rename so the binding is created against the final slug
   and never has to be re-pointed.
3. **The next stable tag publishes it.** `publish-npm` runs on non-hyphenated tags only,
   so a beta/RC tag will not exercise the new binding — the first stable release after
   this is the one that proves it. Verify with `npm view orrerix version`.

`loomux-desktop` is left frozen at its last published version. There is no deprecation
shim, by decision: an npm `deprecate` message is the lighter-weight option available to
the human at any time and needs no code.
