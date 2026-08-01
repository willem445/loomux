// A small context-menu renderer (#214). DOM wiring only — WHAT the menu contains, and
// what it acts on, is decided by a pure model module (filemenu.ts, panemenu.ts) and
// passed in here already built. This file knows about pixels, focus and dismissal, and
// nothing else.
//
// Generic over the action type (`MenuItem<A>`/`showContextMenu<A>`) so a second caller
// (panemenu.ts, #271) can reuse it without growing a second implementation of "keep it
// on screen, dismiss it on Esc" — this is the reuse the original header comment invited
// but the type wasn't yet generic enough for; filemenu.ts's own `MenuItem`/`MenuAction`
// stay as they are and satisfy `MenuItem<MenuAction>` structurally, so that caller is
// unaffected.
//
// Registers with the shared overlay registry (overlaystate.ts) for as long as a menu is
// open (#391, folded into #380) — a plugin pane's native child webview swallows both
// paint and pointer events under a DOM overlay, so a menu opened over one would
// otherwise render behind it and be unclickable.
//
// SUBMENUS need their own rect (#391 W3). `.ctxmenu-sub` is `position: absolute;
// left: 100%` (styles.css) — it renders to the RIGHT of the root, outside its border
// box, and `getBoundingClientRect()` on the root does not grow to cover an
// absolutely-positioned descendant. So the original wiring registered the root and left
// every open submenu unclipped: "Hash →" / "New →" over a plugin pane were exactly the
// dead menu items #391 was filed about, still dead. Two halves, both needed — the
// registered getter reports the submenu rects (`menuRects` below), and the hover/focus
// edges that open a submenu `poke()` the registry, because CSS `:hover`/`:focus-within`
// opens one with no JS event of its own for a plugin pane to re-clip on.

import { overlayState } from "./overlaystate";
import type { ElementRect } from "./pluginwindow";

/** Generic menu-item shape shared by every context menu in the app. `A` is the
 *  caller's own action union (filemenu.ts's `MenuAction`, panemenu.ts's
 *  `PaneMenuAction`, …) — this module only ever moves it around, never inspects it. */
export interface MenuItem<A> {
  label: string;
  /** Absent on a separator or a submenu parent. */
  action?: A;
  /** A submenu (Hash →, New →). */
  children?: MenuItem<A>[];
  separator?: boolean;
  /** Disabled items are shown greyed with `reason` as a tooltip — an item that is
   *  *inapplicable* stays visible (so the menu doesn't reshuffle under the cursor),
   *  while an item that is *unsupported on this OS* is omitted entirely. */
  disabled?: boolean;
  reason?: string;
}

/** The one menu that can be open. A second `showContextMenu` closes the first — you can
 *  never end up with two, which is otherwise the classic way a stale menu survives and
 *  fires an action against a view that has moved on. */
let openMenu: { el: HTMLElement; dispose: () => void } | null = null;

export function closeContextMenu(): void {
  openMenu?.dispose();
}

/** Show `items` at viewport coords (x, y) and call `onAction` with whatever the user
 *  picks. Resolves nothing — the menu is fire-and-forget; dismissal is silent.
 *
 *  Dismissal: Escape, a click anywhere outside, a scroll/resize, or any other menu
 *  opening. Focus goes INTO the menu so Escape lands here rather than in the pane.
 *
 *  KEYBOARD: Tab / Shift+Tab walk the items (each is focusable), Enter or Space fires the
 *  focused one, and a submenu opens on `:focus-within` — so tabbing INTO one opens it. Esc
 *  closes. Arrow-key navigation is NOT implemented; the doc used to claim it was, which is
 *  the sort of comment that costs someone an afternoon. If it's wanted, it goes here. */
export function showContextMenu<A>(
  x: number,
  y: number,
  items: MenuItem<A>[],
  onAction: (action: A) => void
): void {
  closeContextMenu(); // never two at once

  const root = document.createElement("div");
  root.className = "ctxmenu";
  root.tabIndex = -1;

  const closeOverlaySlot = overlayState.open(() => menuRects(root));
  const cleanups: (() => void)[] = [];
  const dispose = () => {
    if (openMenu?.el !== root) return;
    openMenu = null;
    closeOverlaySlot();
    for (const fn of cleanups) fn();
    root.remove();
  };

  const fire = (action: A) => {
    dispose(); // close FIRST, so an action that opens a dialog isn't behind the menu
    onAction(action);
  };

  root.appendChild(buildLevel(items, fire));
  document.body.appendChild(root);
  placeInViewport(root, x, y);
  root.focus();

  // ---- dismissal ----
  const onDocPointer = (e: PointerEvent) => {
    if (!root.contains(e.target as Node)) dispose();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      dispose();
    }
  };
  // Capture phase: the pane's own handlers must not see the click that dismisses us
  // (right-clicking one row while a menu is open on another would otherwise select both).
  document.addEventListener("pointerdown", onDocPointer, true);
  document.addEventListener("keydown", onKey, true);
  // A scroll or resize moves the anchor out from under the menu — it would then be
  // pointing at nothing. Close rather than float somewhere meaningless.
  window.addEventListener("scroll", dispose, true);
  window.addEventListener("resize", dispose);
  cleanups.push(
    () => document.removeEventListener("pointerdown", onDocPointer, true),
    () => document.removeEventListener("keydown", onKey, true),
    () => window.removeEventListener("scroll", dispose, true),
    () => window.removeEventListener("resize", dispose)
  );

  openMenu = { el: root, dispose };
}

