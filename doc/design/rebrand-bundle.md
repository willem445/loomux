# The bundle identity: the binary and the identifier (#1562)

`doc/design/rebrand-external.md` (#1153 phase 5) renamed everything a stranger
types or downloads, and closed with a table of what it deliberately left alone.
Two rows of that table are the two roots this note is about, and both were left
for the same honest reason: each one keys something on a user's machine that the
rename cannot reach into.

| Root | Before | What it derives |
| --- | --- | --- |
| `src-tauri/Cargo.toml` `[package] name` | `loomux` | the executable (`…\Orrerix\<name>.exe`, `Orrerix.app/Contents/MacOS/<name>`, `/usr/bin/<name>`), `target/release/<name>.pdb`, WER dump names `<name>.exe.<pid>.dmp`, the `--webview-exe-name=<name>.exe` argument WebView2 passes its browser process, and `cargo … -p <name>` |
| `src-tauri/tauri.conf.json` `identifier` | `dev.loomux.app` | the WebView2 / WebKitGTK profile dir `<data_local_dir>/<identifier>`, macOS `CFBundleIdentifier` (and therefore the TCC microphone grant), the NSIS uninstaller's "delete app data" target |

They are independent, and they moved in two slices: the binary in slice A, the
identifier in slice B. This note carries both, because the questions a reader
arrives with — *what is my exe called, what happens when I upgrade, why is that
thing still called loomux* — do not split along that line.

## Why the cargo package, and not `mainBinaryName`

`mainBinaryName` is unset, and the Tauri v2 config schema says what that means:
Tauri "uses the output binary from cargo". So the cargo package name **is** the
executable's name, everywhere, with no second mechanism in the middle.

Setting `mainBinaryName: "orrerix"` instead would have produced one name in some
places and another in others:

- it is a build-time `fs::rename` of the exe (`tauri-cli`'s
  `interface/rust/desktop.rs::rename_app`) that runs in `build()` only — so
  `tauri dev` would still produce `loomux.exe` while a bundled build produced
  `orrerix.exe`;
- it renames the **exe only**, not `loomux.pdb`, so every symbolication recipe
  would become "drop `loomux.pdb` beside `orrerix.exe`";
- and it would have needed the same launcher, CI and docs edits anyway.

"Two names for one binary" is precisely the shape #1294 came from. Renaming the
cargo package leaves one name and no rename step whose scope has to be
remembered.

**What stays `loomux`, and why.** The `[lib] name = "loomux_lib"`, the
`loomux-engine` and `loomux-server` crates, `loomux_shim_sh`/`loomux_shim_cmd`
and every Rust/TS identifier. These are internal Rust identity: no user-visible
artefact carries them, ~20 `loomux_lib::` sites in tests and comments would
churn for nothing, and the binary rename does not touch them. That is a
deliberate scope line, not an unfinished sweep.

## The lockstep guard

The binary name has no single source at build time — it is `[package] name`, and
several files then spell it out by hand. Each of those fails in a different
workflow at a different time, and none of them says "you renamed one thing":

- `ci.yml`'s E2E job launches a path that no longer exists;
- `symbolicate.yml` builds `-p <old>`, a package cargo does not have;
- `release.yml` zips a PDB that is not there;
- `scripts/check-versions.js` stops finding the lockfile entry it reads the
  release version out of.

`test/bundleidentity.test.ts` is the half-rename detector. It reads
`[package] name` and runs two instruments over the surfaces:

- **named sites** — one row per construct that spells the name, so a stale one
  fails with a message naming it;
- **a default-deny shape scan** — every `<token>.exe` / `<token>.pdb` in those
  files must be the binary name or sit on an argued `ALLOW` row. It decides on
  the shape of the token rather than on the name of the binding around it, so a
  rename cannot step over it, and a stale `ALLOW` row is itself asserted.

It is deliberately green in both consistent states (all `loomux`, all
`orrerix`): it polices agreement, not a spelling, which is what let it be
written before the rename and survive it. Its own control runs the same scan
against a name the tree does not use and asserts both instruments report
findings — an absence is what a scan that examined nothing also produces.

The one exemption that is derived rather than typed is the launcher's:
`npm/bin/orrerix.js` is the one file where a literal `loomux.exe` is correct
after the rename, and the scan takes that string from `LEGACY_MAIN_BINARY`
rather than hardcoding it, so the exemption is only ever as wide as what the
launcher actually probes.

## Upgrade behaviour: what the installers do on their own

**NSIS, the field case (beta3 → beta4).** tauri-bundler's `installer.nsi` keys
its uninstall entry on `productName`, which did not move, so a beta4 install
finds beta3's key. That key records `MainBinaryName`, and the next install reads
it back:

```
  ; Remove old main binary if it doesn't match new main binary name
  ReadRegStr $OldMainBinaryName SHCTX "${UNINSTKEY}" "MainBinaryName"
  ${If} $OldMainBinaryName != ""
  ${AndIf} $OldMainBinaryName != "${MAINBINARYNAME}.exe"
    Delete "$INSTDIR\$OldMainBinaryName"
  ${EndIf}
```

So an in-place upgrade installs `orrerix.exe`, recreates its own shortcuts at
the new target, and deletes `loomux.exe` by itself. **The rename strands nothing
on the normal path, and no code was needed for it.**

**The one path it does not cover** is the old exe still *running*. That `Delete`
has no `/REBOOTOK`, and the bundler's own guard —
`!insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"` —
asks about the **new** name only. Install beta4 by hand (`setup.exe`,
`install.ps1`) while beta3 is open and the delete silently fails, leaving both
executables in `$INSTDIR` until the next install.

`src-tauri/windows/hooks.nsh` closes that, wired in via
`bundle.windows.nsis.installerHooks`: the same macro, applied to the previous
name, at `NSIS_HOOK_PREINSTALL` — which `installer.nsi` inserts at the top of
`Section Install`, before its own check and before any file is copied. Three
lines of NSIS under a comment explaining them, and the name it hardcodes is this
product's own previous binary rather than a toolchain assumption, so it stays
inside the "no machine-specific special-casing" rule.
It can be deleted once no supported upgrade path starts from a pre-#1562 build.

Two things about it are worth stating because nothing in CI will tell you:
`ci.yml` builds with `--no-bundle`, so **NSIS never runs in CI** and a broken
hooks path would first surface in a release build. `test/bundleidentity.test.ts`
therefore pins that the file resolves and that it guards the *previous* name —
pointed at the current one it would be a duplicate of a check that already runs,
with the stranding path open again and nothing red to say so. (The path is
resolved by `tauri-bundler` with `dunce::canonicalize` against the process cwd,
which `tauri-cli`'s `setup()` sets to the tauri dir before the bundler runs, so
a relative `installerHooks` is relative to `src-tauri/`. Read off
`tauri-cli-v2.11.4`, the tag `package-lock.json` pins.)

**Taskbar pins dangle.** A user-pinned shortcut points at
`…\Orrerix\loomux.exe` and the installer only recreates the shortcuts it made
itself. Unavoidable; it is a release-note line, not a bug to fix.

**MSI (stable releases only — betas ship `--bundles nsis`).** With
`bundle.windows.wix.upgradeCode` unset, `tauri-bundler` derives it as
`Uuid::new_v5(&Uuid::NAMESPACE_DNS, format!("{}.exe.app.x64", &settings.product_name()).as_bytes())`
— off `productName`, so #1153 phase 5 already changed it and the binary rename
does not touch it at all. Consequences: an Orrerix MSI over an Orrerix MSI is a
clean major upgrade and the binary rename strands nothing; an Orrerix MSI over a
**Loomux** MSI installs side by side, exactly as NSIS does.

**Pinning `bundle.windows.wix.upgradeCode` to the Loomux-derived GUID is
deliberately NOT done here.** It would make a stable Orrerix `.msi` remove a
stable Loomux `.msi` install on Windows Installer's own terms, which is the
argument for it — but its only effect is at a stable release, betas cannot
exercise it, and it is the kind of change whose first real test is a user's
machine. It is deferred to the stable-release decision, which is the human's.

## The identifier, and the asymmetry in its migration

*(Slice B. Recorded here so the two axes stay in one place; see #1562 for the
implementation.)*

The identifier keys exactly one directory this app depends on:
`<data_local_dir>/<identifier>`, holding WebView2's `EBWebView` profile. #1205
did not cover it — that migration moved `dirs::data_dir()/loomux`, the
**roaming** base, and the profile is under `data_local_dir()`. Everything else
orrerix persists is under `obs::data_root()` and already moved.

What a user would miss if it were flipped with no migration: `localStorage` —
the launcher's recent-repos list, the default agent, the custom agent command,
the editor command, git-view divider sizes, side-dock prefs. (Tabs already moved
out into `tabs.json` for exactly this reason.)

So the profile moves once, on #1205's rule — with **one deliberate asymmetry**
that is the whole reason this is not a copy of the data-root migration:

> **A refused rename does not fall back to the legacy directory.** For the data
> root, `UseLegacy` is the safe arm — "all my groups are gone" otherwise. For the
> webview profile it is the unsafe one: pointing the new build's
> `data_directory` at the old folder puts it in the **same WebView2 browser
> process as the still-running old build**, which is the #394 hazard the E2E
> identifier split exists to avoid. So the refused arm is "fresh profile", and
> the cost is a one-time reset of those localStorage prefs for a user who
> launches the new build while the old one is still open.

macOS has no Tauri-managed path here at all — WKWebView stores under
`~/Library/WebKit/<CFBundleIdentifier>` — so it takes a documented one-time
prefs reset plus a fresh microphone (TCC) grant on first voice use, rather than
a migration.

## What is deliberately still `loomux` after both slices

| | Why |
| --- | --- |
| `[lib] name = "loomux_lib"`, `loomux-engine`, `loomux-server` | Internal Rust identity; no user-visible artefact carries them. A separate issue if anyone ever wants the internal axis renamed. |
| `loomux_shim_sh` / `loomux_shim_cmd` and every Rust/TS identifier | Same axis. The shim writes the script under **both** command names for a different reason — a stale global `loomux-desktop` install on PATH — which `rebrand-external.md` covers. |
| `LOOMUX_*` env vars in CI and tests | Documented as no-contract in `rebrand-filesystem.md`. |
| The `loomux-mq` git namespaces | Argued in `rebrand-protocol.md`. |
| Every `LEGACY_*` read path, and `.loomux/` discovery | Accept-every-spelling by design (`rebrand-protocol.md`): dropping an accepted spelling is a silent regression, never a compile error. |
