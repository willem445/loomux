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
