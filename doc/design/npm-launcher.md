# The npm launcher: what `orrerix` on your PATH is allowed to do

`npm/bin/orrerix.js` is the whole of the `orrerix` package. Orrerix is a
native Tauri app, so the package ships no binary — the launcher fetches the
right GitHub release asset for the host, installs or caches it, and launches it.

It is under 600 lines of dependency-free Node, and almost all of the design in
it exists to answer one question: **when is this program allowed to run an
installer?** Getting that wrong is not a cosmetic bug. The Windows and macOS
installers are silent, and a silent install terminates the running app
to replace its files — taking down the app, every pane, and every agent working
inside it, mid-task, with no shutdown path and no crash report. That is #815, as
it actually happened.

## Plain `orrerix` never installs over an existing install (#845)

The original launcher auto-updated: it compared the installed app's version
against its own on every run and reinstalled on any mismatch. Two failure modes
came out of that, and they compound.

The first is that "any mismatch" is not "older". A stable launcher left on PATH
saw a newer prerelease install as a difference and reinstalled it — a
*downgrade*, announced as an upgrade. #815 fixed the ordering (see below), but
the ordering fix only narrowed the window.

The second is the real one: **an update is a decision, and it was being made by
whatever process happened to type `orrerix`.** A user launching the app is not
consenting to have it replaced, and there is no ordering rule clever enough to
make an unrequested install safe when the install kills a live session.

So the command surface splits the two intents:

```
orrerix            launch the installed app; install only if there is nothing to launch
orrerix update     install/refresh — the only path that fetches when something exists
orrerix version    print the launcher's version
orrerix help       usage
```

The launch-or-install decision is one pure exported function, `planAction`,
rather than an `existing && !force` repeated in each platform runner. Three
copies of a safety rule are three places to get it wrong and none of them are
testable; one function is pinned by `test/launcher.test.ts` in both directions —
a plain launch over an existing install must launch, and an `update` over an
existing install must install.

`orrerix --reinstall` survives as a deprecated alias, and the launcher says so on
stderr when it is used: the flag's *meaning* changed (it used to install the
release matching the launcher's own version), so a script that relied on the old
behaviour deserves to be told rather than silently rerouted.

## `orrerix update` never reads GitHub's `latest` pointer (#846)

The obvious implementation of "install the latest release" is
`GET /repos/:owner/:repo/releases/latest`. It is wrong here, and not marginally.

