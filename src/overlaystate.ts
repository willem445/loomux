// Shared "which DOM overlays are open, and where" registry (#391, folded into
// #380 — the corrected root-cause fix superseding the reverted global-hide
// band-aid, PR #392, reverted at d3333b3). A plugin pane's content is a
// NATIVE child webview (pluginpaneview.ts, Window::add_child) that always
// paints ABOVE `main`'s own DOM content within its bounds and swallows
// pointer events there — see `src-tauri/src/pluginregion.rs`'s module doc
// comment for the full root-cause writeup and the composition-hosting spike
// that ruled out a DOM-side-only fix. `pluginregion::plugin_set_occlusion`
// fixes this at the OS level by clipping the plugin's own HWND to exclude
// whatever overlay rects currently cover its pane — this registry is the
// "which rects, right now" half of that: every DOM overlay in the app calls
// `open()` with a live rect-getter when it opens and the returned closer when
// it closes; `currentRects()`/`subscribe()` let PluginPaneView
// (pluginpaneview.ts) recompute and re-clip immediately on every open/close
// edge, no lag.
//
// A class (not a bare module singleton) so tests can each build a fresh,
// isolated instance rather than sharing hidden module state — the same reason
// refreshgate.ts is a class. Production code imports the one shared
// `overlayState` instance below.
//
// Pure/DOM-free by design: `open()` takes an opaque rect-GETTER, never an
// `HTMLElement` or a `ResizeObserver` — the registry itself never touches the
// DOM, so it's unit-tested the same way the pre-#391 boolean version was
// (test/overlaystate.test.ts). The DOM wiring (an overlay's own element,
// `getBoundingClientRect()`, a `ResizeObserver` to `poke()` on a
// resize-while-open) lives at each call site, hand-validated per CLAUDE.md's
// convention for DOM wiring.
//
// ===========================================================================
// THE LEDGER — every covering DOM overlay, wired or deliberately excluded
// ===========================================================================
//
// EVERY covering DOM overlay in this codebase is either wired into this
// registry or deliberately excluded below — the point of listing exclusions
// explicitly is that a reviewer (or a future contributor adding a new
// overlay) can check "is this one already covered?" without re-deriving the
// reasoning from scratch. A ledger is not self-maintaining: it was true when
// #380 wrote it and had to be RE-DERIVED from scratch against a moved tree
// (#391 W3, 2026-08-01) — see "How to re-derive this list" at the bottom, and
// do that rather than trusting these entries when you have reason to doubt
// them.
//
// --- Wired: `open()` + the returned closer ---------------------------------
//   - modal.ts — `modal()` and `promptModal()` (two `.launcher-overlay`
//     backdrops); every confirm/prompt in the app routes through one of them.
//   - editor.ts — `editorConfigDialog`, an independent copy of the same
//     `.launcher-overlay` pattern rather than a modal.ts caller.
//   - contextmenu.ts — `showContextMenu`, the shared `.ctxmenu`. Registers
//     the root's rect AND every open `.ctxmenu-sub` panel's, as a multi-rect
//     `OverlayRectSource`, plus a `poke()` on the hover/focus edges that open
//     one (#391 W3 — see `OverlayRectSource`'s own doc comment for why the
//     root's rect alone was not enough and why a bounding box would be wrong).
//   - gitview.ts — its own hand-rolled `.git-menu` (not a contextmenu.ts
//     caller).
//   - tabbar.ts — all three body-level popovers: `.tab-menu` (right-click
//     menu), `.tab-preview` (hover thumbnail), `.tab-palette` (color picker).
//   - toast.ts — `showToast`'s `.app-toast`.
//   - main.ts — `showFatal`'s `#app-error` banner (#391 W3). Same geometry
//     and z-index as `.app-toast` above and click-to-dismiss: unregistered,
//     the one message a user most needs to read was both invisible AND
//     un-dismissable over a plugin pane.
//   - pane.ts — every in-pane view that opens FLOATING, wired ONCE in
//     `openView`'s floating branch rather than per-toggle, so the set is
//     `EMBED_KINDS` (tasks, git, issues, audit, group, editor — six today)
//     and a seventh kind is wired the day it is added, with nothing to
//     remember. The DOCKED mode of the same views is deliberately NOT
//     registered: a docked panel is a flex sibling of the terminal, so it
//     changes a plugin pane's content BOX (that pane's own ResizeObserver's
//     job) instead of covering it. `closeView`'s `else` branch has the same
//     note from the other side.
//
// Unlike the reverted global-hide PR (a single global boolean, so wiring a
// toast would have hidden EVERY plugin pane in the app for the toast's ~5s
// lifetime even where it doesn't visually overlap one at all), this registry
// is per-rect (the whole point of the #391 redo — see pluginregion.rs), so a
// toast only ever punches a hole the size of the toast itself.
//
// --- Wired differently: `poke()` alone, no slot ----------------------------
// sessions.ts's SessionBrowser calls `poke()` alone, NEVER `open()`/the
// closer (#380 round 2). The sessions sidebar was originally wired the same
// way every other entry on this list is (the bug #391 was reported through),
// but `#sessions` (`styles.css`) turned out to be a flex SIBLING of the pane
// grid, not a `position: absolute`/`fixed` covering layer — it never overlaps
// a plugin pane's rect at any point in its own `width` transition, so it
// never had a meaningful rect to register. `poke()` still does exactly what
// its own doc comment says — "force subscribers to recompute without an
// open/close edge" — it just doesn't require a prior `open()` to make sense:
// any call site whose OWN geometry change should prompt every plugin pane to
// recompute ITS OWN bounds (not this call site's coverage) can call `poke()`
// on its own, registering nothing. `sessions.ts`'s header comment has the
// full writeup.
//
// --- Deliberately excluded -------------------------------------------------
// Not "not done yet" — each of these has a reason it can never need a slot.
//
//   Structurally cannot overlap a plugin pane:
//   - restoresplash.ts's `.restore-splash` boot splash — shown before any
//     pane (let alone a plugin pane) exists.
//   - `.pane-plugin`'s own status line and #377 "Review permissions…" button
//     — the one surface that LOOKS like it must be the exception, since it is
//     the only DOM this registry deals with that is deliberately drawn where
//     a plugin webview goes. It isn't: `PluginPaneView.showConsentRequired`
//     runs only from `open()`'s catch, so the webview does not exist (`ready`
//     is false and `repositionNow` early-returns) and there is nothing over
//     it to clip. It is the covered thing's SUBSTITUTE, not a cover. If a
//     future path ever shows in-pane chrome while `ready` is true, that is a
//     new entry on this list and not a small change.
//   - pluginconsent.ts (#377) — DOM-free; it decides what the consent prompt
//     SAYS, and the prompt itself is rendered by modal.ts, which is already
//     wired above. Nothing to register separately.
//   - launcher.ts's `.pane-welcome` form, and `.pane-dormant`'s restore card
//     — each IS a pane's own persistent content ("closed by closing the pane
//     itself"), not a dismissable overlay that opens over OTHER content the
//     way everything else on this list is. A pane showing either of them is
//     by definition not a plugin pane.
//   - `.pane.maximized` (grid.ts) — not an overlay at all: maximizing sets
//     `display: none` on every other pane, so a plugin pane elsewhere in the
//     tree measures zero and `pluginWindowShouldShow` hides its webview
//     outright. Nothing to clip.
//
//   Bounded by an already-registered rect (registering again would add no
//   coverage the parent hasn't already claimed):
//   - tasksview.ts's nested approve/request-changes dialogs (`.tasks-dialog`,
//     `inset: 0` inside the tasks overlay's box).
//   - gitview.ts's `.git-blank` / `.git-toast` / `.git-modal-backdrop`, all
//     inside the registered `.git-overlay`.
//   - issuesview.ts's `.issues-form-backdrop` / `.issues-detail`, inside the
//     registered issues overlay.
//   - fileedit.ts's CodeMirror find panel (`.cm-panels-top`), floated inside
//     the registered editor view's own box.
//     NB the one case where "inside the parent's box" is NOT enough is a
//     child positioned OUTSIDE it — `.ctxmenu-sub` at `left: 100%`, which is
//     exactly the #391 W3 bug. Check the CSS, don't assume the DOM tree.
//
//   Transient / non-covering by construction:
//   - grid.ts's `.drop-indicator` / `.drag-ghost` — drag-gesture-only,
//     cursor-following, and `pointer-events: none`, so they neither cover a
//     fixed region for any meaningful duration nor take a click. (#402 moved
//     the drag STATE machine into dragsession.ts, but both elements are still
//     created and positioned in grid.ts; dragsession.ts is DOM-free.)
//   - `.pane-voice-indicator` — a badge over its own pane's terminal,
//     non-interactive; a plugin pane has no terminal and no voice capture.
//   - the orchestration compose strip's `.orch-compose-input` /
//     `.orch-compose-chip-x` — chrome positioned inside the compose strip's
//     own relative box, on an orchestrator PTY pane.
//   - clipboard.ts / gitview.ts / issuesview.ts's `execCommand("copy")`
//     scratch `<textarea>`s — off-screen, zero-opacity, removed in the same
//     synchronous turn; never painted, never hit-tested.
//
// --- How to re-derive this list --------------------------------------------
// A covering layer has to come from one of two places, and both are greppable
// in a couple of minutes:
//   1. `grep -n 'document\.body\.\(appendChild\|append\|insertBefore\)' src/*.ts`
//      — every body-level layer, which is every popover/modal/banner.
//   2. `grep -n 'position:\s*\(fixed\|absolute\)' src/styles.css` — then map
//      each selector back to the module that builds it, and ask whether its
//      box escapes an already-registered parent's box.
// index.html declares no covering layer (all flex chrome), and the model
// modules that SOUND like overlays — taskboard.ts, auditwindow.ts,
// workflowstatus.ts, compactionstatus.ts, heldbadge.ts, attention.ts,
// restorecard.ts, settings.ts, dragsession.ts, embedsplit.ts, embedtoggle.ts,
// overlaysize.ts — are all DOM-free pure modules (that is the repo's
// convention, CLAUDE.md), so they cannot be one. Re-check that claim with
// `grep -c 'document\.' src/<mod>.ts` rather than trusting this sentence.

