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

import { overlayState } from "./overlaystate";

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

  const closeOverlaySlot = overlayState.open();
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
