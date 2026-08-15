// Which agent CLI the pane-setup card is currently describing (#1020 item 4).
//
// WHY THIS EXISTS. The setup card asks the human to pick what a pane becomes, and until
// they hit Create the only statement of that choice is a `<select>` they have to read. The
// human's demo note was "a preview icon of the CLI agent, for quick identification" — i.e.
// the card should SHOW the answer the way a pane header already does, rather than spelling
// it in a dropdown.
//
// The mark itself is NOT this module's business: `agentMark`/`agentMarkFor`
// (src/agenticons.ts, #992) already own "program name → glyph, letter badge, or the neutral
// tier", including the licensing tiering, the injection clamp and the refusal to guess.
// This module owns the ONE question that surface has and the pane header does not: a pane
// header knows what it launched, and a setup card knows only what is half-filled in. So
// what lives here is the mapping from a form mid-edit to the program (if any) it currently
// names — and, just as often, to `null`, because most of the card's states name no agent at
// all and a badge that appeared anyway would be asserting one.
//
// DOM-free on purpose (test/setuppreview.test.ts imports it directly, no jsdom): the form
// hands over raw control values, and every "does this state name a CLI" rule is decided
// here where a test can reach it.
import { agentMark, agentMarkFor, type AgentMarkView } from "./agenticons.ts";
import { isContentKind, type PaneKind } from "./panesetup.ts";

/** The setup card's control values, as far as the preview cares about them. */
export interface SetupPreviewInput {
  kind: PaneKind;
  /** The Agent picker's id — `"claude"`, `"copilot"`, …, or the literal `"custom"`. */
  agentId: string;
  /** Raw text of the custom-command box. Only consulted when `agentId` is `"custom"`. */
  customCommand: string;
  /** The SSH section's remote-CLI id. `""` is its own "None — a plain login shell" entry,
   *  which is a real choice rather than an unfilled field. */
  sshCli: string;
}

/** The Agent picker's escape hatch, whose command the human types themselves. */
const CUSTOM = "custom";

/** The box the setup card draws the preview in.
 *
 *  Deliberately larger than `ICON_AGENT_PX`'s 13, and for the mirror of that constant's own
 *  reason: a pane-header mark sits on the header's 13px toolbar line, and this one sits
 *  beside a card title in a form the human is reading rather than glancing at. It is chrome
 *  inside the setup pane's own body, so its size reaches no terminal — the card is a form,
 *  not a split (CLAUDE.md constraint 1). */
export const ICON_SETUP_PREVIEW_PX = 20;

/**
 * The mark the setup card should draw right now, or `null` for "draw nothing".
 *
 * **`null` is the common answer and it is the load-bearing one.** #992's rule — inherited
 * wholesale here — is that a wrong badge is worse than no badge, because a badge is read as
 * an answer. A setup card spends most of its life in states that name no agent: a terminal,
 * a file explorer, an SSH login shell, a custom command box the human has not typed into
 * yet. Each of those gets nothing, never a `?` — the neutral tier means "loomux cannot tell
 * WHICH agent this is", and saying that about a pane that will run no agent at all is a
 * different, false claim.
 *
 * Per kind, and each rule exists because the one it replaces would lie:
 *
 *   * **content kinds and `terminal`** — nothing. They spawn no CLI at all (`isContentKind`,
 *     and a shell is a shell), so there is no program for a mark to name.
 *   * **`agent`, a built-in CLI** — that CLI's mark, by id. The ids in `AGENTS`
 *     (src/agents.ts) ARE the program names, which is what makes this a lookup rather than
 *     a second table to keep in step.
 *   * **`agent`, `custom…`, box empty** — nothing. The human has picked "I will type a
 *     command" and has not typed it; a neutral badge here would report a failure to
 *     identify something that has not been named yet.
 *   * **`agent`, `custom…`, box filled** — read the command, exactly as a pane does. Which
 *     means a path-qualified or `.exe`-suffixed first token resolves (`programFromRestore`,
 *     via `agentMark`), a shell or transport lands neutral, and gibberish lands neutral.
 *   * **`orchestrator`** — the group's DEFAULT CLI, which is what the top Agent picker is in
 *     that mode. `custom…` is not selectable there (the form re-picks a supported CLI), so
 *     it is refused here too rather than badged `C` for "custom".
 *   * **`ssh`, a remote CLI chosen** — that CLI, as the AUTHORITATIVE answer, which is what
 *     `knownCli` means: the launch line will be the local ssh client, so reading it would
 *     name the transport. `remote` rides along for the same reason it does on a live pane.
 *   * **`ssh`, "None — a plain login shell"** — nothing. Not the remote-unknown neutral
 *     badge: loomux is not failing to identify a far-end agent, it has been told there
 *     isn't one.
 */
export function setupPreviewMark(input: SetupPreviewInput, size?: number): AgentMarkView | null {
  const { kind } = input;
  if (kind === "terminal" || isContentKind(kind)) return null;

  if (kind === "ssh") {
    const cli = input.sshCli.trim();
    return cli ? agentMark({ knownCli: cli, remote: true }, size) : null;
  }

  const agentId = input.agentId.trim();
  if (agentId === CUSTOM) {
    // Orchestrator mode never runs a hand-typed command line — the group's blocks are
    // spawned from a supported CLI's adapter — so `custom` there is a transient value the
    // form is about to overwrite, not a choice with a program behind it.
    if (kind === "orchestrator") return null;
    const command = input.customCommand.trim();
    return command ? agentMark({ command }, size) : null;
  }
  // An empty picker (no id at all) is the same "nothing has been named" state as an empty
  // custom box, and must not be badged `?`.
  return agentId ? agentMarkFor(agentId, size) : null;
}
