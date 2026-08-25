// The session browser's "Orchestrations" section (#1563), as a pure function.
//
// WHAT THIS SECTION IS FOR. Every other route into a recorded orchestration
// goes through a CLI's own session store: the sidebar's session list is a scan
// of `~/.claude/projects`, `~/.copilot/session-state` and the human's GLOBAL
// opencode store. A group's opencode sessions are NOT in that global store —
// they live in `<group>/opencode/opencode.db`, deliberately excluded because a
// bare `--session` pane restored from one would be powerless (see
// `doc/design/opencode.md`). Before #1563 that left a fresh opencode
// orchestrator with no clickable route to the backend resume at all. This
// section reads loomux's own record (`orch_list_recorded`) instead, and it
// lists every CLI in one shape, so it is the primary restart surface rather
// than an opencode workaround.
//
// NOT THE ONLY ROUTE ANY MORE, AND THE DIFFERENCE MATTERS. Slice A (#1563)
// landed the other half: a learned session id now reaches its pane via
// `orch-session-learned` and is persisted to `tabs.json`, so a DORMANT GROUP
// CARD can carry an opencode id too. That route depends on the pane having
// been open when the watcher bound the id, and on that tab set surviving.
// This one depends on neither — it reads the group's own `agents.json`, so it
// still reaches a group whose card was never captured, whose tab was closed,
// or that belongs to a tab set this window does not have.
//
// EVERY "NO BUTTON" CASE HAS ITS OWN SENTENCE. The three ways a row cannot be
// resumed are genuinely different problems with genuinely different answers,
// and collapsing them into one greyed-out button tells the human nothing:
// a running group wants its pane focused, an unidentified orchestrator wants
// waiting (or one manual resume), and a session the CLI's store has lost wants
// a fresh orchestrator on the same board. The copy says which.
//
// DOM-free and IPC-free on purpose: `sessions.ts` renders these rows, and the
// ordering, the filter and the copy are unit-tested here.

/** One recorded orchestration group, exactly as `orch_list_recorded` returns
 *  it (`RecordedOrchestration` in `src-tauri/src/orchestration/mod.rs`). */
export interface RecordedOrchestration {
  group_id: string;
  /** The group's repo path, or null when its `group.json` could not be read. */
  repo: string | null;
  /** The CLI the group's orchestrator block runs, or `""` when `group.json`
   *  could not be read — never a guessed default. */
  cli: string;
  /** The orchestrator's recorded session id, or null when none has been
   *  identified yet (a copilot/opencode watcher that has not bound one). */
  session_id: string | null;
  group_live: boolean;
  /** Whether `session_id` resolves in the store the resume path will look in.
   *  Always false when `session_id` is null. */
  resumable: boolean;
  /** Most recent `updated_ms` on any of the group's roster rows; 0 when the
   *  roster is empty or unreadable. */
  last_seen_ms: number;
}

/** Why a row does or does not offer Resume. One state per *answer*, not per
 *  boolean: `live` focuses a pane, `unidentified` waits, `lost` starts fresh,
 *  `damaged` repairs the group record. */
export type OrchRowState = "resumable" | "live" | "unidentified" | "lost" | "damaged";

/** One rendered row. `sessionId` is non-null exactly when `canResume` is true,
 *  so the caller cannot build a Resume button with nothing to resume. */
export interface OrchRow {
  groupId: string;
  /** The CLI's name for display, or "unknown CLI" when the record is damaged.
   *  DISPLAY ONLY — it can contain a space, so it must never be interpolated
   *  into a class name or any other token position (#1568 review N4). */
  cli: string;
  /** The raw `cli` off the wire, for keying a CSS class or any other token
   *  the renderer needs: a known CLI (`claude` | `copilot` | `opencode` | …)
   *  or `""` when `group.json` could not be read. Deliberately NOT the
   *  display label above — `cliLabel`'s "unknown CLI" would splice two junk
   *  classes onto the element, and a label is free to gain a space or a
   *  capital at any time without that being a wire change. */
  cliKey: string;
  /** Repo basename when known, else the group id — never a blank line. */
  title: string;
  sessionId: string | null;
  state: OrchRowState;
  /** The one-line explanation under the title. Always present: a row with no
   *  button must still say why. */
  detail: string;
  canResume: boolean;
  lastSeenMs: number;
}

/** Last path segment of a repo path (Windows or POSIX separators), for a
 *  compact title. Falls back to the whole string when it has no separator. */
