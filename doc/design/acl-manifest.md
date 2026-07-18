# Design: ACL manifest for app commands

Status: implemented (issue #363).

## Problem

Tauri's per-window/per-webview command ACL only activates when
`has_app_acl_manifest || !is_local`. loomux had no `src-tauri/permissions/`
directory, so `has_app_acl_manifest` was `false`, and every window is a
`local` origin — so `is_local` alone exempted every one of loomux's ~120
`#[tauri::command]`s from ACL, for every window, regardless of any
`capabilities/*.json` file's contents. Concretely: any script executing in
*any* webview context could `invoke` `orch_grant_merge`, `git_push`,
`ft_write_file`, `spawn_pty` — the full command surface — with no capability
check in the loop at all.

This was surfaced by the #360 pane-plugins Phase-0.5 isolation spike
(`spike/360-sandbox-proof`, findings on
[#360](https://github.com/willem445/loomux/issues/360#issuecomment-4992837152)):
a plugin webview bound to a capability with `"permissions": []` invoked
`git_push` exactly as freely as `main`, for this reason. It is the same root
cause — no ACL layer actually gating loomux's commands — already logged as
the "null-CSP / ~117-commands" fact on #189.

## What changed

`src-tauri/build.rs` now opts loomux into Tauri's app ACL manifest:

```rust
tauri_build::try_build(
    tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(APP_COMMANDS)),
)
```

This one flip makes `has_app_acl_manifest` true. **From that instant, every
command in `APP_COMMANDS` that isn't explicitly granted in `capabilities/` is
denied for every window — main included.** There is no partial state: the
flip, the full 120-command grant to `main`, and the coherence test that keeps
it that way all had to land in one PR (this one).

### The command list — single source of truth

`src-tauri/src/command_manifest.rs` defines `pub const APP_COMMANDS: &[&str]`,
the bare (unmodule-qualified) names of all 120 commands registered in
`src/lib.rs`'s `tauri::generate_handler![...]`. `build.rs` can't depend on the
compiled lib, so it pulls this file in with `include!("src/command_manifest.rs")`
instead of `use loomux_lib::command_manifest`; `lib.rs` also declares
`pub mod command_manifest;` so the same file is one audited list, not two.

Adding a command to `generate_handler!` without adding it here (or adding it
here without granting it) is exactly the failure mode the coherence test
exists to catch — see below.

### The grant map — main gets everything, grouped into tiers

**`main` is the only trusted webview.** It is the whole reason ACL wasn't
gating anything before this change, and it stays fully capable after: every
one of the 120 commands is reachable from `main`, because `main` is the
trusted UI and frontend code across `git.ts`, `pty.ts`, `orchestration.ts`,
`fileapi.ts`, `filemgr.ts`, `editor.ts`, `issues.ts`, and more legitimately
invokes across every module. There is no "should main have this" question —
registered means main may call it, so main is granted it. Tiering
(dangerous/sensitive/benign) is an organizing principle for readability and
for what a *future* non-main capability could selectively grant; it never
withholds anything from `main`.

Rather than 120 flat `allow-*` lines in `capabilities/default.json`, the
permissions are grouped into 14 module/tier **permission sets** under
`src-tauri/permissions/sets/*.toml` (Tauri's own convention — see e.g.
`tauri-plugin-dialog`'s `permissions/default.toml`), each a hand-authored
`[[set]]` referencing the individual `allow-<command>` permissions Tauri's
codegen generates:

| Set | Commands | Covers |
|---|---|---|
| `pty-read` | 3 | `pty_backend_info`, `discover_git_bash`, `dir_info` |
| `pty-control` | 5 | `spawn_pty`, `write_pty`, `kill_pty`, `change_dir`, `resize_pty` |
| `git-read` | 6 | repo root/log/status/diff/branches/worktree-list |
| `git-write` | 16 | commit/stage/checkout/discard/fetch/push/pull/merge/rebase/tag/… |
| `gh-read` | 5 | auth status, issue/PR list and view |
| `gh-write` | 4 | issue create/set-labels/comment, PR comment |
| `gitwatch` | 2 | `git_watch`, `git_unwatch` |
| `orch-read` | 14 | tasks, audit, session roles, autonomy/usage/summary, channel list, … |
| `orch-control` | 39 | create/bind/steer/approve/grant-merge/grant-release, task & channel mgmt, autonomy settings, group lifecycle, solo mode |
| `fileedit-read` | 5 | read file, list dir, search/list-files jobs |
| `fileedit-write` | 2 | `ft_write_file`, `ft_replace` |
| `filemgr-read` | 2 | `fm_list`, `fm_capabilities` |
| `filemgr-write` | 7 | new folder/file, rename, delete, open, open-with, reveal |
| `misc` | 10 | session listing, CLI probing, external editor, hashing, startup notice, UI tab state, voice |

`src-tauri/permissions/sets/main-ui.toml` aggregates all 14 into one name
(Tauri resolves a set's `permissions` list recursively, so a set can name
another set). `capabilities/default.json` grants `main` exactly:

```json
"permissions": ["core:default", "dialog:allow-open", "core:window:allow-destroy", "main-ui"]
```

— one name standing in for all 120 grants, auditable by reading
`main-ui.toml` → the 14 set files → `permissions/autogenerated/*.toml`
(Tauri-generated, one `allow-<cmd>`/`deny-<cmd>` pair per command, marked
`DO NOT EDIT`, committed so the grant chain is inspectable without a build).

The `*-read` / `*-write`/`-control` split exists so a future curated
non-main capability (see below) can be handed a read-only half of a module
without also handing it that module's mutations.

### The zero-permission template — the base a real plugin capability grew from

`capabilities/plugin-zero-template.json` is the artifact #360 Slice C (pane
plugins) depends on — it reuses the shape the Phase-0.5 spike proved holds
(`capabilities/spike-plugin-zero.json` on `spike/360-sandbox-proof`):

```json
{
  "identifier": "plugin-zero-template",
  "windows": ["untrusted-probe-0"],
  "permissions": []
}
```

This file itself stays permanently zero-permission — it is the
`tests/acl_manifest.rs` proof fixture, not what a shipped plugin window
binds to. `capabilities/plugin.json` (`windows: ["plugin-*"]`,
`permissions: ["plugin-broker"]`) is the real, populated capability Slice C
built from this template for actual plugin windows (see
`pane-plugins.md`'s Isolation section). The template's mock label
(`untrusted-probe-0`) is chosen deliberately to **not** match the `plugin-*`
glob `capabilities/plugin.json` binds (rev-65 NB-1 on #369): a label
matching that glob would also pick up the plugin-broker grant, silently
diluting this file's zero-grant proof into "a broker-only window denies
these commands" rather than "a genuinely zero-grant window denies these
commands." Neither this label nor `plugin-zero-template` itself is ever a
real window in the shipped app, so the file is inert in production;
`tests/acl_manifest.rs` opens a mock window with the mock label specifically
to prove the deny is real.

## The coherence test — the actual safety net

`src-tauri/tests/acl_manifest.rs` is what turns "a missed grant silently
breaks main" into "CI is red." Three tests:

1. **`generate_handler_matches_app_commands`** — parses the bare command
   names directly out of `tauri::generate_handler![...]` in `src/lib.rs`
   (string search + bracket match, not a hand count) and diffs them against
   `command_manifest::APP_COMMANDS`. Fails if a command is registered in one
   list but not the other.
2. **`app_commands_len_is_125`** (`app_commands_len_is_120` at this design's
   original landing) — a drift tripwire against the count this design and the
   #363 plan cite; bumped to 122 by #360 Slice B's `list_plugins`/
   `install_plugin`, then to 125 by #360 Slice C's own three broker commands
   (see the addendum below).
3. **`main_has_all_125_and_zero_permission_denies_dangerous_spread`** — the
   one that matters most. It builds a real (headless) `tauri::test` mock app
   — `tauri::test::mock_builder()` + `.build(tauri::generate_context!())` —
   using the app's **actual on-disk `capabilities/`/`permissions/`**, the
   same resolution `build.rs` feeds the shipped binary. This is not a
   reimplementation of ACL resolution; it exercises Tauri's real resolver.
   It registers 120 stub commands sharing the real commands' bare names
   (zero-arg no-ops — no PTYs, no git/gh calls, no orchestration side
   effects), invokes every one of them against the `main` window label and
   asserts none are denied, then invokes the plan's representative dangerous
   spread (`orch_grant_merge`, `git_push`, `ft_write_file`, `spawn_pty`,
   `open_in_editor`) plus a benign control (`pty_backend_info`) against the
   `untrusted-probe-0` window label and asserts the spread **and** the
   control are denied there — while the same control stays allowed for
   `main`, proving the denial is a genuine per-label ACL check and not a
   globally broken IPC pipe that would make the dangerous-spread denials
   meaningless.

**Red-before-green, as run for this PR:** removing `"allow-orch-grant-merge"`
from `permissions/sets/orch-control.toml` and re-running
`cargo test --test acl_manifest` fails test 3 with:

```
main is missing a grant for: ["orch_grant_merge"] — the #363 flip is
all-or-nothing, so an ungranted command silently breaks main. Grant it via
capabilities/default.json or one of the permissions/sets/*.toml sets
aggregated into "main-ui".
```

Restoring the line returns the suite to green. This is the exact "silently
breaks main" failure this migration exists to prevent, now caught by CI
instead of a user's dev session.

## What this does and does not close on #189

#189 logged the "any webview script reaches every command, CSP is `null`"
fact this migration is the concrete answer to for the specific "isolate a
non-main webview" case. It is a **partial** answer, deliberately:

- **What it closes:** per-webview command denial is now real. A future
  plugin/non-main webview can be confined to zero or a curated grant, and
  that confinement is genuinely enforced by Tauri's resolver — the hard
  prerequisite #360 Slice C needed.
- **What it does not close:** `main` is still granted all 120 commands, and
  the app CSP (`tauri.conf.json`'s `app.security.csp`) is still `null`. A
  script injected *into main* — e.g. via the frontend's own XSS surface —
  still reaches every command exactly as before this change. And #189's core
  threat, prompt injection into orchestration agents via untrusted
  issue/PR/comment text steering role instructions, is a surface ACL doesn't
  touch at all.

**#189 stays open** for the agent-injection threat model and for tightening
`main` itself (CSP, high-risk-op confirmations). This design note and the
PR are cross-referenced there.

## Dependencies and Windows constraints

No new dependency. `tauri_build` was already a build-dependency and
`tauri-utils` (which carries the ACL codegen `.app_manifest(...)` exercises)
was already a transitive dependency of `tauri` in `Cargo.lock`. The one
`[dev-dependencies]` addition is `tauri = { features = ["test"] }` — an
empty, pure code-gate feature on the tauri crate loomux already depends on
(unlocks `tauri::test`'s mock runtime for `tests/acl_manifest.rs`); Cargo's
per-target feature unification keeps it out of the shipped binary.

getrandom / `bcryptprimitives.dll!ProcessPrng` (the Windows 10 baseline
hazard this repo bans — see `Cargo.toml`): not triggered. `cargo tree`
confirms no `getrandom` crate enters the dependency graph as a result of this
change; the ACL command codegen runs on the build host, not in the shipped
binary, and produces no random-id generation (permission identifiers derive
from command names).

## Update (#360 Slices B and C): 120 → 122 → 125 commands

The command count this note and `tests/acl_manifest.rs` cite grew twice past
its #363 landing of 120:

- **#360 Slice B** (backend host, `plugins.rs`) added `list_plugins` and
  `install_plugin` (122 total) — both main-only, folded into a new
  `permissions/sets/plugins.toml` set aggregated into `main-ui`.
- **#360 Slice C** (the trust core, `pluginbroker.rs`) added its own three
  commands (125 total) — `plugin_open_window`, `plugin_broker_request`,
  `plugin_broker_open_channel`. `main` is granted all three (the "registered
  means main may call it" rule above applies unchanged); the latter two are
  also — and *only* — granted to a new `capabilities/plugin.json`
  (`windows: ["plugin-*"]`) via a new `permissions/sets/plugin-broker.toml`
  set, the first real (non-template) consumer of the zero-permission pattern
  this note's "zero-permission template" section anticipated.
  `plugin_open_window` is main-only (folded into the `misc` set) — a plugin
  window must never be able to open another plugin window itself.

Both slices' commands are otherwise ordinary entries in `APP_COMMANDS` and
`generate_handler!`, subject to the same all-or-nothing flip as every other
command. See `pane-plugins.md`'s Isolation section for the full trust-core
design Slice C's grant makes possible.
