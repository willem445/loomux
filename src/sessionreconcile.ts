// Pure post-start session-id reconciliation (#440 D1 option B) + the D2
// dormant-card resume candidate. DOM/IPC-free, like groupresume.ts and
// panerestore.ts: main.ts builds these plain-data projections from live
// Panes and the already-fetched listSessions() result, and applies the
// result via Pane.adoptSessionId — this module never touches a Pane, the
// DOM, or the backend, so the matching/refusal LOGIC is unit-tested without
// any of that.
//
// WHY THIS EXISTS. Option A (panerestore.ts's adoptableSessionId) only
// covers a custom command line that already NAMES its session via
// `--session-id`/`--resume`. A bare `claude` custom line mints its OWN id
// with nothing on the command line to read — the only way loomux can learn
// THAT id is to watch what listSessions() turns up for the pane's cwd+CLI
// after it's had a chance to produce a transcript, and match it back. That's
// what this module plans; main.ts (#342-safe: never on the boot path, only
// after a lazy re-scan) executes the plan.
//
// THE REFUSAL IS THE POINT. Two same-CLI panes open on the same folder (a
// legitimate setup — two customer claude sessions in one repo) can each
// match more than one session, or one session can match more than one pane.
// Silently picking one — "newest wins" — would cross-wire a pane onto
// SOMEONE ELSE'S conversation history, with no way for the human to notice
// until they read something that isn't theirs. That failure mode is strictly
// worse than today's bug (a pane that stays dormant forever, which the D2
// card below already gives a human-driven fix for), so ANY contested
// candidate — for any pane it touches — is refused outright rather than
// guessed. Worst case after a refusal is exactly today's status quo.

/** The CLIs a `listSessions()` row can name (`SessionInfo["source"]`) — the
 *  set this module matches panes against. `opencode` joined it with #722's
 *  scanner: its rows arrive with a cwd and a session id like any other, so the
 *  matching and refusal logic below applies unchanged, and leaving it out
 *  would silently make every opencode pane unadoptable while the sidebar
 *  listed the very session it should have adopted. */
export type Cli = "claude" | "copilot" | "opencode";

/** Just enough of a `listSessions()` row to match against, for both functions
 *  below. main.ts maps the backend's `SessionInfo[]` (source→cli, modified_ms
 *  →modifiedMs) into this shape once per reconcile pass. */
export interface SessionRecord {
  id: string;
  cli: Cli;
  cwd: string;
  modifiedMs: number;
  /** Carried through only for the D2 card's display copy — unused by the
   *  matching logic itself. */
  title: string;
  /** The scanner's own ready-to-run resume command (`SessionInfo.resume_command`
   *  — e.g. `claude --resume <id>`), carried through only for the D2 card's
   *  fallback when the dormant placeholder recorded no command/argv of its
   *  own to rewrite via `agentResumeCommand`. Unused by the matching logic. */
  resumeCommand: string;
}

/** Just enough of a live, null-id agent pane to decide adoption — main.ts
 *  maps its null-`sessionId` agent panes to this shape. `key` is caller-
 *  defined (main.ts uses whatever it can map back to the live Pane) and is
 *  never interpreted here. */
export interface ReconcilePane {
  key: string;
  cli: Cli;
  cwd: string;
  /** `Pane.firstInputAt` — when the HUMAN's first input (keystroke/paste)
   *  reached this pane's current process, NOT when the process was spawned
   *  (review round 2, B2). A session transcript is only created once
   *  prompted (#194 BUG-1's "never prompted → no transcript" fact) — gating
   *  on spawn time instead left a pane adoption-eligible for its ENTIRE
   *  idle-before-first-prompt lifetime (seconds to hours), during which any
   *  unrelated same-CLI/same-cwd session modified after spawn (a sibling
   *  terminal, a pane closed moments earlier) was a sole, UNCONTESTED false
   *  match — refusal-on-contest can't fire when there's nothing to contest
   *  against. Gating on first input instead collapses that window to the
   *  genuinely narrow race where the pane's OWN transcript is also usually
   *  in flight (making a collision CONTESTED, and therefore refused — see
   *  the design note's accepted-residual section). */
  eligibleSinceMs: number;
}

export interface SessionAdoption {
  key: string;
  sessionId: string;
}

