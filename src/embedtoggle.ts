// Pure decision for the overlay-toggle button/keybinding shared by every
// embeddable view (#361 user-demo finding): while a view is DOCKED, that
// toggle is deliberately disabled rather than fixed to correctly close/reopen
// it. See doc/design/embedded-panels.md's "Overlay toggle vs. dock" section
// for why: the toggle and the dock slot are two independently-driven pieces
// of visibility state for the same view (the toggle flips the view's own
// `hidden` flag; the slot flips its host panel's), and proving every one of
// their many entry points (header buttons, keybindings, a view's own Escape
// handler and internal close button, main.ts's command dispatch) keeps them
// in lockstep is a standing tax future changes can silently violate. Docking
// removes the toggle from the equation entirely instead: a docked view is
// always shown, the only way to make it stop sharing space with the terminal
// is the explicit "Un-embed" menu action, and the toggle works normally again
// once it does.
export type EmbedToggleAction = "noop" | "close" | "open";

/** `docked` wins over `visible`: even a docked-but-not-currently-visible view
 *  (a state nothing can drive it into anymore once THIS guard is in place,
 *  but harmless to handle defensively) never reopens via the toggle either —
 *  the dock is the only thing driving a docked view's visibility now. */
export function embedToggleAction(docked: boolean, visible: boolean): EmbedToggleAction {
  if (docked) return "noop";
  return visible ? "close" : "open";
}

/** The embeddable views whose CONTENT is read from the pane's live cwd — the
 *  only ones for which opening is a statement about a directory (#1042).
 *
 *  `tasks`, `audit`, `group` and `timeline` are deliberately absent: they are
 *  scoped to an orchestration group, whose repo was declared when the group was
 *  created, so opening one says nothing new about the pane's cwd. Kept as data
 *  rather than a condition at the call site so `toggleDeclaresCwd` below has one
 *  answer, and so `pane.ts` can assert at compile time that every member really
 *  is an `EmbedKind`. */
export const CWD_DECLARING_VIEWS = ["git", "issues", "editor"] as const;

/** Does this toggle gesture DECLARE the pane's cwd as a root? (#1042)
 *
 *  **Only on the `open` direction, and that is the whole point.** A pane's cwd
 *  arrives on an agent-controllable channel — the OSC-7 report emitted by
 *  whatever process runs in the pane — so the stream itself is resolve-only and
 *  never declares. What may declare is a HUMAN asking for a view *at* that cwd,
 *  which is an open. A `close` is a dismissal and a `noop` is a docked view
 *  refusing the toggle outright; neither is anybody asking for anything, and
 *  treating them as declarations would let an agent that has `cd`'d somewhere
 *  interesting get that directory permanently declared the moment the human
 *  dismissed a panel — the opposite of the rule.
 *
 *  Fail-closed on the kind, too: a view added later declares nothing until it is
 *  added to `CWD_DECLARING_VIEWS` on purpose. `kind` is a plain `string` rather
 *  than `pane.ts`'s `EmbedKind` so this module stays free of the pane; the call
 *  site passes the narrower type and `pane.ts` pins the relationship. */
export function toggleDeclaresCwd(kind: string, action: EmbedToggleAction): boolean {
  return action === "open" && (CWD_DECLARING_VIEWS as readonly string[]).includes(kind);
}