`/releases/latest` does not mean "the newest stable release". It resolves the
repository's mutable `make_latest` pointer, which is set at publish time and can
simply fail to move. On this repo it *has* failed to move: the pointer still
resolves `v0.10.0` (published 2026-07-16) while the newer, non-prerelease
`v1.0.0` (2026-07-27) sits above it — the promote-time failure documented at
length in `.github/workflows/release.yml` (#341/#543), where `v1.0.0` published
cleanly but never took the pointer.

Resolving an update through that endpoint downgrades every 1.x user: eleven
releases back for a `1.1.0-beta11` install, and still a downgrade for a stable
`v1.0.0` one. And it fails *silently* — it finds a release, picks an asset,
downloads it, and runs the installer, announcing an update the whole way.

So the launcher enumerates `/releases?per_page=100` and orders the results
itself, with the semver comparator from #815. The pointer is never consulted on
this path. One page is deliberate: the API caps `per_page` at 100 and this repo
has 24 releases; the newest of each channel can only fall off page 1 behind more
than 100 consecutive prereleases.

## Channel, not "newest overall"

Betas on this repo are marked `prerelease` and deliberately never become
`latest`. That makes "newest overall" wrong for a stable user — it would push a
beta at someone who never opted into one — and "newest stable" wrong for a beta
user, because this repo's newest build is *always* a prerelease, so a
stable-only rule makes `update` a permanent no-op for the entire beta train.

`update` therefore stays on the channel of the build that is actually installed:

| Installed | `orrerix update` resolves |
| --- | --- |
| stable (`1.0.0`) | newest **stable** release |
| prerelease (`1.1.0-beta11`) | newest release of **either** kind |

Channel is read from the *installed* app, not from the launcher, because the
installed app is what an installer would overwrite. Each platform probes what it
can:

- **Windows** — the NSIS `DisplayVersion`, in all three places an install can
  record it: `HKCU` for a per-user install (Tauri's default), and `HKLM` in both
  the native (`reg query /reg:64`) and WOW6432Node (`/reg:32`) views for a
  per-machine one. `findWindowsExe` has always looked under `%PROGRAMFILES%`,
  which *is* a per-machine install, so probing only HKCU left a whole install
  class unreadable. When more than one is found the **newest** wins: if any
  install on the machine is newer than the resolved release, installing that
  release downgrades it, so a stale per-machine leftover must not unblock a
  downgrade of a newer per-user install.
- **macOS** — `CFBundleShortVersionString` from the bundle's `Info.plist`.
- **Linux** — the cached AppImage's filename. There is no installer and no
  registry, so `Orrerix_1.3.0_amd64.AppImage` is the only version record
  that exists.

## Unknown is not safe (`updateBaseline`)

The probe has three outcomes, and the difference between the last two is the
whole guard:

| Situation | Ordered against |
| --- | --- |
| nothing installed | this launcher's version |
| version detected | that version |
| **installed, version unreadable** | **nothing — `update` refuses** |

The first is safe by construction: there is nothing to downgrade when there is
nothing there, and a first install still needs a channel to pick.

The third used to substitute the launcher's version as well, and that quietly
disarmed the entire guard for anyone the probe could not read. Concretely: a
1.1.0-beta11 install placed per-machine (HKLM, unreadable under an HKCU-only
probe) with a stale 0.10.0 launcher on PATH. The unknown became "0.10.0", which
selected the **stable** channel, which resolved v1.0.0, which installed over the
beta — a downgrade *and* a channel switch, with the no-downgrade comparison
passing because it was comparing the launcher against itself.

So an install whose version cannot be determined stops the update with a message
saying why and what to do instead. This is the one place where refusing is
plainly worse UX than proceeding, and it is still correct: "I cannot tell an
update from a downgrade" is exactly the condition the guard exists for, and a
guard that treats its own blind spot as a pass is not a guard.

Switching channels is a human action — install the build you want from the
releases page. The launcher will not do it for you in either direction.

## Never downgrade — a refusal, not a warning (#816)

Channel selection alone is not the guard. `updateVerdict` compares the resolved
release against the installed version and returns one of four actions:

- `install` — a genuinely newer build on this channel;
- `reinstall` — the newest build *is* what is installed (this is the repair case
  `--reinstall` always meant, and it is not a downgrade);
- `refuse` — the newest build on this channel is **older** than what is
  installed; the launcher dies with an explanation and installs nothing;
- `none` — no orderable release on this channel at all.

(A fourth refusal sits in front of all of these: an install whose version cannot
be read never reaches the verdict at all — see `updateBaseline` above.)

`refuse`, not "warn and continue": #815 was a downgrade that announced itself as
an upgrade and killed a running app, and fixing the endpoint only removes
*today's* instance. Every other route back to it is still open — a stale
launcher on PATH, a re-pointed `make_latest`, a yanked release. The guard sits on
the verdict so it covers all of them.

The comparator is worth one note. Semver says alphanumeric prerelease
identifiers compare as ASCII, which puts `beta10` *below* `beta9` ("1" < "9") —
on a repo that has shipped eleven `1.1.0-beta*` tags, that is a downgrade wearing
an upgrade's clothes. `compareIdentifier` compares each identifier as alternating
runs of digits and non-digits, digits numerically. Plain semver forms are
unaffected.

## The running-instance guard

Independent of versions: `refuseIfRunning()` refuses to install while the app is
running, **including** under `orrerix update`. An explicit update is a request to
reinstall, not consent to kill a live instance; quitting first is the user's call.

The probe is `tasklist` on Windows and `pgrep` on macOS. An unknown answer (no
probe, probe failed) is reported as "not running" on purpose: both probes ship
with the OS, so a failure means something exotic, and a launcher that refuses to
install on such a machine is a worse bug than one that installs.

Linux has no probe — the AppImage runs in place and nothing is replaced under it.
The one exception is `update` rewriting the *same* cached AppImage while it is
running, which the kernel rejects with `ETXTBSY`; `download()` translates that
errno into the same advice the other two platforms give rather than letting a raw
`Error: ETXTBSY` reach the user.

## Agents never run this

`orrerix` on an agent pane's PATH is shimmed to refuse unconditionally, and so is
the pre-rename `loomux` a stale global install leaves behind
(`orchestration::loomux_shim_sh` / `loomux_shim_cmd` — Rust symbols on the cargo
axis, which #1153 does not rename). There is no grant path, no
delegation and no fallback — unlike the `gh`/`git` gates, there is no authorized
agent use of the launcher at all, because agents reach Orrerix through its MCP
tools. The shim's refusal message describes what the launcher does, so it is
pinned by a test: a message that goes stale is a false claim in the one place an
agent is guaranteed to read.

## The rename (#1153 phase 5)

The package and its command became `orrerix`, with no `loomux-desktop` shim. The
launcher is the one program that has to work across that flip in BOTH directions
— an old install under a new launcher, and a pre-rename release under a launcher
nobody has updated — so it follows `rebrand-protocol.md`'s rule verbatim: emit one
spelling, accept every spelling on every reading surface, write the accepted set
down exactly once (`PRODUCT_NAMES`, `CLI_NAMES`, `EXE_NAMES`).

Two things a reader of this file needs and cannot infer from the code alone:

- **Release-asset resolution needed no fallback and gained none.** The patterns
  were always end-anchored suffixes (`-setup\.exe$`, `_amd64\.AppImage$`), which
  are indifferent to the product-name prefix in both directions. They are now one
  pure `assetPattern(platform, arch)`, pinned against both spellings.
- **The product name and the binary name are different strings** — the bundle is
  `Orrerix.app` and the executable inside it is `loomux`, because `mainBinaryName`
  is unset and Tauri then takes cargo's output. The install and running-app probes
  read one of each (#1294).

Full argument, including why an Orrerix install lands *beside* a Loomux one
rather than replacing it: `doc/design/rebrand-external.md`.
