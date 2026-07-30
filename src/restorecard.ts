// Pure state machine for a dormant restore card's lifecycle (#479). Two
// complaints, one root cause: clicking Resume/Start gave no feedback while
// the backend did real work, and a session that can't be resumed rendered
// as just another neutral dormant card — visually indistinguishable from
// "click here to continue" (#440 was exactly this ambiguity biting once
// already, the other direction: a resumable session wrongly read as dead).
//
// The fix is one small transition table, not per-call-site ad hockery:
//   idle    --click--> pending   (the click is ALWAYS acknowledged immediately)
//   pending --click--> pending   (a second click while in flight is a no-op —
//                                  the #194 P4 MED-3 double-spawn guard,
//                                  generalized to every dormant card, not
//                                  just the group-resume one it was written for)
//   pending --fail---> error     (a failed restore ALWAYS lands here — never
//                                  a spinner that just quietly clears)
//   *       --settle-> idle      (success — the caller usually tears the
//                                  whole card down anyway — or an explicit
//                                  dismiss from the error state)
//   error   --click--> pending   (retry: clicking the primary action again)
//
// DOM-free so the table itself is unit-tested (test/restorecard.test.ts);
// main.ts wires it to the actual dormant-card element (DOM wiring is
// hand-validated per CLAUDE.md, not simulated in tests).

export type RestoreCardStatus = "idle" | "pending" | "error";

export interface RestoreCardState {
  status: RestoreCardStatus;
  /** Diagnostic detail carried into the error state (#440: this text is what
   *  makes a wrongly-unresumable session diagnosable, so it must never be
   *  traded away for a cleaner-looking card) — null outside "error". */
  message: string | null;
}

export type RestoreCardEvent =
  | { type: "click" }
  | { type: "fail"; message: string }
  | { type: "settle" };

/** The card's state at mount, before any click: plain dormant cards (a group
 *  awaiting Resume) start here. */
export const IDLE_RESTORE_CARD_STATE: RestoreCardState = { status: "idle", message: null };

/** A card that already KNOWS at mount time it has nothing to resume (#479 B —
 *  the dormant-agent "Start" card, whose whole reason for existing is a
 *  session with no resumable id) starts directly in the error state, body
 *  text doing double duty as the diagnostic. */
export function errorRestoreCardState(message: string): RestoreCardState {
  return { status: "error", message };
}

/** One transition. Total over every (state, event) pair the DOM wiring can
 *  produce, so a future call site can't silently start skipping a case by
 *  construction — not just by convention. */
export function nextRestoreCardState(state: RestoreCardState, event: RestoreCardEvent): RestoreCardState {
  switch (event.type) {
    case "click":
      // Already in flight: ignore rather than race a second restore in.
      return state.status === "pending" ? state : { status: "pending", message: null };
    case "fail":
      return { status: "error", message: event.message };
    case "settle":
      return IDLE_RESTORE_CARD_STATE;
  }
}
