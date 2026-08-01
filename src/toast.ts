// A small, transient, app-level toast — for non-fatal notices (e.g. "no
// editor configured", "failed to launch editor") that don't warrant the
// full-screen fatal banner in main.ts. Auto-dismisses; click to dismiss early.
//
// Registers with the shared overlay registry (overlaystate.ts) while visible
// (#391, folded into #380): wired THIS time, unlike the reverted global-hide
// PR #392, which deliberately excluded toasts — that registry was a single
// global boolean, so wiring one would have hidden every plugin pane in the
// app for the toast's ~5s lifetime even where it doesn't visually overlap
// one at all. This registry (overlaystate.ts) is per-rect, so a toast now
// only ever punches a hole the size of the toast itself — the cost that
// justified excluding it no longer applies.

import { overlayState } from "./overlaystate";

let toastEl: HTMLElement | null = null;
let toastTimer: number | undefined;
let overlayClose: (() => void) | null = null;

function dismiss(): void {
  toastEl?.classList.remove("visible");
  overlayClose?.();
  overlayClose = null;
}

/** Show a transient toast. `kind` tints it: "error" (red) or "info" (neutral). */
export function showToast(message: string, kind: "error" | "info" = "error"): void {
  if (!toastEl) {
    toastEl = document.createElement("div");
    toastEl.className = "app-toast";
    toastEl.addEventListener("click", dismiss);
    document.body.appendChild(toastEl);
  }
  toastEl.textContent = message;
  toastEl.classList.toggle("info", kind === "info");
  toastEl.classList.add("visible");
  overlayClose ??= overlayState.open(() => toastEl!.getBoundingClientRect());
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(dismiss, 5000);
}