import type { ElementRect } from "./pluginwindow";

export type OverlayCloser = () => void;

/** What one registered overlay contributes to `currentRects()`, read live on
 *  every call. Usually ONE rect — an overlay is usually one box. An overlay
 *  whose visible area is genuinely several disjoint boxes returns them all
 *  (#391 W3): `contextmenu.ts`'s menu is the case that forced this — a
 *  `.ctxmenu-sub` submenu panel is `position: absolute; left: 100%`, so it
 *  renders OUTSIDE its root's border box, and `getBoundingClientRect()` on
 *  the root does NOT grow to include an absolutely-positioned descendant.
 *  Registering the root alone therefore left every open submenu unclipped,
 *  which over a plugin pane is #391's exact reported symptom (painted behind
 *  the native child webview and dead to the pointer). A bounding box around
 *  both would have been the cheaper fix and the wrong one — it punches a hole
 *  through the plugin where nothing is drawn at all. `null` (or an empty
 *  array) contributes nothing while still counting as open. */
export type OverlayRectSource = () => ElementRect | ElementRect[] | null;

/** Why `subscribe()`'s callback fired — passed through so a subscriber that
 *  cares (PluginPaneView, for its breadcrumb's trigger-source label) doesn't
 *  have to guess; one a subscriber doesn't care about (the pre-#380 shape)
 *  can simply ignore the argument. */
