# Session restore (issue #194)

On reopen, loomux can bring back the **whole prior session** — every tab, each
tab's pane layout, and, where possible, the live agent sessions — or start
clean. This note is the architecture of the **data layer** for that: the
persisted schema, the restore decision model, and the per-pane restore policy.
The boot splash, the grid rebuild, and the auto-resume wiring are the wiring
layer (`main.ts`, Phase 4) and are described only where they consume this core.

This extends **project tabs** ([project-tabs.md](project-tabs.md)), which already
persists the tab shells (name / color / order / active / group binding) through
`tabstore.ts` → the opaque `tabs.json` blob. Session restore adds a per-tab
**pane layout tree** and two top-level fields to that same blob — no backend
change, because the blob stays opaque to `uistate.rs`.

## What the schema captures (and deliberately does not)

`tabstore.ts` is the single source of the tab schema. The `tabs.json` blob gains:

- **`schemaVersion`** — bumped to `2`. A pre-#194 file has no version; decode
  reads that as `1`.
- **`restorePref`** — `"ask" | "restore" | "fresh"`. First run is `"ask"`
  (show the splash), then the human's remembered choice.
- per-tab **`layout`** — an optional split tree mirroring `grid.ts`'s
  `GridLayoutNode`, but with serializable **`PersistedPane`** leaves instead of
  live `Pane` objects. Each leaf records only what restore needs:
  `paneKind` (`terminal | agent | orch`), `name`, `cwd`, `command`/`argv`,
  `shellKind`, and a recorded resumable `sessionId`.

What is **never** captured: the live PTY, the terminal buffer/scrollback, or any
geometry. A pane is re-created or resumed from its record; its process history
is gone (the PTY died with the app). This preserves the cost/#78 stance and the
no-resize invariant — capture reads `layoutSnapshot()` (in-memory tree + flex
weights, no geometry) plus `Pane.capture()` (retained launch inputs + live cwd).

### Migration contract — old files load cleanly

Every #194 field is **optional and additive**, so an old `tabs.json` decodes
exactly as before — shells-only, `restorePref` defaulted to `"ask"`,
`schemaVersion` `1`, and **no `layout` key invented** on any tab. `encodeTabs`
also accepts a pre-#194 snapshot *object* (no `restorePref`/`schemaVersion`/
`layout`) unchanged and stamps the current version on write — which is why
`main.ts`'s `tabs.snapshot()` needs no change to keep producing a valid blob.

A malformed `layout` is **fail-safe**: any invalid node (bad pane, unknown kind,
empty or mis-directed split) collapses that tab's **whole** layout to `null`
rather than throwing — the tab then restores as a single fresh shell. This is
the same "degrade, never crash boot" guard the tab decoder already applies to
malformed tab entries. Malformed *scalar* fields inside an otherwise-valid leaf
coerce to `null`/defaults (bad `cwd` → `null`, non-string `argv` element → whole
`argv` `null`, unknown `shellKind` → `null`, bad `weight` → `1`).

## The restore decision — `restoredecision.ts`

`decideRestore(pref, hasSnapshot) → "restore" | "fresh" | "prompt"`. Tiny by
design: the remembered preference decides, except that with **nothing worth
restoring** we always go `"fresh"` — never prompt over an empty session, never
claim to restore a blank state. `hasSnapshot` is computed by `main.ts` from the
decoded blob (at least one tab, with a captured layout worth rebuilding).

| `pref` \ `hasSnapshot` | `false` | `true` |
| --- | --- | --- |
| `"ask"` | `fresh` | `prompt` (splash) |
| `"restore"` | `fresh` | `restore` |
| `"fresh"` | `fresh` | `fresh` |

## The per-pane restore policy — `panerestore.ts` (the adopted hybrid)

