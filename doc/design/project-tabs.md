# Project tabs (issue #63)

Project-scoped **tabs**: each project's panes (plain terminals, agent panes,
orchestrator + worker groups) live in their own tab; switching tabs swaps the
whole workspace; tabs are renamable, colorable, show a live status + attention
alert + hover preview, and persist across restarts. User-facing usage lives in
the README ([Project tabs](../../README.md#project-tabs)); this note is the
architecture and the decisions behind it.

This is **Option A** from the issue #63 investigation: one `Grid` per tab,
swapped via `display:none`. `Grid`/`Pane` and the backend are reused essentially
unchanged; a thin tab layer sits on top and the orchestration event listeners
become tab-aware routers.

## Module map

| Piece | File | Role |
| --- | --- | --- |
| `TabManager` | `src/tabs.ts` | Ordered tab list, active tab, the never-zero-tabs invariant, the group→tab / pty→tab routing maps, per-tab attention, and the persistence snapshot. **DOM- and Grid-free** → unit-tested under `node --test`. Generic over a minimal `ManagedWorkspace` so tests drive it with a fake. |
| `Workspace` | `src/workspace.ts` | One tab = a `Grid` + its own dock, mounted in a container in the workspace stack. Owns hide/show (`display:none` + GL policy) and the preview composite. |
| `TabBar` | `src/tabbar.ts` | The tab strip: switch/close/new, rename, color, the attention chip, the live status chip, the hover-preview composite (safe HTML render), and the right-click pause menu. |
| `tabroute.ts` | `src/tabroute.ts` | Pure routing + preview decisions: cross-tab attention folding, the live cross-tab pane lookup (`findPaneByPty`), and the preview-style **sanitizer**. Unit-tested. |
| `tabstore.ts` | `src/tabstore.ts` | Pure encode/decode + **schema validation** of the persisted tab set. The single source of the tab schema. Unit-tested. |
| `panefit.ts` | `src/panefit.ts` | The pure `shouldResizePty` decision — the **no-resize invariant** at its testable core. |
| `uistate.rs` | `src-tauri/src/uistate.rs` | Backend durable store: atomic `tabs.json` write + corrupt-file quarantine, behind two `#[tauri::command]`s. |
| wiring | `src/main.ts` | Builds the `TabManager`, implements the `OrchWiring` router over it, owns `openUserTab` / `launchOrchestratorTab` and persistence, and the boot restore flow. |

`grid.ts` and `pane.ts` gain only small, additive hooks: `Grid.layoutSnapshot()`
+ `Grid.setHidden()`, and `Pane.setHidden()` + `Pane.serializeViewportHtml()`.

## The no-resize invariant (CLAUDE.md constraint 1) — load-bearing

Resizing a ConPTY on the Windows 10 inbox conhost repaints the whole screen,
which floods scrollback. loomux's hard rule is therefore: **a hidden pane must
never resize its PTY.** Tabs inherit the exact mechanism maximize already uses.

- Switching tabs is `display:none` on the inactive `Workspace` containers. A
  `display:none` element reports **zero client width**.
- `Pane.applyFit` funnels its resize decision through the pure
  `shouldResizePty` (`panefit.ts`), which returns `false` for any zero-width
  pane (and for a no-PTY-yet pane, and for a no-op same-size fit). So a hidden
  tab's panes issue **no** `resize_pty`.
- The preview (below) is a **serialized text snapshot** of the in-memory buffer,
  never a laid-out element — so it can't give a hidden pane a nonzero width and
  re-arm `applyFit`.

**Regression tests.** `test/panefit.test.ts` asserts the predicate directly
(zero width ⇒ no resize, even when the fitted size "changed"). `test/tabs.test.ts`
asserts the mechanism that feeds it: a tab switched away from is only ever set
invisible and **never re-shown while inactive** — a stray `setVisible(true)` on
an inactive tab would give its panes width and trigger the resize storm this
whole design avoids. Together they pin the invariant from both ends.

## Routing model (orchestration ↔ tabs)

The backend is unchanged: pty↔agent↔group binding, the global id-demuxed
`pty-output` stream, and the visibility-independent `orch-attention` /
`orch-focus` / `orch-spawn-request` / `orch-group-ended` events all already
work across N grids. The tab layer is **additive routing** on top.

`TabManager` owns **one** map — `groupId → workspaceId` — maintained in one place
so add/close keep it consistent (closing a tab forgets its route). The group→tab
binding is stable: a group belongs to a tab for that tab's life. `orchestration.ts`
exposes an `OrchWiring` interface it calls into; `main.ts` implements it over the
`TabManager`:

- **spawn** (`orch-spawn-request`) → `targetForGroup(req)` returns the grid +
  pane-events for the group's tab, **creating and repo-naming a tab on first
  sight**. Background open, so it never steals focus from where the human is
  typing (#117).
- **focus** (`orch-focus`) → `focusPty(ptyId)` finds the pane by scanning live
  panes across tabs (`findPaneByPty`), **switches to its tab, then focuses it**
  (`switchTo` no-ops if that tab is already active).

**Why no `ptyId → tab` map.** An earlier cut maintained a pty→tab side-map, but
the paths that need it — focus, pty-exit reaping, rename — all *scan live panes*
(`findPaneByPty` over each grid's `findByPtyId`) instead. A maintained pty map
would go **stale**: nothing removes a per-pty entry when an individual pane
closes (only whole-tab close forgets routes), so a closed pty could resolve to a
tab that no longer holds it. The scan is O(panes), runs only on rare events, and
is always correct because it reads the panes that actually exist. So the pty map
was deleted rather than papered over; the group map stays because it *is* stable.
- **attention** (`orch-attention`) → `applyAttention` badges every pane by its
  pty across all tabs **and** folds the scan into per-tab badge state
  (`tabAttention`), so a hidden tab with a blocked/waiting agent raises a
  labelled alert chip on its strip entry. Urgency/priority reuse `attention.ts`
  so the tab chip, pane header, and dock chip always agree. A dedup
  (`sameAttention`) skips the re-render on the 3-second re-emits.
- **group-ended** (`orch-group-ended`) → close the group's (now-dead) panes in
  whichever tab they live.

Tests: `test/tabroute.test.ts` (attention folding — every class badges its tab,
most-urgent reason wins; and `findPaneByPty` — the live cross-tab lookup, incl.
the not-found case that makes the scan stale-proof) and `test/tabs.test.ts`
(group bind/resolve + route-forgetting on close).

## Persistence (durable backend store)

The tab set persists so a restart brings your projects back. **What persists:**
each tab's name, color, order, the active-tab index, and the orchestration group
it owns. **What does not:** the live panes/PTYs themselves (see Boot restore).

### Format & atomicity

Storage is a single **`tabs.json`** under the app data dir
(`<data dir>/loomux/tabs.json`), a sibling of `orchestration/` and `logs/` — the
same durable tree the rest of the app uses, **not** browser `localStorage` (so
it survives a webview data clear, matching the app's other durable state). The
blob is opaque JSON; `tabstore.ts` owns the schema.

```
{ "tabs": [ { "name": "loomux", "color": "#9ece6a", "groupId": "loomux-abcd" },
            { "name": "scratch", "color": null,      "groupId": null } ],
  "activeIndex": 0 }
```

Writes go through `uistate::write_atomic`, which mirrors the canonical
`orchestration::atomic_write` (the #133/#161 fix): write a **unique** sibling
temp (pid + seq, so concurrent saves never collide), **`fsync` it**, then
`fs::rename` over the target (atomic replace on Windows and Unix). This is the
**#133 anti-truncation guarantee** — a bare `fs::write` truncates in place, so a
crash / kill mid-write destroys the file, which is exactly what wiped the task
board in #133; and the `fsync` before the rename is the disk-full guard (a
rename must not expose a metadata-only file whose data blocks never reached
disk). A crash leaves either the old (valid) file or the temp, never a
half-written target. The one path that can still truncate is the fallback direct
write taken *only* if `rename` fails (a briefly-locked destination on Windows).

### Corrupt-file fail-safe (two layers)

A bad file must never silently cost the user their tabs, and the evidence must
survive for inspection:

1. **Backend (`load_or_quarantine`).** If `tabs.json` is present but not valid
   JSON at all (truncated / garbled — the corruption class), it is *quarantined*
   — renamed aside to `tabs.corrupt.json` so the next save can't clobber it and a
   human can inspect it — and `None` is returned. The caller degrades to a fresh
   tab **without losing the bad file.**
2. **Frontend (`decodeTabs`).** Valid-JSON-but-wrong-shape (a hand-edit) is
   caught by the schema decoder, which validates and coerces every field and
   returns `null` for anything unusable → boot seeds one default tab. Malformed
   individual tab entries are dropped; `activeIndex` is clamped into range.

Tests: `uistate.rs` inline tests (round-trip, missing-parent-dir create,
overwrite-without-truncation, absent→None, corrupt→quarantine, and
quarantine-doesn't-clobber-the-next-save) and `test/tabstore.test.ts` (round-trip
+ every validation branch). Migration and the corrupt→default degrade are also
covered by `test/tabstore.test.ts`'s decode cases.

### Migration

The prototype stored to `localStorage["loomux.tabs"]`. On first boot after
upgrade, `loadPersistedTabs` reads the legacy key **once**, hands it to the
backend, and clears it — thereafter the backend copy is the single source of
truth and `localStorage` is never read again.

## Boot restore — what revives, and why not more

On boot, `restoreTabs` rebuilds the tab **shells**: name, color, order, active
tab, and each tab's group binding.

**Live agent panes/PTYs are deliberately NOT auto-spawned on boot.** This is a
design decision, not a missing feature: reviving N orchestrator + worker CLIs on
every launch would spawn a process storm and burn the user's credits without
them asking (directly the cost concern of #78). Instead the persisted group
binding makes revival **routing-correct**: when the human restores that group's
session from the session browser (or a spawn/rejoin event arrives for the group),
`restoreSession` routes it into the tab that **owns** the group — via the
`groupId → tab` map — rather than whatever tab is active. So a restored project
re-inhabits its own tab through the existing per-session/per-group resume
machinery (`resume_orch_session`), which is the real integration the prototype
stubbed as "a named shell bound to a group."

### The empty-tab rule — ONE rule, no background modals

A tab must always hold at least one pane (the grid's "never empty" rule). There
is exactly **one** rule for filling an empty tab, applied everywhere:

> **An empty tab holds a SILENT plain shell — never the launcher modal — except
> the single genuine fresh start, where the app opens its first pane and the
> human picks via the launcher.**

- **Last-pane exit** (a human ✕, or a background agent process exiting): the
  grid's `onEmpty` refills with `openShellIn` (a plain shell). It must never call
  the launcher — for a hidden/background tab that would pop an interactive modal
  over the *active* tab, driven by a background lifecycle event the human never
  triggered. (This was the review's MED-1.)
- **Boot after a restore**: every restored tab that came back empty — active or
  background, plain or group-bound — is filled with a silent shell. For a
  group-bound tab the shell is a **placeholder** until that group's session is
  restored into it (above); it is not "stray." (MED-1's rule reconciled with the
  boot fill — the review's LOW-2.)
- **Fresh start only** (no restore): the one brand-new default tab opens via the
  normal first-pane flow — the launcher in agent mode. This is the *only* place a
  modal appears on boot, and only when there is no restored state at all.

The launcher otherwise appears solely from an explicit human action in the active
tab: `Ctrl+Shift+T` / the **+** button (`openUserTab`), or a split.

## Memory / GL policy under many tabs

Because inactive tabs are **hidden, not detached** (detaching would lose
scrollback beyond the backend's 256 KB per-PTY ring), every mounted-but-hidden
pane keeps two costs: its xterm in-memory buffer and — if left alone — a WebGL
context. Browsers cap live WebGL contexts (~16), so contexts are the scarce
resource; buffers are the memory cost we accept to make switching instant and
lossless.

**Policy:**

- **WebGL is dropped on hide, reloaded on show.** `Grid.setHidden(true)` drops
  every pane's context (via `Pane.setHidden` → `WebglAddon.dispose`), and
  crucially **latches the hidden state on the grid** so a pane *opened into an
  already-hidden tab* (a background orchestrator spawn) also refuses a context —
  `Pane.tryWebgl` no-ops while `hiddenTab` is set. Result: **live GL contexts ≈
  the panes in the *active* tab only, regardless of how many tabs are open.**
  (The prototype dropped contexts only for panes present at switch time, so
  background-spawned hidden panes leaked one each — that gap is closed.)
- **Context restore is safe even under exhaustion.** On show, `tryWebgl` recreates
  the context; if the GPU is out of contexts and creation fails, the pane falls
  back to xterm's DOM renderer (the `onContextLoss` handler disposes the addon
  and clears the handle). Rendering degrades, correctness does not.
- **Scrollback is unchanged (10k lines/pane).** Lowering it for hidden tabs would
  lose history on switch-back for no real memory win against the buffer's actual
  footprint, and would fight the no-resize invariant. The backend PTY ring
  (256 KB) is independent and always retained, so nothing is lost regardless.

**Rough cost** (order-of-magnitude, to bound expectations): for *T* tabs of *M*
panes each at 80×(24+10 000) cells, xterm's cell buffers are on the order of
`T · M · ~3 MB` of JS heap (≈ tens of MB for a handful of busy tabs) — the
accepted memory cost. GL contexts, the capped resource, stay at *M* (active tab)
rather than `T · M`, so tab count scales without exhausting the GPU. PTY/OS
process cost is per-agent and unchanged by tabs — tabs make it *easy* to keep
many groups alive, which is why the per-tab **Pause** exists (below).

## Preview pipeline + sanitizer

Hovering a background tab shows a live thumbnail compositing its **whole layout**.
Because a hidden tab is not painted (and holds no GL context), the preview can't
read pixels — it serializes the in-memory buffer instead:

1. `Grid.layoutSnapshot()` returns the split tree (direction + flex weights +
   panes) from memory — valid while hidden, no geometry read.
2. `Workspace.previewLayout()` serializes each leaf pane's viewport to HTML,
   **capped at `PREVIEW_PANE_CAP` = 8 panes** per refresh (extras render as a
   titled `(preview capped)` placeholder); docked panes aren't shown.
3. `TabBar` renders a nested flex composite mirroring the layout, re-serializing
   every **`PREVIEW_REFRESH_MS` = 700 ms while hovered** (so a running prompt
   streams in), one tab at a time, stopping the instant the pointer leaves.

**Cost is bounded and documented:** the only expense is serialization, which runs
only while a single background tab is hovered, on ≤ 8 panes, at ~1.4 Hz — a
trivial slice of one frame budget. Degradation on a huge grid is the pane cap,
never unbounded work.

### One shared scale across the composite

Each pane is serialized at **its own** terminal `cols×rows` — whatever it was
last laid out at (a hidden pane is never re-fitted, per the invariant), or 80×24
if it was never laid out. Scaling each mini-pane to fit its *own* cell
independently therefore gave every pane a **different effective font size**: a
pane last laid out full-width shrank to an illegible, sub-pixel smear (its
background-colored rows collapsing into horizontal bars) beside an 80-col pane at
readable size (#63 review finding).

The composite now renders **every** mini-pane at **one** shared scale
(`compositeScale`, pure + tested in `tabroute.ts`): the **median** of the panes'
per-cell fits, clamped to `[PREVIEW_MIN_SCALE, 1]`. The scale is always a single
uniform `scale()` — glyph aspect is preserved, never squished. Median makes it
robust to a **stale/oversized-dims** pane: rather than that one pane dragging the
whole composite down to an illegible fit (what `min` would do), the composite
stays at the typical readable scale and the oversized pane simply **crops** to
its cell (cells are `overflow:hidden`) — crop, never squish; panes that would fit
larger letterbox. `PREVIEW_MIN_SCALE` floors the scale off the sub-pixel range
where the smear appeared. **Decision for the never-laid-out / stale-dims pane:**
it is rendered at its own cols at the shared scale and cropped, *not* specially
clamped by dims — the median already neutralizes its influence, so no magic
dimension cap is needed.

### Why `serializeAsHTML`, and the sanitizer

The string serializer emits cursor-forward escapes (`ESC[nC`) to skip blank
cells, so stripping ANSI collapses runs of spaces (`Please count` →
`Pleasecount`). `@xterm/addon-serialize`'s `serializeAsHTML` instead emits a
literal space per blank cell and a per-run `<span style='color:…'>`, preserving
**spacing and color**.

But that HTML is **untrusted**: the addon does *not* escape cell text, and a
terminal can print any bytes. The tab bar rebuilds it **safely**, never touching
`innerHTML` of the raw string:

- Parse the addon output **detached** (`DOMParser`, no live insertion).
- Re-emit each cell run as a `<span>` whose text is set via **`textContent`**
  (auto-escaped — no markup from cell bytes can execute).
- Apply only a **whitelisted, value-sanitized** inline style, via the pure
  `safeStyleDeclarations` (`tabroute.ts`): properties limited to
  `SAFE_STYLE_PROPS` (color / background-color / font-weight / font-style /
  text-decoration / opacity / visibility), and values rejected if they contain
  `< > { }`, `url(`, `expression`, or `javascript:`.

Keeping the whole rule in one pure function lets a security reviewer read it in
one place and lets `node --test` prove the whitelist and the value guards against
injection attempts (`test/tabroute.test.ts`).

## Tests

| Area | File |
| --- | --- |
| No-resize invariant (hidden ⇒ no resize) | `test/panefit.test.ts` |
| TabManager: add/remove/switch, active invariant, never-zero-tabs, switch-is-hide-not-dispose, group routing + forget-on-close | `test/tabs.test.ts` |
| Cross-tab attention folding, live cross-tab pane lookup (`findPaneByPty`), preview cap edge, composite scale (median / outlier-crop / clamp), preview sanitizer (whitelist + injection rejection) | `test/tabroute.test.ts` |
| Persistence round-trip + schema validation + clamp/coerce edges | `test/tabstore.test.ts` |
| Backend atomic write + corrupt quarantine + migration-safe overwrite | `uistate.rs` (inline) |

## Relationship to the prototype

The prototype (draft PR #148, Option A, five phases) validated the direction at
demo quality. This feature productionizes it: persistence moved from
`localStorage` to the atomic backend store; session-restore became real
tab-routing; the GL-drop policy was closed to cover background-spawned panes;
the preview cost was formalized and its sanitizer extracted + tested; and every
prototype TODO/stub was resolved or documented as a deliberate decision (the
boot-revive scope above). The prototype's separate demo walkthrough has been
retired — its content now lives in this note plus the README user section
([Project tabs](../../README.md#project-tabs)).

## Interaction with maximize (#155)

A background (orchestrator-driven) spawn preserves a maximized pane rather than
collapsing the human's fullscreen view (#155): `openPane` keeps `.has-maximized`
and re-lifts the maximized element after growing the split tree. This composes
with tabs because it is **already per-workspace**: the CSS selector is
`.grid-root.has-maximized > :not(.maximized)` (a class on *each* workspace's grid
root, not a single `#grid-root` id), and the re-lift is `this.rootEl.appendChild`
where `this.rootEl` is the grid's own per-tab root. So a spawn into a **hidden**
tab that has a maximized pane keeps that tab fullscreen (revealed on switch-back),
its new pane lands in the hidden, zero-width subtree (no fit → no PTY resize —
the invariant holds), and its WebGL context is withheld by the hidden-tab GL
policy above — all three properties independent and intact. The pure decision
(`shouldPreserveMaximize`) is unit-tested in `test/panefocus.test.ts`; the
per-workspace DOM wiring is validated by hand (no single-root global to make
tab-aware — it never was one).