export type OverlayChangeReason = "open" | "close" | "poke";

export class OverlayRegistry {
  private overlays = new Map<number, OverlayRectSource>();
  private nextId = 0;
  private listeners = new Set<(reason: OverlayChangeReason) => void>();

  /** Register that one overlay instance just opened, tracked by a live rect
   *  getter — called fresh every time `currentRects()` runs, never cached, so
   *  an overlay that moves/resizes while open is always read correctly
   *  without a separate "moved" event of its own. Returns the matching
   *  closer — call it exactly once, on whichever path ends THIS overlay's
   *  lifetime (a close button, Escape, an outside click, the owning pane
   *  disposing while it's open — every one of them, not just the "normal"
   *  close). Idempotent: calling the returned closer more than once only
   *  removes it on its first call.
   *
   *  ONE overlay, however many rects: an overlay whose visible area is
   *  several disjoint boxes registers ONE slot returning all of them
   *  (`OverlayRectSource`), not one slot per box — the slot's lifetime is the
   *  overlay's, and a sub-box that appears and disappears WHILE the overlay
   *  is open (a submenu on hover) has no open/close edge of its own to hang a
   *  second slot off. `poke()` is how such a call site announces that its own
   *  rect SET just changed. */
  open(getRect: OverlayRectSource): OverlayCloser {
    const id = this.nextId++;
    this.overlays.set(id, getRect);
    this.notify("open");
    let closed = false;
    return () => {
      if (closed) return;
      closed = true;
      this.overlays.delete(id);
      this.notify("close");
    };
  }