The issue's key insight: **resuming a CLI session re-opens its context but costs
nothing until a prompt is sent.** That makes auto-resume viable for agent panes
without burning credits — but *not* for whole orchestration groups, where a
resumed autonomous orchestrator (#83) can idle-tick and spawn a worker storm
(#78). So the policy is **kind-aware**:

| Pane kind | On restore | Why |
| --- | --- | --- |
| **Terminal** | Re-spawn a fresh shell in the recorded cwd + `shellKind` | No session to resume; zero cost; layout/cwd back instantly. |
| **Agent** (has `sessionId`) | **Auto-resume** via `--resume <id>` into the idle TUI; **never** replay a queued prompt | Loads context, spends no credits — the "near-exact state" goal. |
| **Agent** (no `sessionId`) | **Dormant** pane with a Start button, in the same cwd — plus, when a matching session is found, a second "Resume last session" button (#440) | Best-effort CLIs (copilot/codex/gemini) have no clean resumable id; honest, not silently broken. A claude/copilot pane can *also* land here despite being session-capable — see [session-id-learning.md](session-id-learning.md) for how loomux learns an id it didn't itself mint, and why "no id recorded" isn't always "no session exists". |
| **Orchestrator / worker / reviewer** (`orch`) | **Dormant** — the human resumes the whole group via the existing `resumeOrchSession` | The one place a resume can actually burn credits; keep the safety stance exactly here. The rule is keyed on **kind, not the presence of an id** — a worker with a session id still stays dormant. |
| **File explorer** (`files`, #214) | Re-open the listing at its recorded root — or, if that folder is gone, **fail soft to the welcome form** in that slot with a toast | Pure content: no process, no session, no credits, nothing to resume. The only thing that can rot under it is the *folder*, so the root is re-probed (`ftRootIsDir`) before the pane is built. Keyed on kind like the orch rule: a stray `sessionId` on a files leaf must never send it down an agent path. |
| **File editor** (`editor`, #217) | Re-open the editor at its recorded root, **re-opening the file it was showing** (`file`, a root-relative path — read fresh from disk); same `ftRootIsDir` probe, same fail-soft | Same reasoning, plus one wrinkle: a pane opened from the file browser is *titled after its file*, so restoring a bare tree under that title would name a file the pane isn't showing. What is **not** restored is the BUFFER. Persisting unsaved text would make the layout file a second, silent copy of the user's work — the close guards are what ensure they were *asked* before it could be lost, and a snapshot that quietly preserves it undermines exactly that. A file deleted since just fails to open (a toast); the pane still comes back, rooted. |
| **Git** (`git`, #217) | Re-open the git view over its recorded repo — probed with **`gitRepoRoot`**, not `ftRootIsDir`. A probe that *throws* (git not on `PATH`, unreadable path) keeps the pane; only git's own "not a repo" fails soft | A folder can still exist and no longer be a work tree (a pruned worktree, a deleted `.git`, a repo restored from backup as plain files), and a git pane over a non-repo can only tell you it isn't one. But a git that cannot be RUN is a fact about the environment, not the repo: failing soft on it would swap every git pane for a welcome form *and* drop the recorded path from the next save — losing it for good over a transient hiccup. Also **not** restored: the selected worktree and the read-only unlock (#208) — a restored pane opens on the primary, locked, like a fresh one. An unlock that survived a restart is the one piece of this pane's state that could quietly cost you something. |

None of the content kinds needed a **schema change**: each one's root rides in the
existing `cwd`, so `SCHEMA_VERSION` stays at 2 and older files (which simply never
contain such a leaf) decode unchanged — the same shape-driven, additive move `role`
made in #194.5. A rootless content leaf is *well-formed but unrestorable*, so it
decodes (rather than triggering the whole-tree fail-safe and taking its sibling
panes down with it) and is resolved in the one slot at restore time.

`planPaneRestore(pane) → RestoreAction` is the per-pane core; `planLayoutRestore`
turns a layout tree into an ordered `RestoreOpenStep[]` — one `grid.openPane`
call each, with `relativeTo` (the index of an earlier step's pane to split from),
`dir`, and a `weights` chain. This is the **reconstructible** plan: a split's
first child stays put as the anchor and its siblings open beside it, so the
direction and the subtree's weights ride on the sibling steps. A flat
`{dir, weight}[]` (an earlier draft) dropped `relativeTo` and split weights, which
made a 2×2 grid and four stacked panes flatten to the *identical* sequence —
unreconstructible. A serialize → `planLayoutRestore` → replay round-trip is now
structure- **and** weight-identical; `test/panerestore.test.ts` proves it with a
pure model of grid's `insertBeside` (and pins that the 2×2 and 4-stack plans
differ). `grid.openPane` resets flex to equal shares as it splits, so `main.ts`
applies the `weights` after building. All three functions are pure and
exhaustively unit-tested.

**The one-line flip.** The plan promised that switching to all-dormant (every
agent gets a Start button, matching the earlier #167 default) is a single-line
change. It is: `export const AUTO_RESUME_AGENTS` in `panerestore.ts`. Set it to
`false` and every agent restores dormant; groups are dormant regardless.

Rejected outright: **re-attaching** to the old PTY (impossible — it died with the
process) and **auto-resume-with-a-replayed-prompt** (would spend credits on boot).

**Orch leaves + the double-spawn contract.** Unlike the earlier plans, `capture()`
*does* serialize orchestration panes (as `paneKind: "orch"`) rather than dropping
them, so the layout keeps its shape — but `planPaneRestore` maps them to
`dormant-group`, which **must spawn nothing**. The group is revived only by the
tab's `groupId` binding through `resumeOrchSession`; if Phase 4's handling of
`dormant-group` ever opened a pane, a subsequent group resume would double-spawn
every worker (the #78 storm). That contract lives on the `RestoreAction`
`dormant-group` variant and must be honored in the Phase 4 rebuild.

## Module map (this phase)

| Piece | File | Role |
| --- | --- | --- |
| Schema + validators | `src/tabstore.ts` | `PersistedPane` / `PersistedLayoutNode` / `RestorePref`, versioned encode/decode, the fail-safe layout validator. Unit-tested. |
| Restore decision | `src/restoredecision.ts` | `decideRestore` — restore/fresh/prompt. Unit-tested. |
| Per-pane policy | `src/panerestore.ts` | The adopted hybrid + tree flattening + the all-dormant flip. Unit-tested. |
| Capture getter | `src/pane.ts` | `Pane.capture() → PersistedPane \| null` (null for a setup-state welcome pane); retains launch inputs (`command`/`argv`/`shellKind`/`sessionId`) for it. DOM-coupled → hand-validated. |
| Wiring (Phase 4) | `src/main.ts` | Splash, `hasSnapshot`, layout capture into `snapshot()`, grid rebuild, auto-resume, dormant Start/Resume. |
| Splash overlay (Phase 4) | `src/restoresplash.ts` | Cold-boot "Restore last session?" overlay (thin DOM over `decideRestore`). |
| Counter/markers (Phase 4) | `src/tabcounts.ts` | Pure per-tab live-agent count + live/dormant orchestration markers. Unit-tested. |
| Group resume (Phase 4) | `src/groupresume.ts` | Pure whole-group resume plan (orchestrator first, delegates rejoin/skip). Unit-tested. |

`shellKind` is recorded here but the backend spawn plumbing that acts on it lands
in the shell-kinds phase; `sessionId` is populated by the launcher when it spawns
a session-capable CLI (Phase 4). This phase makes both **capturable**.

## Phase 4 — the wiring (this phase)

The data layer above is now driven end to end by `main.ts` and a thin overlay.

**Capture, populated.** `Pane.capture()` already reduced a live pane to a
`PersistedPane`; Phase 4 fills the last gap — the **session id**. The launcher
mints one for a session-capable CLI (Claude only) as `crypto.randomUUID()` — the
webview's Web Crypto, **not** a getrandom crate, so constraint 2 (which governs
`src-tauri` Rust) doesn't apply — appends `--session-id <uuid>` to the command,
and threads the id onto the pane. `Workspace.captureLayout()` walks
`grid.layoutSnapshot()` into a `PersistedLayoutNode` tree (pruning welcome/setup
leaves, collapsing a split that thereby loses a sibling), and `TabManager.snapshot()`
now carries each tab's `layout` plus the remembered `restorePref`. A new grid
`onChange` callback (fired on pane open/close) re-persists and re-renders the tab
strip, so **live panes persist on change and close** — no longer only on tab-level
edits.

**The boot decision.** `main.ts` decodes the blob, computes `hasSnapshot`
(`hasRestorableContent`: ≥1 tab with a layout, a group binding, or simply >1 tab),
and calls `decideRestore(pref, hasSnapshot)`. `prompt` shows `restoresplash.ts` —
Restore / Start fresh, with a *Remember my choice* box that writes the preference
back (unticked keeps it `"ask"`). It's a pure overlay before any tab exists, so it
resizes nothing.

**The rebuild.** For each restored tab, `rebuildLayout` runs
`planLayoutRestore(layout)` and replays each `RestoreOpenStep` into the tab's grid
(`relativeTo` → the anchor pane from an earlier step, `dir` → the split direction),
then calls the new `grid.applyLayoutWeights(layout)` **once** — `openPane`/
`openDormantPane` reset flex to equal shares as they split, so the saved divider
drags are re-applied after the tree exists. The replay matches the pure model in
`test/panerestore.test.ts` (same `insertBeside` semantics), so structure and
weights come back identical.

Per action:

- **spawn-terminal** → `grid.openPane` with the recorded `cwd` + `shellKind`.
- **resume-agent** → `grid.openPane` with `agentResumeCommand(command, argv,
  sessionId)` — the recorded launch line with any `--session-id`/`--resume`
  stripped and `--resume <id>` appended (flags like the autopilot permission flag
  survive; **no prompt is ever appended** — the no-replay rule). The session id is
  re-recorded so a *second* restore resumes identically.
- **dormant-agent** → `grid.openDormantPane` showing a **Start** card that calls
  `pane.startFromDormant(...)` with the recorded command.
- **dormant-group** → `grid.openDormantPane` showing a **Resume group** card. This
  is where the **no-double-spawn contract** is honored: the placeholder spawns
  nothing. Resume looks up the group's recorded orchestrator session
  (`orchSessionRoles`) and revives the whole group through the existing
  `resumeOrchSession` — the *one* path that spawns it — **then** closes the now-
  redundant dormant ORCH placeholders (after the revive added a real pane, so the
  grid never empties). A dormant pane re-captures its record verbatim, so a session
  closed without resuming offers the identical restore next boot.

Every pane rebuilds `background` (no focus theft); the active tab is focused last.
The rebuild runs with a `booting` guard so the many intermediate opens don't each
re-persist — boot persists once at the end.

**Counter + markers.** The tab strip's agent counter was unreliable (it read only
a 4-second backend group poll, so a plain-agent tab showed nothing and a just-
opened group flashed a stray `0`). It now derives from `tabcounts.ts` over the
panes actually open in the tab (`Workspace.paneInfos()` → `Pane.tabPaneInfo()`):
`agents` counts live agent + live orchestration panes; `liveOrch` drives the `⛓`
icon; `dormantOrch` (a bound-but-not-live group, or a dormant ORCH placeholder)
drives the static `ORCH` chip — never both at once. Cost/paused still come from the
poll. The grid `onChange` re-render makes the count immediate, not poll-latent.

**Stranded-form fix (P1 debt).** A welcome form fires its result and is retired,
but an orchestrator launch that threw afterward left the form stranded with a
disabled *Working…* button. `handleWelcomeSubmit` now catches it, toasts the
error, and calls `form.reopenAfterLaunchFailure` (restoring the fired callback and
re-opening the `SubmitLatch`) so the human can fix the cause and retry.

### rev-80 hardening — every population/layout change flows through one notify

The first cut hooked `grid.onChange` only at leaf placement, which fired *before*
`pane.start()` assigned a `ptyId` and never fired at all on the in-place
conversion paths — so the counter missed a single-agent submit and undercounted a
fan-out by one, and a divider drag or pane drag-move was never persisted (the
demo's "drag then quit" restored stale weights). The rule now is: **anything that
changes a tab's live pane population or its layout re-renders + re-persists.**

- `grid.openPane` fires `onChange` *after* `pane.start()` resolves (PTY live), and
  the in-place conversions (`startFromWelcome`, `startFromDormant`) + the
  kept-open exit path call `onGridChanged()` in `main.ts` once they settle.
- Terminal layout mutations that only touched flex/order — the divider-drag
  `mouseup`, the drag-reorder commit, and dock/undock — now fire `onChange`. All
  are terminal (one per gesture), so `persistTabs`'s snapshot dedup absorbs the
  rest; no per-mousemove write storm.
- A kept-open exited pane sets `Pane.exited`, so `tabPaneInfo().live` is
  `ptyId !== null && !exited` — a dead agent stops inflating the count.

**Docked panes are captured.** `layoutSnapshot` only covers the split tree, so a
minimized pane would have been silently dropped. `PersistedTab.docked` (additive,
migration-safe) carries `Workspace.captureDocked()`; restore reopens each via the
same `openActionPane` used for layout leaves, then `grid.minimize`s it back into
the dock. So the live buffer/scrollback is still never captured, but no *session*
is lost to the dock.

### Post-demo fixes — resume-of-empty-session and the boot ordering

**BUG-1 — a `--resume` with no conversation must not strand a dead pane.** We mint
`--session-id` at launch, but a session the user never prompted persists no
transcript, so `claude --resume <id>` exits 1 ("No conversation found …"). Two
layers now handle it, both keeping panerestore pure:

- *Pre-check.* `main.ts` fetches `listSessions()` (which lists exactly the
  sessions that HAVE a transcript) and passes a `SessionResumable` predicate into
  `planLayoutRestore`/`planPaneRestore`. An agent whose id is absent plans a new
  `fresh-agent` action instead of `resume-agent` — a fresh session **in place**
  with the same name/cwd/CLI, reusing the recorded id (via `agentFreshCommand`,
  which pins `--session-id`, not `--resume`) so it's resumable again next boot.
  On an empty/failed session list we assume resumable and lean on the backstop.
- *Runtime backstop.* A resumed pane registers a one-shot fresh-fallback. If its
  PTY exits unexpectedly non-zero **within a short window** of the resume spawn
  (`shouldRespawnFresh` + a time gate), `Pane.respawnFresh` reuses the open
  terminal to start fresh in place — covering a transcript deleted between the
  pre-check and the spawn, or any other resume-time CLI failure. The time gate is
  essential: a resume that *succeeded* and was worked in for a while and then
  exits non-zero is the human's own session ending, not a resume failure, so it's
  left alone. Unlike the pre-check, the backstop mints a **new** session id for the
  fresh command instead of reusing the recorded one: a resume can fail because the
  transcript EXISTS but is corrupt/half-written, and `--session-id <recorded>`
  would then hit the same conflict again — a brand-new id always creates cleanly.
- *Early-exit symmetry.* Both restore open paths (`rebuildLayout`, `restoreDocked`)
  call `reapIfExited` after each `openActionPane`, matching the welcome/session
  paths — a spawn that exits in the sub-tick before `ptyId` is assigned is drained
  from `earlyExits` (and can trip the fresh-fallback) rather than leaking.

### Whole-group resume (demo rounds 3–4)

The dormant **Resume group** button restores the panes that were LIVE at close —
the whole group, but **exactly** that group, no more.

**The set comes from CAPTURE, never the roster.** An early cut derived the member
set from `orchSessionRoles()` → `session_roles()`, which lists every member the
group *ever* had (long-killed workers included) — so a group that closed with an
orchestrator + 1 worker came back with a swarm of stale worker panes (demo round 4
over-restore). The fix: each captured orch pane now records **its own session id
and role** (`Pane.capture()` for kind `orch`, the id parsed from the backend-built
command by `sessionIdFromCommand` at spawn), so the persisted layout carries one
leaf per orch pane that was open at close. On restore those become `dormant-group`
placeholders each holding that record; `resumeDormantGroup` reads the member set
straight off the tab's placeholders (`Pane.restoreRecord`). `session_roles()` is
no longer consulted for the SET — the backend still validates membership and drives
re-registration when each member resumes, but it can never EXPAND beyond what was
captured. Members that were not open at close stay dead; they remain resumable
later from the session browser (out of scope, by design).

`planGroupResume` (pure, unit-tested) turns the captured members into an ordered
plan: orchestrator first, then the delegates, split into `rejoin` (session has a
transcript) and `skipped` (none). Its tests pin captured-set-in == planned-set-out
— a 10-member historical roster is irrelevant because it's never an input.
`resumeDormantGroup` executes the plan through the **existing** `resumeOrchSession`
path — no backend change:

1. Resume the orchestrator → the backend `resume_recorded_session` relaunches the
   whole control plane (`create_orchestration_group` with the resumed session),
   bringing the group live.
2. Resume each `rejoin` delegate **sequentially** — the backend refuses a rejoin
   into a group that isn't live yet, so order matters and the orchestrator must be
   awaited first. Each rejoin runs `spawn_agent_ex` with the recorded session id,
   which **re-registers** the agent into the group (MCP identity, roster, cwd) so
   the orchestrator can message it again, and `--resume`s its idle TUI (credit-
   neutral, no prompt replay). Its pane arrives in this tab via the group→tab
   routing.
3. The per-group latch (`resumingGroups`) wraps the whole sequence, so one click is
   one atomic multi-pane restore — the many placeholder cards of a group can't each
   kick off a resume, and no member is double-spawned.

**What restores:** exactly the captured members (the orch panes live at close),
each whose session has a saved conversation — re-registered with the group and
resumed into its idle TUI; same number of panes out as were captured in. **What
does NOT (stated, not silent):** a captured delegate that was never prompted has no
transcript, so `--resume` would fail and strand a dead pane, and the frontend can't
spawn a fresh *group-registered* worker (only the orchestrator spawns delegates).
Those members — plus any captured member with **no resumable id at all** (a copilot
delegate: copilot mints its own id after boot, so there's nothing to `--resume`) —
are counted together in the skip toast and left behind; the orchestrator can respawn
a fresh one on demand once it's live. The **orchestrator itself** is gated on the
same transcript predicate (`planGroupResume` → `orchestratorUnresumable`): a stale
orchestrator session doesn't relaunch into a dead pane — the whole resume falls back
to the session browser with a specific message. Pane **positions** within the tab
are also approximate — the orchestrator and rejoining workers lay out as they arrive
(a fresh group layout), not the exact captured split; the tab, sessions, and roster
are what's preserved.

**BUG-2 — decline crashed with "no active workspace".** The restore splash is
awaited while the app has zero tabs, and the window-focus handler (plus voice
init) resolve through `tabs.activeWorkspace`, which throws when the manager is
empty. Root cause was ordering, not a missing guard: boot now **seeds one tab
before** the splash, so there is always an active workspace. The restore path
builds its saved tabs and then drops the seed (indexing `activeIndex` against the
tabs it created, not `tabs.tabs`, since the seed offsets it); the fresh/decline
path just keeps the seed as the blank welcome tab.

**The credit/data sharp edges.** The dormant **Resume group** button disables on
first click and re-enables only on failure — a second click while the first resume
is in flight can't double-create the group (the double-spawn the contract
forbids), and a resume error is a toast, not the crash banner. The restore splash
is non-committal on **Esc**: a keyboard dismiss is a one-time fresh that never
writes the preference, and boot skips the end-of-boot persist for a non-committal
decline, so the saved `tabs.json` survives for the next launch's splash (one
habitual Escape can't wipe the session). An orchestrator launch that fails tears
down the tab it just created (`launchOrchestratorTab`'s catch) instead of leaking
an empty tab per retry, and re-focuses the form's own tab.

### #412 hardening — resolution robustness, and failing loudly

Root cause (confirmed on this machine, not inferred): a worker/reviewer resume's
launch cwd came from the roster's cached `AgentRecord.cwd` — the directory that
worktree was cut into at spawn time. When that worktree is later removed (its
branch merged, `git worktree remove`), `resume_recorded_session` used to
`.filter(|c| Path::new(c).is_dir())` the stale cwd down to `None` and let it fall
through to `spawn_agent_ex`'s per-role default — **the group's main clone**. The
pane then launched `claude --resume <id>` from the main clone's cwd. Per the CLI
reference, "passing a session ID searches only the current project directory and
its git worktrees" — Claude Code's own project-directory store is keyed off the
*launch* cwd (`~/.claude/projects/<munged-path>/<id>.jsonl`), so a resume from the
wrong cwd searches the wrong project directory and reports "no session found" —
even though `list_sessions` (which walks every `~/.claude/projects/*/` directory,
not one cwd) finds the exact same session fine, which is why the session browser
shows it as resumable while the resume itself fails. Reproduced directly: a
session's own `cwd` field (inside its `.jsonl`) named a worktree no longer present
in `git worktree list` for that repo; a resume attempt recorded against the main
clone's cwd instead failed with "no session found" for a session plainly on disk.

**Resolution (`sessions::find_session_cwd`, `orchestration::resolve_resume_cwd`/
`resolve_worker_resume_cwd`).** Locate the session directly in its CLI's store BY
ID — a bounded scan of `~/.claude/projects/*/<id>.jsonl` (claude, filename-keyed)
or `~/.copilot/session-state/*/workspace.yaml` (copilot, matched on the parsed
`id:` field, since its dirname isn't guaranteed to equal the id) — and read back
the cwd the session itself recorded. This is the best available signal, not a
guarantee (#412 review N2): it's the exact string the CLI already wrote for
itself, so it sidesteps a stale/moved worktree AND any casing/separator drift
between loomux's cache and the CLI's own record, without loomux ever having to
reproduce Claude's project-directory munging algorithm — but the recorded
`cwd` is not always the directory `--resume` actually searches (see
`find_session_cwd`'s doc comment in `sessions.rs` for the `.claude/worktrees`
case this doesn't cover: 2 of 691 real sessions on the machine this was
verified against).

**Testability.** The claude/copilot store roots are each overridable via a
`thread_local!` seam (`set_claude_projects_root_for_test`/
`set_copilot_session_state_root_for_test`, both `sessions.rs`), scoped to the
calling thread only — deliberately NOT a process-wide env var (#412 review
B2): Rust's default test harness runs each `#[test]` on its own OS thread, so
a thread-local set inside one test's body can never be read by a concurrently
running test the way a `std::env::set_var` mutation could (real,
unsynchronized-mutation undefined behavior across threads, which is why that
function is `unsafe` as of recent Rust editions — not just a style concern).
`tests/orchestration.rs`'s `fixture_claude_session`/`fixture_copilot_session`
helpers write the on-disk shape these seams point at.

**Launch-cwd choice, stated (corrected after #412 review N1 — this section
previously claimed the opposite of what the code does).** The roster's cached
cwd wins whenever it's still a real directory on disk — `resolve_worker_resume_cwd`
returns it directly, without ever consulting the store. Only a missing, empty, or
no-longer-existing cached cwd falls through to the store scan. This is
deliberate, not merely the cheap path: a live worktree the roster still points
at IS the session's current home, and is strictly better evidence than the
store's possibly-stale snapshot of where that same session happened to run
*at some point* — the store is consulted only because loomux's cache has gone
stale (the one case it's actually needed), never used to second-guess a cache
entry that's still checked out. A caveat this stance accepts: if the roster's
cwd and the store's cwd disagree while BOTH still exist as real directories
(e.g. a worktree re-added at a different path than the one the session
originally ran under), the roster wins even though it may not be the exact
directory the CLI itself would search — a narrower case than the #412 repro
(worktree gone entirely), left unhandled by design rather than by omission.

**Failing loudly.** When resolution comes up empty, `resume_recorded_session`
(worker/reviewer path) now resolves the cwd **synchronously, before** the
background `spawn_agent_ex` thread — so an unresolvable resume returns `Err`
straight back through the IPC call, and **no pane is ever opened** for it (no
"normal agent pane with no steering box" to degrade into, because nothing gets
spawned at all). The error is tagged (`resume-not-found:` / `resume-workspace-
missing:` / `resume-store-unreadable:` / `resume-ambiguous:` — the last from
`resolve_session_ref`'s existing prefix matching), so an orchestrator's
`spawn_agent(resume_session:)` can branch on the tag instead of parsing prose, and
`resumeerror.ts` (`resumeFailureKind`) does the same on the frontend. The session
browser turns a `not-found`/`workspace-missing` failure into a confirm dialog
("Session not resumable — Start fresh?") instead of a dead-end fatal banner;
confirming re-spawns fresh with the SAME recorded group/role/block/task brief
(`start_fresh` on `resume_orch_session`/`resume_recorded_session`), cutting a new
worktree rather than resuming the unresolvable one.

**`start_fresh` is a fresh CONVERSATION, never a fresh LAUNCH (#412 rev-17
blocker, fixed).** For an orchestrator, "start fresh" must reattach to the
group's EXISTING state — its persisted roster and merge gate, the ones the
human previewed and approved at the actual launch — not re-read
`.loomux/workflow.yml` as if this were a new launcher session. The first cut
of `start_fresh` got this wrong by conflating two questions that happened to
coincide in every case that existed before it: `resume_session.is_some()`
used to double as "does this launch read the workflow file"
(`create_orchestration_group` derived `Launch` from it directly). `start_fresh`
introduced a THIRD case — an existing group, no session id to resume — where
that derivation gives the wrong answer: `Launch::Fresh`, which silently
swapped the group's roster to whatever the repo currently declares and could
delete its merge-gate spec file if the repo no longer declares one. Neither
the roster swap nor the gate deletion goes through anything a human sees; a
two-button "Start fresh?" confirm is not the launcher's roster preview, and
`Launch`'s own contract (`orchestration/mod.rs`) is explicit that a resume's
consent moment is the ORIGINAL launch, not this one.
`create_orchestration_group` now takes `launch: Launch` as its own explicit
argument instead of inferring it — `resume_recorded_session`'s orchestrator
branch always passes `Launch::Resume`, whether or not it's carrying a session
id to `--resume`, because either way it is reopening a group that already
has an approved roster and gate on disk. `tests/orchestration.rs`'s
`start_fresh_on_an_orchestrator_does_not_re_read_the_workflow_file` pins both
directions (roster identity, merge-gate content) byte-for-byte across a
repo-file change that would otherwise have been silently adopted.

**Scope, stated (updated after #412 rev-17 B1 — this paragraph's second half
described the pre-B1-fix behavior, which shipped and was then found still
broken).** The orchestrator's own resume is NOT put through the store's cwd —
its launch cwd is always the group's repo path, fixed, never a worktree, so
the moved/deleted-worktree failure mode can't arise for it structurally. That
part still holds: there is no cwd SWAP for the orchestrator. What's no longer
true is "a cleared session still surfaces from inside the pane" — the
orchestrator branch DOES now run the same existence-only pre-check as the
worker/reviewer path (session genuinely absent from the store, or the store
unreadable) before opening a pane, tagged the same way, so `start_fresh` is
reachable for it too — closing #412's titular symptom (a cold-started
orchestration pane that fails inside with no steering box), not just its
worker/reviewer half. See `resume_recorded_session`'s orchestrator branch.
Copilot's own `--resume <id>` cwd-scoping behavior is **undocumented** (the
official reference is silent on it — see the `agent-cli-reference` skill's
citation discipline); the fix applies the same store-lookup mechanism to it
defensively (its session-state layout is flat and id-keyed, so the "wrong project
directory" failure mode is Claude-specific by construction), but this is not
empirically verified against a real `copilot --resume` the way the Claude repro
above is.