/** Everything this menu currently covers: its own box, plus every submenu panel
 *  (open or not — a closed one is `display: none`, so it measures all-zero and the
 *  registry drops it, which keeps this a plain unconditional read with no
 *  `getComputedStyle` visibility guess). Deliberately a LIST, never a bounding box
 *  around the lot: the root and an open submenu are side by side with dead space
 *  above/below the submenu, and a bounding box would punch that dead space out of the
 *  plugin's webview too — a hole showing nothing. */
function menuRects(root: HTMLElement): ElementRect[] {
  const rects: ElementRect[] = [root.getBoundingClientRect()];
  for (const sub of root.querySelectorAll<HTMLElement>(".ctxmenu-sub")) {
    rects.push(sub.getBoundingClientRect());
  }
  return rects;
}

/** Standalone so every `addEventListener` above shares one function identity — a
 *  fresh closure per row per edge would be four more objects a menu has to carry for
 *  no reason, and these listeners die with the menu's own element anyway. */
function pokeOverlays(): void {
  overlayState.poke();
}

/** One level of the menu (the top level, or a submenu panel). */
function buildLevel<A>(items: MenuItem<A>[], fire: (a: A) => void): HTMLElement {
  const list = document.createElement("div");
  list.className = "ctxmenu-level";

  for (const item of items) {
    if (item.separator) {
      list.appendChild(el("div", "ctxmenu-sep"));
      continue;
    }
    const row = el("div", "ctxmenu-item");
    row.textContent = item.label;
    if (item.reason) row.title = item.reason;

    if (item.disabled) {
      row.classList.add("disabled");
      list.appendChild(row);
      continue;
    }

    if (item.children) {
      row.classList.add("has-sub");
      const sub = buildLevel(item.children, fire);
      sub.classList.add("ctxmenu-sub");
      row.appendChild(sub);
      // Hover/focus opens it; CSS does the showing, so there is no timer to leak.
      row.tabIndex = 0;
      // ...but CSS showing it means there is no event for anyone ELSE either, and a
      // plugin pane re-clips its native webview only when the registry notifies
      // (#391 W3). These four edges are exactly the ones that flip
      // `:hover`/`:focus-within`, so each one is a "my rect set just changed" —
      // `poke()`, not `open()`: the submenu is part of THIS menu's slot, not a
      // second overlay with a lifetime of its own. Cheap by construction, since a
      // menu has a handful of rows and they fire on human pointer motion.
      row.addEventListener("pointerenter", pokeOverlays);
      row.addEventListener("pointerleave", pokeOverlays);
      row.addEventListener("focusin", pokeOverlays);
      row.addEventListener("focusout", pokeOverlays);
      list.appendChild(row);
      continue;
    }

    if (item.action) {
      const action = item.action;
      row.tabIndex = 0;
      row.addEventListener("click", (e) => {
        e.stopPropagation();
        fire(action);
      });
      row.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          e.stopPropagation();
          fire(action);
        }
      });
    }
    list.appendChild(row);
  }
  return list;
}

/** Keep the menu fully on screen: flip it left/up rather than letting it hang off the
 *  edge, which is where a right-click near the window's bottom-right always lands. */
function placeInViewport(root: HTMLElement, x: number, y: number): void {
  // Measure first (it's already in the DOM, off-position).
  root.style.left = "0px";
  root.style.top = "0px";
  const { width, height } = root.getBoundingClientRect();
  const pad = 4;
  const left = x + width + pad > window.innerWidth ? Math.max(pad, x - width) : x;
  const top = y + height + pad > window.innerHeight ? Math.max(pad, y - height) : y;
  root.style.left = `${left}px`;
  root.style.top = `${top}px`;
}

function el(tag: string, cls: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  return e;
}
