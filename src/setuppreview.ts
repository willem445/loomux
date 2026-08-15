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
  /**
   * In orchestrator mode: the CLI the orchestrator pane will actually run, ALREADY RESOLVED
   * — `orchestratorCliOf(roster, groupCli)` (src/roster.ts). `null` means the answer is not
   * available yet, and this module draws nothing for it. Ignored for every other kind.
   *
   * **A resolved answer rather than a control, and that distinction is the fix** (#1020
   * rev-740 blocking 1 and 1b). This preview was wrong twice, both times because it read a
   * form control while the launch reads the resolved roster: first the group-default picker
   * when the pane launches on the per-role one, then the per-role select when the advanced
   * toggle makes `create_group_ex` discard the form's blocks for the declared file's
   * entirely. Every such fix names a control and produces the next twin. Taking the roster's
   * own answer ends the class — the two cannot disagree, because there is only one
   * resolution and the human is already reading its output in the roster box below.
   *
   * Hence `null` is a real state rather than a defensive `?`: a roster resolves
   * asynchronously (the backend reads the repo), and a badge drawn from the previous repo's
   * answer would be the same confident-wrong-answer one refresh later.
   */
  orchestratorCli: string | null;
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
 *   * **`orchestrator`** — the CLI the RESOLVED ROSTER gives the orchestrator block, handed
 *     in already worked out (`orchestratorCliOf`); no form control is read. That roster is
 *     the form's per-role picks in the ordinary case and the declared workflow file's blocks
 *     when the advanced toggle is on, which is exactly the substitution the backend makes —
 *     so the badge cannot disagree with the pane, or with the roster box beneath it. Nothing
 *     resolved yet ⇒ nothing drawn. `custom…` is refused, never badged `C`.
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

  if (kind === "orchestrator") {
    // Whatever the roster resolved, and nothing else — no control is consulted here, which
    // is the point (see `orchestratorCli`). `null` (not resolved yet, or a roster naming no
    // orchestrator) draws nothing rather than falling back to a form value: a fallback is
    // how this went wrong twice.
    const cli = input.orchestratorCli?.trim();
    // Orchestrator mode never runs a hand-typed command line — the group's blocks are
    // spawned from a supported CLI's adapter — so `custom` is a transient value the form is
    // about to overwrite, not a choice with a program behind it.
    return cli && cli !== CUSTOM ? agentMarkFor(cli, size) : null;
  }

  if (agentId === CUSTOM) {
    const command = input.customCommand.trim();
    return command ? agentMark({ command }, size) : null;
  }
  // An empty picker (no id at all) is the same "nothing has been named" state as an empty
  // custom box, and must not be badged `?`.
  return agentId ? agentMarkFor(agentId, size) : null;
}
