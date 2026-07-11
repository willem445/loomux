// A small context-menu renderer (#214). DOM wiring only — WHAT the menu contains, and
// what it acts on, is decided by the pure `filemenu.ts` and passed in here already
// built. This file knows about pixels, focus and dismissal, and nothing else.
//
// Deliberately generic (it takes `MenuItem[]`), so a second caller can reuse it without
// growing a second implementation of "keep it on screen, dismiss it on Esc".

import type { MenuAction, MenuItem } from "./filemenu";

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
 *  opening. Focus goes INTO the menu so the keyboard works immediately (↑/↓/Enter,
 *  →/← for submenus) and so a blur is a reliable "the user went elsewhere" signal. */
export function showContextMenu(
  x: number,
  y: number,
  items: MenuItem[],
  onAction: (action: MenuAction) => void
): void {
  closeContextMenu(); // never two at once

  const root = document.createElement("div");
  root.className = "ctxmenu";
  root.tabIndex = -1;

  const cleanups: (() => void)[] = [];
  const dispose = () => {
    if (openMenu?.el !== root) return;
    openMenu = null;
    for (const fn of cleanups) fn();
    root.remove();
  };

  const fire = (action: MenuAction) => {
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
function buildLevel(items: MenuItem[], fire: (a: MenuAction) => void): HTMLElement {
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
