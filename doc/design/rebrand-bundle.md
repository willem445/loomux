# The bundle identity: the binary and the identifier (#1562)

`doc/design/rebrand-external.md` (#1153 phase 5) renamed everything a stranger
types or downloads, and closed with a table of what it deliberately left alone.
Two rows of that table are the two roots this note is about, and both were left
for the same honest reason: each one keys something on a user's machine that the
rename cannot reach into.

| Root | Value before #1562 | What it derives |
| --- | --- | --- |
| `src-tauri/Cargo.toml` `[package] name` | `loomux` | the executable (`…\Orrerix\<name>.exe`, `Orrerix.app/Contents/MacOS/<name>`, `/usr/bin/<name>`), `target/release/<name>.pdb`, WER dump names `<name>.exe.<pid>.dmp`, the `--webview-exe-name=<name>.exe` argument WebView2 passes its browser process, and `cargo … -p <name>` |
| `src-tauri/tauri.conf.json` `identifier` | `dev.loomux.app` | the WebView2 / WebKitGTK profile dir `<data_local_dir>/<identifier>`, macOS `CFBundleIdentifier` (and therefore the TCC microphone grant), the NSIS uninstaller's "delete app data" target |

They are independent, and they move in two slices. **Slice A — the binary — has
landed, and is what the "Before" column above describes leaving behind. Slice B
— the identifier — has NOT: `tauri.conf.json` still reads
`"identifier": "dev.loomux.app"`, and the section on it near the end of this
note is a design recorded ahead of its implementation, not a description of
shipped behaviour.** This note carries both anyway, because the questions a
reader arrives with — *what is my exe called, what happens when I upgrade, why
is that thing still called loomux* — do not split along that line.

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

  ; Save current MAINBINARYNAME for future updates
  WriteRegStr SHCTX "${UNINSTKEY}" "MainBinaryName" "${MAINBINARYNAME}.exe"
```

So an in-place upgrade installs `orrerix.exe`, recreates its own shortcuts at
the new target, and deletes `loomux.exe` by itself. **The rename strands nothing
on the normal path, and no code was needed for it.**

**The one path it does not cover** is the old exe still *running*. That `Delete`
has no `/REBOOTOK`, and the bundler's own guard —
`!insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"` —
asks about the **new** name only. Install by hand (`setup.exe`, `install.ps1`)
while the previous build is open and the delete silently fails, leaving both
executables in `$INSTDIR`.

**That stranding is permanent.** Read the second statement in the block above:
the `WriteRegStr` that records `MainBinaryName` sits **outside** the `${If}`,
and is not conditioned on the `Delete` having succeeded. So the very install
that failed to remove `loomux.exe` still records `MainBinaryName = orrerix.exe`.
Every later install reads that back, finds `$OldMainBinaryName` equal to
`${MAINBINARYNAME}.exe`, and never runs the `Delete` again — and that `Delete`
is the only thing in `Section Install` that removes pre-existing `$INSTDIR`
content. Once missed, it is missed for good: nothing on this axis will ever
retry it, because the name it would now be looking for is the one already
installed.

It outlives an uninstall, too. The uninstaller deletes
`"$INSTDIR\${MAINBINARYNAME}.exe"` — the *current* name — and then calls
`RMDir "$INSTDIR"` **without `/r`**, so a directory still holding `loomux.exe`
is not empty and is not removed. An uninstall leaves both
`%LOCALAPPDATA%\Orrerix\` and the stale exe behind.

The residual is therefore small but **not** self-healing: one un-launched file
(every shortcut points at the new exe, and the launcher probes the current name
first) that stays until the user deletes it. The mitigation is prevention, not
cleanup — quit the old build before a hand-install — and that is what
`docs/getting-started.md` tells the user.

### The NSIS `NSIS_HOOK_PREINSTALL` hook, considered and rejected

`bundle.windows.nsis.installerHooks` can insert
`!insertmacro CheckIfAppIsRunning "loomux.exe" "${PRODUCTNAME}"` before the
bundler's own check, which looks like a three-line fix for the residual above.
It was written, reviewed, and removed. **Do not re-add it** without answering
both of these:

1. **That macro does not refuse — it kills.** In `utils.nsh` at
   `tauri-cli-v2.11.4`, once `FindProcess{,CurrentUser}` matches,
   `CheckIfAppIsRunning` runs

   ```nsis
   ${If} $R0 = 0
       IfSilent kill_${UniqueID} 0
       ${IfThen} $PassiveMode != 1 ${|} MessageBox MB_OKCANCEL $R2 IDOK kill_${UniqueID} IDCANCEL cancel_${UniqueID} ${|}
       kill_${UniqueID}:
         !if "${INSTALLMODE}" == "currentUser"
           nsis_tauri_utils::KillProcessCurrentUser "${executableName}"
         !else
           nsis_tauri_utils::KillProcess "${executableName}"
         !endif
   ```

   (`$R0` is `FindProcess`'s result; `INSTALLMODE` is `currentUser` by default,
   which is ours), where `$R2` is
   `"{{product_name}} is running!$\nClick OK to kill it"`. So a silent or
   passive install terminates the running build with **no prompt at all**, and
   the interactive one offers killing as the OK action. `install.ps1:28` is
   `Start-Process -FilePath $dest -ArgumentList "/S" -Wait` — the silent path —
   so the documented one-line install would have terminated a running build,
   closing every pane and every agent session in it. That is the exact harm the
   launcher's own `refuseIfRunning` exists to prevent, and that one really does
   refuse: it calls `die()`.
2. **It matches by image name, and `loomux.exe` names two products.** It is the
   binary of a beta1–beta3 Orrerix install (`%LOCALAPPDATA%\Orrerix\loomux.exe`)
   *and* of a stable Loomux 1.0/1.1 install (`%LOCALAPPDATA%\Loomux\loomux.exe`)
   — an install this installer deliberately leaves alone (see the side-by-side
   argument below and in `rebrand-external.md`). Killing it is collateral damage
   on a product this installer will not otherwise touch. The bundler's own check
   is name-based too, but `${MAINBINARYNAME}.exe` names exactly one product;
   `loomux.exe` does not, and that asymmetry is what stops the macro being the
   like-for-like reuse it appears to be.

Weighed against one un-launched file that a user can delete — permanent, as the
section above establishes, but inert and visible only in `$INSTDIR` — neither
cost is worth paying. Killing a running app, possibly a *different product*,
with no prompt, is a far larger harm than a stale file on a path the user
already opened an installer against. A future version that genuinely needs this
wants a guard scoped to `$INSTDIR` (so a side-by-side install is out of reach)
*and* a refusal rather than a kill — which the bundler's macro does not offer,
so it would mean writing the NSIS by hand.

Note for whoever revisits it: **`ci.yml` builds with `--no-bundle`, so NSIS
never runs in CI.** Nothing on a PR compiles a hook file, and a mistake in one
first surfaces during a release build.

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

## The identifier, and the asymmetry in its migration — PLANNED, NOT SHIPPED

> **Nothing in this section has landed.** `src-tauri/tauri.conf.json` still reads
> `"identifier": "dev.loomux.app"`, no `init_webview_profile` exists, and no
> profile is migrated by any build you can install today. This is #1562's slice
> B, recorded here ahead of its implementation so the two axes stay in one
> place — every "moves", "is", and "does" below describes what slice B *will*
> do, and is a design decision to implement against, not behaviour to rely on.
> The tracking issue is #1562; if it is closed and this warning is still here,
> the warning is what is stale.

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

## What stays `loomux` on purpose, in both slices

| | Why |
| --- | --- |
| `[lib] name = "loomux_lib"`, `loomux-engine`, `loomux-server` | Internal Rust identity; no user-visible artefact carries them. A separate issue if anyone ever wants the internal axis renamed. |
| `loomux_shim_sh` / `loomux_shim_cmd` and every Rust/TS identifier | Same axis. The shim writes the script under **both** command names for a different reason — a stale global `loomux-desktop` install on PATH — which `rebrand-external.md` covers. |
| `LOOMUX_*` env vars in CI and tests | Documented as no-contract in `rebrand-filesystem.md`. |
| The `loomux-mq` git namespaces | Argued in `rebrand-protocol.md`. |
| Every `LEGACY_*` read path, and `.loomux/` discovery | Accept-every-spelling by design (`rebrand-protocol.md`): dropping an accepted spelling is a silent regression, never a compile error. |
