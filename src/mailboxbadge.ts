// Pure presentation mapping for the manager pane's unread-mail chip (#1161 M5):
// the backend's `orch-mailbox-changed` push (and the `orch_mailbox_status`
// seed) → the label/tooltip on the pane header and the marker on a minimized
// pane's dock chip. DOM-free, like `queuebadge.ts` and `heldbadge.ts`, so the
// wording and the count rules are unit-testable without a webview.
//
// **What the chip is for.** The manager pane is the one pane loomux never types
// into (`doc/design/manager.md` — `deliver_prompt` refuses a `Role::Manager`
// target). News from the orchestrator reaches it by PULL: `message_manager`
// writes a durable row, the manager runs `check_mail()` at the start of its next
// turn, and its next turn is the next time its human speaks to it. So without a
// chip the human's only signal that anything happened is the manager telling
// them after they have already started talking — which is one conversation too
// late to decide whether to have it.
//
// **Why the chip does NOT render the unread cap**, unlike `.pane-queue`'s
// `3/32`. There, the cap is the fact that makes the number legible, because a
// full queue DROPS arrivals. Here nothing is ever dropped: at
// `mailbox::UNREAD_MAX` the WRITER is refused, loudly, with the alternatives
// spelled out in the refusal text (say it in your own pane, `ask_human`,
// `request_attention`) — so the consequence of a full mailbox reaches the human
// on a surface of its own rather than needing to be inferred from a badge. That
// keeps this module's only input the one number the shipped event already
// carries, instead of widening two shipped contracts (the event payload and
// `orch_mailbox_status`'s return) to render a cue for a state that means "you
// have been away for a very long time".

/** The `orch-mailbox-changed` payload, as the backend emits it
 *  (`OrchRegistry::write_mailbox`). */
export interface MailboxChanged {
  group_id: string;
  unread: number;
}

export interface MailboxPresentation {
  /** Header-chip text. Carries the count itself — never hover-only (#813). */
  label: string;
  /** The sentence a hover adds: what the count MEANS, which is the pull model. */
  title: string;
}

/** Normalize a count off the wire. `usize` on the Rust side, so a negative or
 *  non-integer value is impossible from the real backend — but this is the one
 *  place that decides what the chip does with one, and "hide the chip" is the
 *  only answer that cannot render `✉ -1 unread` or `✉ 2.5 unread` at a human. */
function safeCount(unread: number): number {
  if (!Number.isFinite(unread)) return 0;
  return Math.max(0, Math.floor(unread));
}

/** Map an unread count to the header chip's label and tooltip. `null` for an
 *  empty mailbox, which is how the chip is hidden — and how the overwhelmingly
 *  common case (a group with no manager at all posts nothing) costs nothing.
 *
 *  The tooltip states the pull model rather than restating the number, because
 *  the number alone is misread: a human who sees "3 unread" on any other pane
 *  in this app assumes something is being delivered. Here nothing is, by
 *  design, and the human is the clock — so the sentence says both that nothing
 *  is typed into this pane and what will make the manager read. */
export function mailboxPresentation(unread: number): MailboxPresentation | null {
  const n = safeCount(unread);
  if (n === 0) return null;
  const noun = n === 1 ? "update is" : "updates are";
  return {
    label: `✉ ${n} unread`,
    title:
      `${n} ${noun} waiting in this manager's mailbox. Nothing is ever typed into this ` +
      `pane — the manager reads its mail at the start of its next turn, which is the next ` +
      `time you speak to it.`,
  };
}

/** The dock-chip form, for a manager pane the human has minimized: its header —
 *  and so its chip — is out of the DOM entirely.
 *
 *  A manager pane opens un-minimized (it is exempt from `spawn_opens_minimized`),
 *  so unlike the queue marker this is not the common case — it is the same
 *  argument the channel chip's dock mirror makes: minimizing a pane must not
 *  look like the thing the chip was reporting went away. Short by necessity;
 *  the full sentence rides the title the grid already sets. `null` for an empty
 *  mailbox. */
export function dockChipMail(unread: number): { marker: string } | null {
  const n = safeCount(unread);
  return n === 0 ? null : { marker: `✉${n}` };
}

/** The panes an `orch-mailbox-changed` push (or a seed read) applies to.
 *
 *  ONE gate, and it is here rather than at the DOM call site, because the push
 *  is routed by `group_id` alone and a group holds every other role's panes too:
 *  a filter that asked only "same group" would put an unread-mail chip on the
 *  orchestrator and on every worker in the group. The mailbox belongs to exactly
 *  one pane in a group — `MANAGER_MAX` is 1, enforced at parse — so a role test
 *  is not defensive tidying, it is the whole addressing scheme.
 *
 *  A pane with no group id can never match: `groupId` arrives from the backend
 *  and is never empty, but `orchGroupId` is `null` for every plain shell, and
 *  `null === ""` is false in TypeScript either way. */
export function mailboxPanes<T extends { orchGroupId: string | null; orchRole: string | null }>(
  panes: readonly T[],
  groupId: string
): T[] {
  return panes.filter((p) => p.orchGroupId === groupId && p.orchRole === "manager");
}
