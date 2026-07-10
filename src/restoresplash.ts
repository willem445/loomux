// The cold-boot restore splash (#194 P4). On reopen with a persisted session and
// no remembered preference, ask whether to bring back the last session — every
// tab, its pane layout, and (where possible) the agent sessions that were live —
// or start fresh. This is the demo-requested prompt.
//
// The DECISION is pure (restoredecision.ts's decideRestore); this module is only
// the overlay DOM that collects the human's answer and whether to remember it.
// It's plain content shown over an empty app before any PTY exists, so it never
// resizes a terminal (constraint 1) — an overlay, not a pane/layout change.

/** The human's answer to the restore splash. */
export interface SplashChoice {
  /** Restore the last session (true) or start fresh (false). */
  restore: boolean;
  /** Remember this choice so future boots skip the splash. When false the
   *  preference stays "ask" and the splash returns next launch. */
  remember: boolean;
}

/** Show the restore splash and resolve with the human's choice. Resolves exactly
 *  once — when they click Restore or Start fresh (or press Enter/Esc) — and the
 *  overlay removes itself. `restoreDefault` focuses the recommended button. */
export function showRestoreSplash(host: HTMLElement = document.body): Promise<SplashChoice> {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "restore-splash";

    const card = document.createElement("div");
    card.className = "restore-splash-card";

    const title = document.createElement("h2");
    title.className = "restore-splash-title";
    title.textContent = "Restore your last session?";

    const body = document.createElement("p");
    body.className = "restore-splash-body";
    body.textContent =
      "loomux can bring back every tab and its pane layout, and reopen the agent " +
      "sessions that were live — resuming a session loads its context but spends " +
      "nothing until you send a prompt. Orchestration groups come back dormant, " +
      "with a Resume button.";

    const rememberWrap = document.createElement("label");
    rememberWrap.className = "restore-splash-remember";
    const remember = document.createElement("input");
    remember.type = "checkbox";
    remember.checked = true; // first-run: ask once, then remember (recommended)
    const rememberText = document.createElement("span");
    rememberText.textContent = "Remember my choice";
    rememberWrap.append(remember, rememberText);

    const actions = document.createElement("div");
    actions.className = "restore-splash-actions";
    const freshBtn = document.createElement("button");
    freshBtn.className = "restore-splash-btn";
    freshBtn.type = "button";
    freshBtn.textContent = "Start fresh";
    const restoreBtn = document.createElement("button");
    restoreBtn.className = "restore-splash-btn primary";
    restoreBtn.type = "button";
    restoreBtn.textContent = "Restore";
    actions.append(freshBtn, restoreBtn);

    card.append(title, body, rememberWrap, actions);
    overlay.appendChild(card);

    let settled = false;
    // `rememberOverride` forces the remembered flag off for a non-committal
    // dismissal (Esc): a habitual Escape must not permanently disable the whole
    // feature and clobber the saved session — it's a one-time "fresh this boot",
    // and the splash returns next launch (#194 P4 MED-4). A button click honors
    // the checkbox.
    const finish = (restore: boolean, rememberOverride?: boolean): void => {
      if (settled) return;
      settled = true;
      document.removeEventListener("keydown", onKey, true);
      overlay.remove();
      resolve({ restore, remember: rememberOverride ?? remember.checked });
    };
    // Enter confirms the recommended action (Restore, honoring the checkbox); Esc
    // is a non-committal one-time fresh (never remembered). Captured so the choice
    // can't leak to the app underneath before it's built.
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === "Enter") {
        e.preventDefault();
        finish(true);
      } else if (e.key === "Escape") {
        e.preventDefault();
        finish(false, false);
      }
    };
    restoreBtn.addEventListener("click", () => finish(true));
    freshBtn.addEventListener("click", () => finish(false));
    document.addEventListener("keydown", onKey, true);

    host.appendChild(overlay);
    restoreBtn.focus();
  });
}
