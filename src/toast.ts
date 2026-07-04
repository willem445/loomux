// A small, transient, app-level toast — for non-fatal notices (e.g. "no
// editor configured", "failed to launch editor") that don't warrant the
// full-screen fatal banner in main.ts. Auto-dismisses; click to dismiss early.

let toastEl: HTMLElement | null = null;
let toastTimer: number | undefined;

/** Show a transient toast. `kind` tints it: "error" (red) or "info" (neutral). */
export function showToast(message: string, kind: "error" | "info" = "error"): void {
  if (!toastEl) {
    toastEl = document.createElement("div");
    toastEl.className = "app-toast";
    toastEl.addEventListener("click", () => toastEl!.classList.remove("visible"));
    document.body.appendChild(toastEl);
  }
  toastEl.textContent = message;
  toastEl.classList.toggle("info", kind === "info");
  toastEl.classList.add("visible");
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => toastEl?.classList.remove("visible"), 5000);
}