/** Case- and separator-insensitive cwd equality (Windows: `C:\repo`,
 *  `c:/repo/`, and `C:\REPO\` are the same folder). Backslash-vs-forward-
 *  slash and a trailing separator are the two forms `cwd` shows up in across
 *  this codebase — a raw pane cwd vs whatever the CLI's own scan wrote. */
function normalizeCwd(cwd: string): string {
  return cwd.trim().replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

/** Plan which null-id panes get which session id adopted (#440 D1 option B).
 *  A pane adopts a session only when it has EXACTLY ONE candidate left after
 *  filtering, AND that candidate isn't ALSO a candidate for some other pane
 *  in this same call — ambiguity in either direction refuses the whole
 *  contested set (see the module comment) rather than guessing. Refusal is
 *  computed up front across every pane so the OUTCOME never depends on the
 *  order `panes` is given in.
 *
 *  A candidate must: match the pane's CLI exactly (never cross claude/
 *  copilot); match the pane's cwd (normalized); have `modifiedMs` no earlier
 *  than `eligibleSinceMs` — STRICT, no tolerance/slack of any kind (review
 *  round 2, B1: `eligibleSinceMs` and a transcript's mtime are both read off
 *  the same OS clock at a point strictly before the CLI could have written
 *  that transcript, so nothing legitimate ever lands before it; any slack
 *  can only ever admit a session that predates — and therefore cannot
 *  belong to — this pane, which is exactly the wrong direction for the
 *  settled refuse-on-ambiguity policy: it WIDENS acceptance, when every
 *  other knob in this module narrows it); and not already be in `claimed` —
 *  session ids already recorded on some OTHER pane, live or persisted, so a
 *  second null-id pane in the same folder can't re-adopt an id that's
 *  already spoken for. */
export function planSessionAdoption(
  panes: readonly ReconcilePane[],
  sessions: readonly SessionRecord[],
  claimed: ReadonlySet<string>
): SessionAdoption[] {
  const candidatesFor = (pane: ReconcilePane): SessionRecord[] =>
    sessions.filter(
      (s) =>
        s.cli === pane.cli &&
        normalizeCwd(s.cwd) === normalizeCwd(pane.cwd) &&
        s.modifiedMs >= pane.eligibleSinceMs &&
        !claimed.has(s.id)
    );

  const perPane = panes.map((pane) => ({ pane, candidates: candidatesFor(pane) }));

  // How many DISTINCT panes (in this call) each session id is a candidate
  // for. >1 means it's contested and gets refused everywhere it appears —
  // computed once, up front, so no pane's own position in the array can
  // change another pane's outcome.
  const claimCounts = new Map<string, number>();
  for (const { candidates } of perPane) {
    for (const s of candidates) claimCounts.set(s.id, (claimCounts.get(s.id) ?? 0) + 1);
  }

  const out: SessionAdoption[] = [];
  for (const { pane, candidates } of perPane) {
    const uncontested = candidates.filter((s) => claimCounts.get(s.id) === 1);
    if (uncontested.length !== 1) continue; // none survive, or this pane itself has >1 → refuse
    out.push({ key: pane.key, sessionId: uncontested[0].id });
  }
  return out;
}

/** The newest session record matching a dormant placeholder's CLI + cwd
 *  (#440 D2) — the pane has NO recorded id (a bare custom launch caught
 *  before a prompt, or one the reconciler above hasn't run against yet) but
 *  the folder DOES have resumable history, so the dormant card can offer
 *  "resume the most recent match" alongside plain Start instead of a dead
 *  end. Unlike `planSessionAdoption`, this is advisory — a human clicks it,
 *  or doesn't — rather than an automatic identity change, so it doesn't need
 *  the ambiguity refusal above: "resume the most recent session in this
 *  folder" IS what newest-wins means here, chosen BY the human reading the
 *  card, not silently on their behalf.
 *
 *  Null when the record has no cwd (nothing to match against) or no session
 *  in the folder matches this CLI. */
export function dormantResumeCandidate(
  record: { cli: Cli; cwd: string | null },
  sessions: readonly SessionRecord[]
): SessionRecord | null {
  if (!record.cwd) return null;
  const cwd = record.cwd;
  const matches = sessions.filter((s) => s.cli === record.cli && normalizeCwd(s.cwd) === normalizeCwd(cwd));
  if (!matches.length) return null;
  return matches.reduce((newest, s) => (s.modifiedMs > newest.modifiedMs ? s : newest));
}
