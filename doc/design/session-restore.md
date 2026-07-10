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
| **Agent** (no `sessionId`) | **Dormant** pane with a Start button, in the same cwd | Best-effort CLIs (copilot/codex/gemini) have no clean resumable id; honest, not silently broken. |
| **Orchestrator / worker / reviewer** (`orch`) | **Dormant** — the human resumes the whole group via the existing `resumeOrchSession` | The one place a resume can actually burn credits; keep the safety stance exactly here. The rule is keyed on **kind, not the presence of an id** — a worker with a session id still stays dormant. |

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
| Wiring (Phase 4) | `src/main.ts` | Splash, `hasSnapshot`, layout capture into `snapshot()`, grid rebuild, auto-resume. *Not in this phase.* |

`shellKind` is recorded here but the backend spawn plumbing that acts on it lands
in the shell-kinds phase; `sessionId` is populated by the launcher when it spawns
a session-capable CLI (Phase 4). This phase makes both **capturable**.