  /** Whether at least one overlay is currently open. */
  get isOpen(): boolean {
    return this.overlays.size > 0;
  }

  /** How many overlays are currently open (for assertions/debugging). */
  get openCount(): number {
    return this.overlays.size;
  }

  /** Every currently-open overlay's rect(s), read live right now — never a
   *  stale snapshot. An overlay whose getter returns null (e.g. its element
   *  was detached from the document without going through its own close
   *  path) contributes nothing rather than throwing.
   *
   *  Empty rects are dropped here rather than at each call site: an overlay
   *  sub-box that is currently `display: none` — a closed `.ctxmenu-sub`, a
   *  hidden banner still holding its slot — measures as an all-zero rect at
   *  the viewport origin, and a pane whose own rect starts at (0,0) would
   *  otherwise take a 0-size "hole" from it. `pluginregion.rs` would clip
   *  nothing for a 0-size rect either way, so this is defence in depth, not
   *  the only thing standing between a phantom rect and a wrong clip — but it
   *  means a call site can return its whole set unconditionally (the
   *  `contextmenu.ts` shape: root + every submenu panel, open or not) instead
   *  of each one re-deriving "is this sub-box actually showing?". */
  currentRects(): ElementRect[] {
    const out: ElementRect[] = [];
    for (const getRect of this.overlays.values()) {
      const r = getRect();
      if (!r) continue;
      for (const one of Array.isArray(r) ? r : [r]) {
        if (one && one.width > 0 && one.height > 0) out.push(one);
      }
    }
    return out;
  }

  /** Be told every time the open set changes (not just the open/closed edge,
   *  and not just from `open()`/the closer — `poke()` below fires it too) —
   *  a PluginPaneView subscriber only needs "something might have changed,
   *  recompute", so any edge is enough. Returns an unsubscribe. */
  subscribe(fn: (reason: OverlayChangeReason) => void): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** Force subscribers to recompute without an open/close edge — a call site
   *  whose OWN registered overlay can resize/move WHILE open (a
   *  `ResizeObserver` on its own element, say) calls this instead of
   *  re-opening a new slot. Does NOT require a matching `open()`, though (#380
   *  round 2): a call site whose geometry change affects every plugin pane's
   *  own BOUNDS rather than what covers them — never overlapping a pane, so
   *  never a rect worth registering — can call `poke()` on its own, with no
   *  `open()` at all. `sessions.ts`'s `SessionBrowser` is that case; see its
   *  header comment.
   *
   *  Third caller shape (#391 W3): an overlay whose rect SET grows or shrinks
   *  while it stays open — `contextmenu.ts`'s submenu, which opens and closes
   *  purely on CSS `:hover`/`:focus-within` and so produces no `open()`/close
   *  edge of its own. Without a `poke()` the registry's CONTENTS would be
   *  right (the live getter reports the submenu) but nothing would ASK: a
   *  plugin pane re-clips only when notified, so the submenu would stay
   *  unclipped until some unrelated later trigger happened along. The
   *  multi-rect getter and the poke are two halves of one fix; either alone
   *  leaves the bug. */
  poke(): void {
    this.notify("poke");
  }

  private notify(reason: OverlayChangeReason): void {
    for (const fn of this.listeners) fn(reason);
  }
}

/** The one registry every loomux overlay call site shares. */
export const overlayState = new OverlayRegistry();
