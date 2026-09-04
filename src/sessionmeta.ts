// Pure formatting for the session browser's #1 metadata (name/description/
// goal + repo/branch/PR) — kept DOM-free so the truncation and "what's
// missing stays hidden, never guessed" rules are unit-testable.
// sessions.ts renders these strings; nothing here touches the DOM.

import type { SessionRoleInfo } from "./orchestration";

const MAX_TASK_LEN = 140;

/** The task/goal line for a session item, or null when there's nothing
 *  recorded (a legacy session, or an orchestrator with no assigned task) —
 *  callers hide the line entirely rather than showing an empty one. */
export function taskSummary(role: SessionRoleInfo | undefined): string | null {
  const task = role?.task.trim();
  if (!task) return null;
  return task.length > MAX_TASK_LEN ? `${task.slice(0, MAX_TASK_LEN - 1)}…` : task;
}

/** Last path segment of a repo path, for a compact identity label. Falls
 *  back to the full path if it has no separator (already short). */
function shortRepoName(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** The repo/branch identity line, or null when neither is known. Shows
 *  whichever pieces are present — branch alone, repo alone, or "repo @
 *  branch" — never a placeholder for the missing half. */
export function repoBranchLine(role: SessionRoleInfo | undefined): string | null {
  const repo = role?.repo?.trim();
  const branch = role?.branch?.trim();
  if (repo && branch) return `${shortRepoName(repo)} @ ${branch}`;
  if (branch) return branch;
  if (repo) return shortRepoName(repo);
  return null;
}

/** How much of a session's title fits in a restored pane's name before it is
 *  cut. Unchanged from the inline value this replaced. */
const MAX_PANE_TITLE_LEN = 34;

/** The pane name a Sessions-tab restore opens with: the CLI that owns the
 *  session, then its title.
 *
 *  Derived from the row's own `source` rather than a per-CLI branch (#722).
 *  What this replaced was `s.source === "claude" ? "claude · " : "copilot · "`,
 *  which was correct only while there were exactly two sources — the moment
 *  the backend's scanner learned a third, that ternary labelled every opencode
 *  session "copilot · …": a pane naming the wrong CLI, with nothing to catch
 *  it, since `source` crosses IPC as a plain string. Reading the field means a
 *  fourth source is named correctly on arrival instead of silently joining
 *  whichever CLI sits in the else-branch. */
export function restoredPaneName(source: string, title: string): string {
  const short = title.length > MAX_PANE_TITLE_LEN ? `${title.slice(0, MAX_PANE_TITLE_LEN)}…` : title;
  return `${source} · ${short}`;
}

/** The CLI badge on a session row.
 *
 *  Read off the row, not branched on it (#722) — the same correction
 *  `restoredPaneName` above carries, and the same bug it had:
 *  `s.source === "claude" ? "CLAUDE" : "COPILOT"` labelled every session that
 *  was neither one **COPILOT**, so the sidebar would have asserted the wrong
 *  CLI about a row whose resume command names a different one. A source with
 *  no styling rule gets the base badge, which is a plain chip — legible, just
 *  uncoloured — never a wrong name. */
export function sessionBadgeLabel(source: string): string {
  return source.toUpperCase();
}

/** The recorded pane-name line for a session row (#2116), or `null` when there
 *  is nothing worth showing.
 *
 *  THE FALLBACK IS THE TITLE ITSELF, NEVER A PLACEHOLDER. The row already shows
 *  the session's transcript title; this line is an ADDITION for the case where
 *  the human called the pane something of their own. So it returns `null` — the
 *  caller renders no line at all — rather than an empty string or a dash.
 *
 *  Three things are "nothing worth showing", and the third is the one worth
 *  arguing:
 *
 *   1. no recorded name (a session predating `sessionlog.json`, or one nobody
 *      has opened a pane on since);
 *   2. a name equal to the title, which would print the same words twice;
 *   3. a name equal to the one a Sessions-tab restore MINTS
 *      (`restoredPaneName`). That is not a name the human chose — it is this
 *      module's own auto-name, `"<cli> · <title>"` — and every pane restored
 *      from this very list carries it. Without this clause the commonest row on
 *      the page grows a second line that restates its own title with a CLI
 *      prefix, which is noise on the majority of rows rather than the signal
 *      the line exists for.
 *
 *  Compared on the trimmed strings and case-sensitively: a human who renames a
 *  pane from `worker` to `Worker` has renamed it, and this is a report of what
 *  they wrote, not a guess at what they meant. */
export function paneNameLine(
  paneName: string | undefined,
  title: string,
  source: string
): string | null {
  const name = paneName?.trim();
  if (!name) return null;
  if (name === title.trim()) return null;
  if (name === restoredPaneName(source, title)) return null;
  return name;
}

/** What the notes chip on a session row says (#2116 slice E2).
 *
 *  `SessionLogStore.notesCount` returns **0 for a session with no notes and 0
 *  for a file nobody has read yet** — its own doc says so, and says a caller
 *  must not collapse the two. This is where that separation is made for the
 *  chip, because the two situations are the whole reason the chip needs a rule
 *  rather than a template string:
 *
 *   - **unread** (`loaded` false — the read has not landed, or it rejected):
 *     no number at all. A `0` here would be the chip asserting that a session
 *     with notes on disk has none, which is the same silent-loss shape the
 *     overlay's own "could not read the notes file" line exists to avoid;
 *   - **read, and none**: still no number — a `0` on every row is a mark that
 *     means nothing — but the tooltip says so plainly and names the action, so
 *     the chip is an affordance rather than a mystery glyph;
 *   - **read, and some**: the count, and a tooltip that agrees with it.
 *
 *  The chip is rendered in every one of those states. A row whose notes cannot
 *  be counted still has to be a way IN to that session's notes: the overlay
 *  reads the file itself and says what it found. */
export interface NotesChipLabel {
  /** The count, or `""` when there is no honest number to state. */
  text: string;
  /** Tooltip. Always names what the chip is for — never empty. */
  title: string;
  /** This session is KNOWN to carry notes. A styling hook, and false for an
   *  unread store, where "known" is precisely what it is not. */
  hasNotes: boolean;
}

export function notesChipLabel(count: number, loaded: boolean): NotesChipLabel {
  if (!loaded) {
    return {
      text: "",
      title: "Notes about this session — the notes file has not been read.",
      hasNotes: false,
    };
  }
  if (count <= 0) {
    return {
      text: "",
      title: "No notes about this session yet — click to write one.",
      hasNotes: false,
    };
  }
  return {
    text: String(count),
    title:
      count === 1
        ? "1 note about this session"
        : `${count} notes about this session`,
    hasNotes: true,
  };
}

/** The PR chip label, or null when no PR is known yet. A bare number (how
 *  the board stores most PR refs) renders as "#123"; anything already
 *  prefixed or otherwise shaped is shown verbatim. */
export function prLabel(role: SessionRoleInfo | undefined): string | null {
  const pr = role?.pr?.trim();
  if (!pr) return null;
  return /^\d+$/.test(pr) ? `#${pr}` : pr;
}
