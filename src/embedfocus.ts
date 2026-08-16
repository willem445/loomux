// The pure half of the embed FOCUS-REQUEST hook (#1091 slice C) — DOM/Tauri-free
// so it can be unit-tested (`test/embedfocus.test.ts`); `pane.ts` owns the wiring.
//
// WHAT THE HOOK IS FOR. Two of a pane's embeds now cite each other: a NEEDS-YOU
// card that names a board task (`t-7`) should take you to that row, and — once
// #1091 slice G lands the board marker — a board row held by a question should
// take you back to the card. Both are embeds on the SAME orchestrator pane, so
// neither direction is a backend round trip; it is one pane opening its own
// other view and telling it which item the human asked for.
//
// WHY IT NEEDS STATE AT ALL. The obvious spelling — "open the view, then call
// `focusItem(id)` on it" — cannot work here, because every embed is LAZILY
// CONSTRUCTED (`ensureTasksView`, `ensureDecisionsView`, …) and, once
// constructed, renders from an ASYNC refresh. At the instant the request is
// made the target view may not exist, and even when it does its list may not
// yet hold the row being asked for. So the request is parked, and the view
// drains it on its own next render, when the rows are actually there. That is
// the "survives lazy construction" property this module exists to hold.
//
// CONSUMED EXACTLY ONCE, and that is the load-bearing rule. A view re-renders
// constantly (an `orch-tasks-changed` burst, a poll, a human edit), so a
// request that stayed set would re-scroll and re-highlight on every one of
// them — yanking the viewport back long after the human had scrolled somewhere
// else. `take` is therefore destructive, and `peek` exists only for tests and
// assertions that must not consume.

/** Which embed a focus request is addressed to. A plain `string` rather than
 *  `pane.ts`'s `EmbedKind` so this module stays free of the pane — the same
 *  division `embedtoggle.ts` keeps, and `pane.ts` pins the relationship at its
 *  call sites by passing the narrower type. */
export type FocusKind = string;

/** Parked focus requests, one slot per embed kind.
 *
 *  ONE SLOT, NOT A QUEUE, on purpose: a focus request is "show me this", and
 *  if the human asks twice before the view has rendered once, the thing they
 *  want to see is the SECOND target. Queueing would scroll to a stale row
 *  first and then jump, which reads as a glitch rather than as history. */
export class PendingEmbedFocus {
  private slots = new Map<FocusKind, string>();

  /** Park a request for `kind` to focus `target`, replacing any request for
   *  that kind that has not been drained yet. A blank target is refused
   *  outright (it would consume the slot and then focus nothing), so a caller
   *  that has no id to offer can pass what it has without a guard of its
   *  own. Returns whether anything was parked. */
  request(kind: FocusKind, target: string): boolean {
    const t = target.trim();
    if (!t) return false;
    this.slots.set(kind, t);
    return true;
  }

  /** Take `kind`'s pending target, clearing it — the drain a view calls on
   *  each render. `null` when there is nothing parked, which is the common
   *  case: an ordinary refresh must not re-focus anything. */
  take(kind: FocusKind): string | null {
    const t = this.slots.get(kind);
    if (t === undefined) return null;
    this.slots.delete(kind);
    return t;
  }

  /** What `kind` has parked, without consuming it. For assertions and tests;
   *  never for the render path, which must drain. */
  peek(kind: FocusKind): string | null {
    return this.slots.get(kind) ?? null;
  }

  /** Drop `kind`'s pending request without acting on it — for a view that is
   *  being disposed, so a request parked for a view that will never render
   *  again cannot be picked up by a later instance of it. */
  clear(kind: FocusKind): void {
    this.slots.delete(kind);
  }
}