function repoName(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** The CLI label. A damaged `group.json` leaves `cli` empty on the wire; say
 *  "unknown CLI" rather than printing nothing or inventing "claude". */
function cliLabel(cli: string): string {
  return cli.trim() || "unknown CLI";
}

/** The CLI as a class-name TOKEN, or `""` when it cannot be one. A token may
 *  not contain whitespace at all: the renderer interpolates this into a
 *  `class` attribute, where one space silently becomes two class names. */
function cliToken(cli: string): string {
  const t = cli.trim();
  return /\s/.test(t) ? "" : t;
}

/** Which of the five states a record is in. Order matters and is argued:
 *
 *  - `damaged` first, because with no readable `group.json` nothing else about
 *    the row is trustworthy — including which CLI a resume would even launch.
 *  - `live` next, because `resume_recorded_session` REFUSES a live group
 *    ("already has a live orchestrator — focus its pane instead"), so a live
 *    row must never offer the button whatever its `resumable` says.
 *  - then the two honest no-id / not-in-the-store cases, which are what the
 *    backend's `session_id`/`resumable` pair distinguishes. */
function stateOf(r: RecordedOrchestration): OrchRowState {
  if (!r.cli.trim()) return "damaged";
  if (r.group_live) return "live";
  if (!r.session_id) return "unidentified";
  return r.resumable ? "resumable" : "lost";
}

function detailFor(r: RecordedOrchestration, state: OrchRowState): string {
  const cli = cliLabel(r.cli);
  switch (state) {
    case "damaged":
      return "This group's record could not be read — orrerix can't tell which CLI ran it, so there is nothing safe to resume.";
    case "live":
      return "Running now — focus its orchestrator pane instead of resuming it.";
    case "unidentified":
      return `Session not yet identified — orrerix has not learned this ${cli} orchestrator's session id, so there is nothing to resume yet.`;
    case "lost":
      return `Recorded session is no longer in the ${cli} session store on this machine — start a fresh orchestrator on this group's board instead.`;
    case "resumable":
      return `Resumes the ${cli} orchestrator and reopens this group.`;
  }
}

/** Does this record match the sidebar's filter box? Matches on the fields a
 *  human can see or would type: group id, repo path, and CLI. */
function matches(r: RecordedOrchestration, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return (
    r.group_id.toLowerCase().includes(q) ||
    (r.repo ?? "").toLowerCase().includes(q) ||
    r.cli.toLowerCase().includes(q)
  );
}

/** The rows to render, filtered and ordered.
 *
 *  ORDER: live groups first (they are the ones the human is working in right
 *  now), then most recently active first. `group_id` breaks a tie so the list
 *  does not reshuffle between two refreshes that read the same timestamps —
 *  a moving list is a misclick.
 *
 *  Never mutates its input: `sessions.ts` holds the fetched array. */
export function orchRows(
  list: readonly RecordedOrchestration[],
  query = ""
): OrchRow[] {
  return list
    .filter((r) => matches(r, query))
    .slice()
    .sort(
      (a, b) =>
        Number(b.group_live) - Number(a.group_live) ||
        b.last_seen_ms - a.last_seen_ms ||
        a.group_id.localeCompare(b.group_id)
    )
    .map((r) => {
      const state = stateOf(r);
      return {
        groupId: r.group_id,
        cli: cliLabel(r.cli),
        // A token, not prose — so it is trimmed, and any value that still
        // carries whitespace yields NO key at all rather than a string the
        // renderer would splice into two classes. `trim()` alone closes only
        // the surrounding case; an INTERIOR space survives it, and
        // `agent_cli` comes from `group.json`, which is operator-authored
        // (#1568 review round 2). No key renders as a bare `session-badge`:
        // uncoloured, still labelled, never two junk classes.
        cliKey: cliToken(r.cli),
        title: r.repo ? repoName(r.repo) : r.group_id,
        sessionId: r.session_id,
        state,
        detail: detailFor(r, state),
        // The ONLY place a button is authorized, and it re-derives every
        // condition rather than trusting `resumable` alone: the backend
        // refuses a live group, and a null id has nothing to pass to
        // `resumeOrchSession`. Deriving it from `state` would be the same
        // rule written twice.
        canResume: state === "resumable" && r.session_id !== null,
        lastSeenMs: r.last_seen_ms,
      };
    });
}
