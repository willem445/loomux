// Pure per-pane restore policy for session restore (#194). DOM-free so the
// hybrid decision (below) is unit-tested (test/panerestore.test.ts); the actual
// grid rebuild — feeding these actions to grid.openPane / resumeOrchSession —
// is main.ts wiring (Phase 4).
//
// THE ADOPTED HYBRID (issue #194, plan comment). Resuming a CLI session re-opens
// its context but costs NOTHING until a prompt is sent, so:
//
//   - Terminal  → re-spawn a fresh shell in the recorded cwd + shell kind. No
//                 session to resume; zero cost; layout/cwd back instantly.
//   - Agent     → AUTO-RESUME via the recorded session id (--resume into the idle
//                 TUI): loads context, spends no credits, delivers "near-exact
//                 state". NEVER replays a queued prompt. With no resumable id
//                 (best-effort CLIs) it falls back to a DORMANT pane with a Start
//                 button in the same cwd.
//   - Orch      → NEVER auto-resumed. An orchestration pane (orchestrator /
//                 worker / reviewer) restores DORMANT; the human resumes the
//                 whole group via the existing resumeOrchSession path. This is
//                 the ONE place a resume can actually burn credits — a resumed
//                 autonomous orchestrator (#83) may idle-tick and spawn a worker
//                 storm (#78) — so the credit-safety stance stays exactly here.
//   - SSH       → NEVER auto-reconnected (#887 S4). An ssh pane restores DORMANT
//                 with a Reconnect button, for two reasons that do not overlap:
//                 the far end is an agent CLI on someone else's machine, so an
//                 automatic reconnect spends REMOTE credits with no human present
//                 (the orch-pane argument, one host removed); and a host that is
//                 down, asleep, or behind a VPN that isn't up yet turns boot into
//                 a wait on a TCP connect that may not fail for a minute. Neither
//                 applies to a local shell, which is why "terminal" still comes
//                 straight back. Reconnect rebuilds the command from the SAVED
//                 PROFILE (not a captured argv — see PersistedPane.sshProfileId),
//                 resuming the remote session when one was recorded.
//   - Content   → re-open the pane at its recorded root: the file MANAGER (#214),
//                 the file EDITOR, the GIT view (#217), or the WORKFLOW pane (#222).
//                 No process, no session,
//                 nothing to resume — they're pure content, so they come straight
//                 back. Whether the root still exists (and, for git, is still a
//                 repo) is I/O, which this pure module can't do: each action carries
//                 the recorded root (possibly null) and the caller fails soft to the
//                 welcome form in that slot when the probe says no.
//
// Flip AUTO_RESUME_AGENTS to false to make EVERY agent restore dormant instead —
// the plan's promised one-line switch, kept literally one line here.

import type { PersistedPane, PersistedLayoutNode, PersistedEmbed } from "./tabstore";
// The CLIs loomux can scan sessions for (`SessionInfo["source"]`), imported
// rather than re-spelled — see `sessionCliFromCommand` (#722).
import type { Cli } from "./sessionreconcile";

/** The adopted default (#194): auto-resume agent panes into their prior session.
 *  Set to false for the conservative all-dormant behavior (every agent gets a
 *  Start button; groups are dormant regardless). */
export const AUTO_RESUME_AGENTS = true;

/** What to do with one persisted pane on restore. `relaunch` carries the fields
 *  main.ts needs to open (or leave dormant) the pane; none of these actions ever
 *  replays a prompt or auto-resumes a group. */
export type RestoreAction =
  | { type: "spawn-terminal"; name: string; cwd: string | null; shellKind: PersistedPane["shellKind"] }
  | {
      type: "resume-agent";
      name: string;
      cwd: string | null;
      command: string | null;
      argv: string[] | null;
      /** The recorded session id to --resume into (guaranteed present here). */
      sessionId: string;
    }
  | {
      // An agent whose recorded session id has NO resumable conversation on disk
      // (never prompted → no transcript, or the transcript was deleted). Resuming
      // it would exit 1 ("No conversation found …") and strand a dead pane, so we
      // start a FRESH session in place instead — SAME cwd / CLI / name — reusing the
      // recorded id so the fresh session is itself resumable next boot. (#194 BUG-1)
      type: "fresh-agent";
      name: string;
      cwd: string | null;
      command: string | null;
      argv: string[] | null;
      sessionId: string;
    }
  | {
      type: "dormant-agent";
      name: string;
      cwd: string | null;
      command: string | null;
      argv: string[] | null;
    }
  | {
      // The orchestration pane's whole group stays dormant; the human resumes it
      // via resumeOrchSession. main.ts does NOT spawn a pane for this action. The
      // recorded session id + role ride along so the dormant placeholder carries
      // the CAPTURED group member — the set a whole-group resume restores (#194.5).
      type: "dormant-group";
      name: string;
      sessionId: string | null;
      role: string | null;
      /** The group this placeholder's own record names (#485) — null for a
       *  pre-#485 snapshot. The Resume click resumes THIS group, and the
       *  member set it plans is the tab's placeholders carrying the same
       *  value, so a second group sharing the tab is neither swept into the
       *  plan nor silently dropped from it. */
      groupId: string | null;
      /** Every orchestration-family view docked to this pane (#361) — up to
       *  three, one per edge — carried the same way role and sessionId are:
       *  main.ts's resumeDormantGroup matches this back to the member it
       *  belongs to (by sessionId) and re-applies it to the freshly resumed
       *  pane via Pane.restoreEmbeds — the ONE place a captured UI
       *  preference is threaded through a whole-group resume today. */
      embeds: PersistedEmbed[];
    }
  | {
      // An SSH pane (#887 S4), back as a dormant Reconnect card. Nothing spawns
      // until the human clicks it (see the policy note at the top of this file).
      //
      // Carries the CONNECTION and the recorded session — never a command line.
      // The caller resolves `profileId` against the live profile store and runs
      // it back through the same builders a fresh launch uses, so a profile the
      // human edited between boots reconnects with the edit, and a profile they
      // DELETED reconnects with nothing: `profileId` may name a connection that
      // no longer exists, and a null one (a pre-S4 or hand-mangled record) names
      // none at all. Both land on the card's error state rather than on a guess.
      //
      // Deliberately NO orchestration fields — not `role`, not `groupId`, not an
      // agent id — even though the persisted leaf they come from has room for
      // all three. An SSH pane can never be an orchestration group member (the
      // #887/#888 boundary, refused at the pane in `sshOrchestrationRefusal`),
      // and restore is the one path that could smuggle one in, since it builds
      // a spawn out of a file on disk that a human can hand-edit. The boundary
      // holds here by construction: there is no field to carry it through.
      type: "dormant-ssh";
      name: string;
      profileId: string | null;
      /** The remote session id recorded at launch (claude only — see
       *  `sshMintsSessionId`), so Reconnect can resume it instead of starting a
       *  second conversation on the far host. Null for every other remote CLI
       *  and for a plain login shell. */
      sessionId: string | null;
    }
  | {
      // A file-explorer pane (#214), back at its recorded root. Nothing to spawn
      // or resume — but `root` may be null (a record written without one) or name
      // a folder that has since been deleted/renamed/unmounted. The caller probes
      // it and, when it isn't a readable directory, opens the WELCOME form in that
      // slot with a message instead — a broken listing pane would be worse than a
      // legible "pick a folder".
      type: "open-files";
      name: string;
      root: string | null;
    }
  | {
      // A file-EDITOR pane (#217), back at its recorded root, with the file it had
      // open re-opened from disk. Same contract as open-files, same probe (is this
      // still a readable directory?), same fail-soft.
      //
      // `file` is a PATH, not a buffer. Unsaved edits are NOT persisted: the layout
      // records where the pane was rooted and what it was showing, never what was
      // typed into it — a snapshot that quietly kept unsaved text would be a second
      // copy of the user's file, and it would undercut the close guard
      // (Pane.confirmClose), whose whole point is that they were ASKED before it could
      // be lost. A file that has since been deleted/renamed just fails to open (a
      // toast); the pane still comes back, rooted.
      type: "open-editor";
      name: string;
      root: string | null;
      file: string | null;
    }
  | {
      // A GIT pane (#217), back over its recorded repo. The probe here is stricter
      // than a directory check — the folder can still exist and no longer be a git
      // work tree (deleted .git, a worktree pruned since) — so the caller resolves
      // it with `gitRepoRoot`. A definitive "not a repo" fails soft to the welcome
      // form; a git that could not be RUN at all does not (see main.ts) — that is a
      // tooling failure, not a fact about the repo, and pruning the pane over it
      // would lose the recorded path for good.
      type: "open-git";
      name: string;
      root: string | null;
    }
  | {
      // A WORKFLOW pane (#222), back over its recorded repo, showing the workflow file it
      // was showing. Same contract as open-editor — a root plus a path, never a buffer —
      // and the same probe (is the root still a readable directory?), because the workflow
      // file itself NOT existing is not a failure: the pane opens on its empty state and
      // offers to create one. A repo that has been deleted or unmounted is a different
      // matter, and fails soft to the welcome form in that slot.
      type: "open-workflow";
      name: string;
      root: string | null;
      file: string | null;
    };

/** True when a recorded agent session id still has a resumable conversation on
 *  disk. The wiring builds this from `listSessions()` (which lists exactly the
 *  sessions that HAVE a transcript) and passes it in, keeping this module pure —
 *  the alternative would be a Tauri call from here (#194 BUG-1). */
export type SessionResumable = (sessionId: string) => boolean;

/** Map ONE persisted pane to its restore action, per the adopted hybrid.
 *
 *  @param resumable optional predicate: does this session id still have a
 *  resumable conversation? When omitted, an agent with an id is assumed
 *  resumable (the original behavior). When provided and it returns false, the
 *  agent restores FRESH (same identity) instead of a doomed `--resume`. */
export function planPaneRestore(pane: PersistedPane, resumable?: SessionResumable): RestoreAction {
  switch (pane.paneKind) {
    case "terminal":
      return { type: "spawn-terminal", name: pane.name, cwd: pane.cwd, shellKind: pane.shellKind };
    case "orch":
      // Never auto-resume a group — dormant, human-triggered Resume only. Carry
      // the captured session id + role so the placeholder knows which group member
      // it is (a whole-group resume reads these off the tab's placeholders), and
      // the embed preference (#361) for the same reason.
      return {
        type: "dormant-group",
        name: pane.name,
        sessionId: pane.sessionId,
        role: pane.role,
        groupId: pane.groupId,
        embeds: pane.embeds,
      };
    case "ssh":
      // Dormant, human-triggered Reconnect only (#887 S4) — the remote-credit and
      // dead-host arguments are at the top of this file. Note what is NOT read
      // off the record here: `role`, `groupId` and `command`/`argv` are all
      // dropped on the floor. The first two are the #887/#888 boundary (a
      // hand-edited tabs.json naming an ssh leaf with `role: "worker"` restores
      // as an ordinary dormant SSH card, never as a group member); the last two
      // are the profile-not-argv contract on `PersistedPane.sshProfileId`.
      return {
        type: "dormant-ssh",
        name: pane.name,
        profileId: pane.sshProfileId,
        sessionId: pane.sessionId,
      };
    case "files":
      // Pure content: no process, no credits, no session — it just comes back at
      // the root it was captured with (which lives in `cwd`).
      return { type: "open-files", name: pane.name, root: pane.cwd };
    case "editor":
      // Same deal (#217): the pane comes back rooted where it was, showing the file it
      // was showing — re-read from disk. What was typed and not saved is deliberately
      // not persisted (see the action's comment above).
      return { type: "open-editor", name: pane.name, root: pane.cwd, file: pane.file };
    case "git":
      // Same deal (#217), over a repo instead of a folder. The worktree SELECTION and
      // the read-only unlock (#208) are view state, not layout: a restored git pane
      // opens on the primary worktree, locked, exactly like a freshly opened one — an
      // unlock that survived a restart would be the one piece of this pane's state
      // that could quietly cost you something.
      return { type: "open-git", name: pane.name, root: pane.cwd };
    case "workflow":
      // Same deal (#222): the pane comes back over the repo it was pointed at, on the
      // workflow file it was editing — re-read from disk. The block SELECTION and the
      // open tab are view state, not layout: a restored workflow pane opens on its roster
      // exactly like a freshly opened one.
      return { type: "open-workflow", name: pane.name, root: pane.cwd, file: pane.file };
    case "agent":
      // Auto-resume when we have a session id AND the hybrid is enabled; else a
      // dormant Start placeholder (no id to resume into, or the flip is off).
      if (AUTO_RESUME_AGENTS && pane.sessionId) {
        // If we can tell the session has no resumable conversation, start fresh in
        // place rather than crash on `--resume` (BUG-1). Unknown (no predicate) →
        // attempt the resume; the runtime backstop (shouldRespawnFresh) catches a
        // resume that fails anyway (deleted transcript, CLI error).
        if (resumable && !resumable(pane.sessionId)) {
          return {
            type: "fresh-agent",
            name: pane.name,
            cwd: pane.cwd,
            command: pane.command,
            argv: pane.argv,
            sessionId: pane.sessionId,
          };
        }
        return {
          type: "resume-agent",
          name: pane.name,
          cwd: pane.cwd,
          command: pane.command,
          argv: pane.argv,
          sessionId: pane.sessionId,
        };
      }
      return {
        type: "dormant-agent",
        name: pane.name,
        cwd: pane.cwd,
        command: pane.command,
        argv: pane.argv,
      };
  }
}

/** The value/token grammar shared by every quote-aware scan in this module: a
 *  whole double-quoted run kept as ONE unit (quotes included, so a `.slice`
 *  excise never leaves a stray `"` behind), or a bare whitespace-free run —
 *  the one shape every flag value AND every command token recorded here
 *  uses (a session id, a solo MCP config path, an arbitrary CLI argument, …).
 *  THREE call sites are built from this exact constant via `new RegExp`, not
 *  three independently hand-typed copies of the pattern text:
 *  `CLAUDE_SOLO_MCP_RE`/`COPILOT_SOLO_MCP_RE` further below (#439, one value
 *  each inside a fixed flag group) and `tokenizeWithPositions` just below
 *  (#449, the general per-token scan `stripSessionFlagsFromCommand` walks). A real
 *  binding, checked by construction: change this constant and every call
 *  site moves with it — the #452 worry (two parsers that quietly disagree)
 *  this exists to actually close, not merely gesture at. */
const QUOTED_OR_BARE_VALUE = `"[^"]*"|\\S+`;

/** The two flag names that name a Claude session on a recorded launch line —
 *  the "which flag NAMES this module treats as session identity" answered in
 *  exactly one place, not two independently-typed literals that could drift
 *  (#471 review round 2: "the shared grammar" means the flag-name set too,
 *  not just the value shape). `#458`/#473's `scan_copilot()` is about to
 *  start recording `--resume=<id>` (the `=` form) for copilot panes too —
 *  this set, and every helper below built on it, does not care which CLI
 *  recorded the command, only the flag's literal text. */
const SESSION_FLAG_NAMES = ["--session-id", "--resume"];

/** The same set plus opencode's own, for a line this module has ALREADY
 *  identified as opencode's (#722). opencode names a session with
 *  `--session <id>` and has no `--session-id` at all — nothing pre-assigns an
 *  id there — so `--session` is a third session-identity flag its excision has
 *  to recognize, or a pane re-resumed twice ends up carrying
 *  `opencode --session A --session A`.
 *
 *  Deliberately NOT merged into `SESSION_FLAG_NAMES` above, whose doc is
 *  explicit that it is applied to every recorded line whatever CLI wrote it:
 *  whether `--session` means something else to claude or copilot is an
 *  unverified vendor fact, and the rule for those is to check the official
 *  reference, not to assume. Adding it globally would risk excising a flag
 *  this project never established the meaning of; adding it per-CLI risks
 *  nothing, because the program is already known at both call sites.
 *
 *  Exact-match, so the `--session-id` prefix relationship is a non-issue: a
 *  bare token equals one name or the other, and the `=` form is matched as
 *  `--session=` / `--session-id=`, which cannot alias either. */
const OPENCODE_SESSION_FLAG_NAMES = [...SESSION_FLAG_NAMES, "--session"];

/** True when `raw` — the RAW slice of ONE token, quotes included if it was a
 *  quoted token — is a session flag written bare (no value attached: not
 *  even a trailing `=`). A quoted token's raw slice always starts with `"`,
 *  so a flag-looking WORD sitting inside a quoted argument (a system prompt
 *  that happens to discuss `--resume`) can never equal a bare flag name —
 *  this is what keeps `stripSessionFlagsFrom*` from mistaking quoted content
 *  for a flag (#471 review round 2, case 5). */
function isBareSessionFlag(raw: string, names: readonly string[]): boolean {
  return names.includes(raw);
}

/** True when `raw` is a session flag's SELF-CONTAINED `=value` token —
 *  including an EMPTY value (`--resume=`) — so it needs no following value
 *  token consumed. Same quote-immunity as `isBareSessionFlag` above, for the
 *  same reason. */
function isEqSessionFlag(raw: string, names: readonly string[]): boolean {
  return names.some((name) => raw.startsWith(`${name}=`));
}

/** Quote-aware tokenizer WITH POSITIONS into the ORIGINAL string: a fully
 *  double-quoted run (`"..."`) is ONE token whose range includes the quotes
 *  (so its raw slice can never equality-match a bare flag name — see
 *  `isBareSessionFlag`), everything else is a whitespace-delimited token.
 *  Gaps BETWEEN tokens (whitespace, including multi-space runs) are not
 *  tokens at all — a caller reconstructs the string by slicing the ORIGINAL
 *  around whichever token ranges it drops, so every gap it doesn't touch
 *  survives untouched by construction (never a `.join(" ")` reflow — the
 *  #449 bug this whole approach exists to close).
 *
 *  Built from `QUOTED_OR_BARE_VALUE` — the SAME grammar constant
 *  `CLAUDE_SOLO_MCP_RE`/`COPILOT_SOLO_MCP_RE` below are built from, via
 *  `new RegExp`, not a second hand-typed copy of the pattern text. A
 *  genuinely converged binding, not just a co-located one: change the
 *  constant and both call sites move together (the #452 drift concern this
 *  is written to actually satisfy, not merely gesture at). */
function tokenizeWithPositions(command: string): Array<{ start: number; end: number }> {
  const tokens: Array<{ start: number; end: number }> = [];
  const re = new RegExp(QUOTED_OR_BARE_VALUE, "g");
  let m: RegExpExecArray | null;
  while ((m = re.exec(command)) !== null) {
    tokens.push({ start: m.index, end: m.index + m[0].length });
  }
  return tokens;
}

/** Excise every session-identity flag occurrence from the ORIGINAL command
 *  STRING — never a tokenize/`.join(" ")` round trip. The property this owes
 *  (#449, hardened across two #471 review rounds): NO session-identity flag
 *  or its value survives into the rebuilt command, IN ANY FORM, for EITHER
 *  flag name — and nothing else is ever touched, whitespace runs inside
 *  unrelated quoted args (a system prompt, a spaced Windows path) included.
 *  Concretely, every form below is covered and fixture-pinned:
 *   - space form (`--resume abc`) — flag + its value token, both dropped;
 *   - `=` form (`--resume=abc`, or `--resume=` with an EMPTY value) — one
 *     self-contained token, dropped whole (round 2: the `#473`/#458 shape);
 *   - bare/valueless (`--resume` with nothing after it, or immediately
 *     followed by ANOTHER flag rather than a value) — the flag token alone
 *     is dropped; a following token that itself starts with `--` is NEVER
 *     swallowed as if it were this flag's value (round 1's blocker: the
 *     prior split/rejoin dropped a bare flag's "value slot" unconditionally,
 *     which silently doubled the flag on the NEXT restore — a compounding,
 *     credit-spending corruption once the doubled id lands as a bare
 *     positional argument, i.e. a prompt);
 *   - repeated occurrences of either flag — each found and dropped
 *     independently, in one pass, regardless of order;
 *   - a flag-looking WORD sitting inside a quoted argument — never mistaken
 *     for the flag (round 2, case 5): see `tokenizeWithPositions`.
 *  Each drop absorbs exactly ONE leading whitespace character (mirroring
 *  what a single flag[+value] run displaced), so untouched whitespace
 *  elsewhere is never reflowed. */
function stripSessionFlagsFromCommand(
  command: string,
  names: readonly string[] = SESSION_FLAG_NAMES
): string {
  const tokens = tokenizeWithPositions(command);
  const dropRanges: Array<[number, number]> = [];
  for (let i = 0; i < tokens.length; i++) {
    const raw = command.slice(tokens[i].start, tokens[i].end);
    const bare = isBareSessionFlag(raw, names);
    if (!bare && !isEqSessionFlag(raw, names)) continue;
    let dropStart = tokens[i].start;
    let dropEnd = tokens[i].end;
    if (bare && i + 1 < tokens.length) {
      const nextRaw = command.slice(tokens[i + 1].start, tokens[i + 1].end);
      if (!nextRaw.startsWith("--")) {
        dropEnd = tokens[i + 1].end; // consume the value token too
        i++; // skip it — already accounted for
      }
    }
    if (dropStart > 0 && /\s/.test(command[dropStart - 1])) dropStart -= 1;
    dropRanges.push([dropStart, dropEnd]);
  }
  if (!dropRanges.length) return command;
  let result = "";
  let cursor = 0;
  for (const [s, e] of dropRanges) {
    result += command.slice(cursor, s);
    cursor = e;
  }
  return result + command.slice(cursor);
}

/** Same excision, same property, for the already-discrete `argv` array — no
 *  whitespace-collapse or quoted-substring risk there (each element is one
 *  token already, and there's no quoting concept in an array), but the SAME
 *  flag-name/value-form coverage: bare (never swallowing a following
 *  element that is itself another flag), `=` form (incl. empty), and
 *  repeated occurrences. */
function stripSessionFlagsFromArgv(
  tokens: string[],
  names: readonly string[] = SESSION_FLAG_NAMES
): string[] {
  const out: string[] = [];
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (isBareSessionFlag(t, names)) {
      const next = tokens[i + 1];
      if (next !== undefined && !next.startsWith("--")) i++; // consume the value, when there is one
      continue;
    }
    if (isEqSessionFlag(t, names)) continue;
    out.push(t);
  }
  return out;
}

/** Turn a resumed agent's recorded launch line into the command that re-opens
 *  its prior session — the "resume/reattach command from a recorded sessionId"
 *  the plan calls for. Pure so it's unit-tested; main.ts feeds the result to
 *  grid.openPane.
 *
 *  This function rewrites a `claude …` OR a `copilot …` line: drop any
 *  recorded `--session-id`/`--resume` (every form `stripSessionFlagsFrom*`
 *  covers) so we never carry a stale id or double the flag, KEEP every other
 *  flag (model, the autopilot permission flag) so the resumed pane matches
 *  how it was launched, then append a fresh `--resume`. Resuming into the
 *  idle TUI costs nothing until a prompt is sent — and we never append one
 *  (the no-replay rule).
 *
 *  Four properties this module holds at once (#449; the last two added on
 *  #471 review round 3, per rev-11's trace through #473/#458's copilot
 *  emission fix — stated explicitly here per that review, not left implicit):
 *   1. Excision (`stripSessionFlagsFrom*`) strips a recorded session flag in
 *      ANY form, for EITHER flag name — see those functions' own docs.
 *   2. **copilot's `--resume` is emitted in the `=` form** (`--resume=<id>`):
 *      per the copilot CLI reference, the space form does not reliably bind.
 *      #473/#458 changed `scan_copilot()` to RECORD `--resume=<id>` for
 *      exactly this reason; without this, restoring that pane through THIS
 *      function would silently rewrite it back to the space form on every
 *      restore — the two PRs' fixes undoing each other, oscillating forever
 *      instead of converging.
 *   3. **copilot's `--session-id` stays SPACE form** — `agentFreshCommand`
 *      below, untouched. Deliberately asymmetric: the copilot CLI reference
 *      documents `--session-id ID` in the space form specifically, so
 *      "making the two flags consistent" would be a plausible-looking
 *      cleanup that quietly breaks a function that is currently correct.
 *      Pinned in `test/panerestore.test.ts` precisely so the next person who
 *      notices the asymmetry finds a test explaining it, not a TODO.
 *   4. A command needing no change comes back byte-identical apart from the
 *      appended flag (#442's guarantee, general to this whole module).
 *   5. **opencode resumes with `--session <id>`, not `--resume`** (#722): it
 *      has no `--resume` flag at all, so the default arm would hand its TUI a
 *      flag it does not know — and a pane opened from the Sessions tab is
 *      recorded with `opencode --session <id>` already, so without this the
 *      very next tab restore would rewrite it to
 *      `opencode --session A --resume A`. Space form, matching the resume arm
 *      of the backend's own `build_agent_command`; its excision uses
 *      `OPENCODE_SESSION_FLAG_NAMES` so the recorded `--session` is dropped
 *      rather than doubled.
 *
 *  Prefers the string `command`; falls back to structured `argv`, then to a
 *  bare `claude --resume` (the historical default — a bare fallback with no
 *  recorded command/argv at all has no CLI to detect, and every session this
 *  project mints an id for today is claude's). */
export function agentResumeCommand(
  command: string | null,
  argv: string[] | null,
  sessionId: string
): { command?: string; argv?: string[] } {
  // Three shapes, by detected CLI: copilot's `=` form (property 2), opencode's
  // different flag NAME (property 5), and — for claude or no CLI detected at
  // all — the space-form `--resume` this function has always emitted.
  const program = programFromRestore(command, argv);
  const isCopilot = program === "copilot";
  const isOpencode = program === "opencode";
  const names = isOpencode ? OPENCODE_SESSION_FLAG_NAMES : SESSION_FLAG_NAMES;
  if (command && command.trim()) {
    const resumeFlag = isCopilot
      ? `--resume=${sessionId}`
      : isOpencode
        ? `--session ${sessionId}`
        : `--resume ${sessionId}`;
    return { command: `${stripSessionFlagsFromCommand(command, names)} ${resumeFlag}` };
  }
  if (argv && argv.length) {
    const stripped = stripSessionFlagsFromArgv(argv, names);
    if (isCopilot) return { argv: [...stripped, `--resume=${sessionId}`] };
    return { argv: [...stripped, isOpencode ? "--session" : "--resume", sessionId] };
  }
  return { command: `claude --resume ${sessionId}` };
}

/** Build the command that starts a FRESH agent session in place, for the
 *  fallback when a recorded session has no resumable conversation (#194 BUG-1).
 *  Same shape as agentResumeCommand but pins the recorded id via `--session-id`
 *  (not `--resume`), so the fresh session is created with that id and becomes
 *  resumable itself once a prompt is sent — and, like resume, never carries a
 *  prompt. Drops any stale `--resume`/`--session-id` first so we don't double or
 *  attempt a resume.
 *
 *  Deliberately ALWAYS space form (`--session-id <id>`), for every CLI
 *  including copilot — property 3 on `agentResumeCommand`'s doc comment
 *  above. This is NOT an oversight to "make consistent" with that function's
 *  `=`-form copilot `--resume`: the copilot CLI reference documents
 *  `--session-id ID` specifically in the space form, so the two flags are
 *  correctly asymmetric. `test/panerestore.test.ts` pins this explicitly so
 *  a future cleanup pass finds the reason, not just the inconsistency.
 *
 *  **opencode keeps no identity across this fallback, because it cannot**
 *  (#722): no opencode flag pre-assigns a session id — `--session` continues
 *  an existing one, and there is no `--session-id` — so the recorded id is
 *  dropped along with any stale session flag and the pane starts a genuinely
 *  new conversation, whose id the reconciler learns afterwards exactly as it
 *  does for any other bare opencode pane. Appending `--session-id` to an
 *  opencode line instead (what the shared arm would do) hands its TUI an
 *  unknown flag; appending `--session <id>` would be worse than that — it is
 *  precisely the doomed resume this function exists to avoid, since this arm
 *  is only reached when the caller has established that the session is not
 *  resumable. */
export function agentFreshCommand(
  command: string | null,
  argv: string[] | null,
  sessionId: string
): { command?: string; argv?: string[] } {
  const isOpencode = programFromRestore(command, argv) === "opencode";
  const names = isOpencode ? OPENCODE_SESSION_FLAG_NAMES : SESSION_FLAG_NAMES;
  if (command && command.trim()) {
    const stripped = stripSessionFlagsFromCommand(command, names);
    return { command: isOpencode ? stripped : `${stripped} --session-id ${sessionId}` };
  }
  if (argv && argv.length) {
    const stripped = stripSessionFlagsFromArgv(argv, names);
    return { argv: isOpencode ? stripped : [...stripped, "--session-id", sessionId] };
  }
  return { command: `claude --session-id ${sessionId}` };
}

/** The two CLIs `solo_prepare` (mod.rs:10441) mints a channel identity for —
 *  the only ones whose recorded command can carry the flags `stripSoloMcpFlags`
 *  below recognizes.
 *
 *  Bound to `CliCaps::mcp_argv_seam`, NOT to the set of CLIs whose sessions
 *  loomux can list: opencode joined `SessionInfo["source"]` in #722 and stays
 *  out of here, because its MCP/containment seam is an env-delivered config
 *  document rather than flags on a command line (`solo_prepare`'s own
 *  `unreachable!` arm covers every seamless CLI). A solo opencode pane is
 *  delivery-only from birth and its recorded command carries no identity flags
 *  at all, so there is nothing for this type to name and nothing for
 *  `stripSoloMcpFlags` to excise — it comes back from that function untouched,
 *  as `cli: null`, which is correct. This widens when opencode gets an argv
 *  seam, not when the Sessions tab learns to list it. */
export type SoloCli = "claude" | "copilot";

// The minted config path is always a real Windows profile path
// (`C:\Users\<username>\AppData\Roaming\loomux\orchestration\__solo__\configs\
// solo-N.json`, mod.rs:10451) and the backend quotes it (mod.rs:10454, 10458)
// precisely because a username CAN contain a space ("Will H", "John Smith") —
// this is not a hypothetical, it's the shape of real field data (CLAUDE.md
// constraint 8: no this-machine assumptions). These match the quoted-path
// group as ONE unit (`QUOTED_OR_BARE_VALUE`, the same value grammar
// `stripSessionFlagsFromCommand` above uses) rather than naively splitting on
// whitespace, and excise the match
// FROM THE ORIGINAL STRING — never a tokenize/rejoin round trip — so nothing
// outside the matched region is touched: a `cli: null` command (no solo
// flags at all) comes back byte-identical, and the surviving remainder of a
// matched command keeps whatever whitespace it already had (#439 review B1 +
// N1).
/** Every MCP identity a RECORDED launch command can carry (#1153 phase 3).
 *  The backend mints exactly one -- today's -- but this module reads command
 *  lines saved by a PAST session, so a tab recorded before the flag day still
 *  names the old server. Failing to recognise it would leave the dead
 *  `--mcp-config` path in the replayed command: the pane then boots against a
 *  file agent exit already deleted, which is exactly what this excision
 *  exists to prevent. Two spellings, listed once and read by BOTH forms --
 *  the string regexes below alternate over these arrays and the argv scan
 *  membership-tests the same ones -- so a third spelling cannot reach one
 *  form and miss the other. */
const MCP_TOOL_PREFIXES = ["mcp__orrerix", "mcp__loomux"] as const;
const MCP_SERVERS = ["orrerix", "loomux"] as const;

const CLAUDE_SOLO_MCP_RE = new RegExp(
  `(^|\\s)--mcp-config\\s+(${QUOTED_OR_BARE_VALUE})\\s+--strict-mcp-config\\s+--allowedTools\\s+(?:${MCP_TOOL_PREFIXES.join("|")})(?=\\s|$)`
);
const COPILOT_SOLO_MCP_RE = new RegExp(
  `(^|\\s)--additional-mcp-config\\s+(${QUOTED_OR_BARE_VALUE})\\s+--allow-tool\\s+(?:${MCP_SERVERS.join("|")})(?=\\s|$)`
);

/** Remove a recorded agent command's solo channel-identity MCP flags (#439):
 *  `--mcp-config <path> --strict-mcp-config --allowedTools mcp__<server>`
 *  (claude) or `--additional-mcp-config @<path> --allow-tool <server>`
 *  (copilot) — the exact, contiguous flag group `launcher.ts:1343` appends via
 *  `soloPrepare`'s `mcp_args`. That path is guaranteed gone by the time ANY
 *  restore replays it: agent exit deletes the config file and clears the
 *  token (mod.rs:18236), so replaying it hard-errors claude (missing file)
 *  and authenticates nothing on copilot. This never inspects the path's
 *  CONTENT — only the fixed flag tokens either CLI's mint always emits — so a
 *  config file that happens to live under a "solo"-named folder can't be
 *  mistaken for one; it only has to tolerate the path containing whitespace,
 *  which the quoted-path regex above does.
 *
 *  Returns which CLI's flags were found (null when the command carried none
 *  — nothing to re-mint) alongside the command/argv with that group excised.
 *  On `cli: null` the command/argv are returned EXACTLY as given — no
 *  reflow, no whitespace normalization, no trim — because most restores never
 *  had a solo identity at all, and this function must never be the thing that
 *  mutates their command line. The caller (main.ts) re-mints a FRESH identity
 *  via `soloPrepare` for the returned `cli` and appends its `mcp_args` with
 *  `appendSoloMcpArgs`; on a failed re-mint it uses this function's output
 *  as-is, so the pane still boots — delivery-only, never replaying the dead
 *  path (the same best-effort contract `launcher.ts:1337` already states for
 *  a fresh launch). */
export function stripSoloMcpFlags(
  command: string | null | undefined,
  argv: string[] | null | undefined
): { cli: SoloCli | null; command?: string; argv?: string[] } {
  if (command) {
    const claude = CLAUDE_SOLO_MCP_RE.exec(command);
    if (claude) {
      return {
        cli: "claude",
        command: command.slice(0, claude.index) + command.slice(claude.index + claude[0].length),
      };
    }
    const copilot = COPILOT_SOLO_MCP_RE.exec(command);
    if (copilot) {
      return {
        cli: "copilot",
        command: command.slice(0, copilot.index) + command.slice(copilot.index + copilot[0].length),
      };
    }
    return { cli: null, command }; // untouched — see doc comment above
  }
  if (argv && argv.length) {
    // The argv form never needs quote-awareness: each element is already
    // discrete (a spaced path is still exactly one array element), so the
    // fixed-offset scan below can't be fooled by whitespace the way naively
    // re-splitting a STRING can.
    for (let i = 0; i < argv.length; i++) {
      if (
        argv[i] === "--mcp-config" &&
        argv[i + 2] === "--strict-mcp-config" &&
        argv[i + 3] === "--allowedTools" &&
        (MCP_TOOL_PREFIXES as readonly string[]).includes(argv[i + 4])
      ) {
        return { cli: "claude", argv: [...argv.slice(0, i), ...argv.slice(i + 5)] };
      }
      if (argv[i] === "--additional-mcp-config" && argv[i + 2] === "--allow-tool" && (MCP_SERVERS as readonly string[]).includes(argv[i + 3])) {
        return { cli: "copilot", argv: [...argv.slice(0, i), ...argv.slice(i + 4)] };
      }
    }
    return { cli: null, argv }; // untouched, same guarantee as the command branch
  }
  return { cli: null };
}

/** Quote-aware tokenizer for a flag STRING (`soloPrepare`'s `mcp_args`, always
 *  a string) destined for an argv ARRAY: a whole double-quoted run becomes
 *  ONE element with its quotes stripped — never a literal `"` surviving into
 *  argv — so a spaced path (the same real case as above) can't fracture
 *  across two elements the way a plain `.split(/\s+/)` would (#439 review N2). */
function splitQuotedTokens(s: string): string[] {
  const tokens: string[] = [];
  const re = /"([^"]*)"|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s)) !== null) tokens.push(m[1] !== undefined ? m[1] : m[2]);
  return tokens;
}

/** Append a freshly-minted solo identity's `mcp_args` (`soloPrepare`'s return
 *  value — always a plain flag string) to a command already cleaned by
 *  `stripSoloMcpFlags`. For the argv form, `mcpArgs` is tokenized
 *  quote-aware (`splitQuotedTokens`) rather than split on whitespace, so a
 *  freshly-minted path with a space in it lands as one argv element with no
 *  literal quotes — solo panes are never argv-spawned today (`launcher.ts`
 *  only ever builds a string command), so this path is latent, but it must
 *  still be CORRECT rather than merely unexercised (#439 review N2). Pure
 *  concatenation, kept here so main.ts's restore call site is just: strip →
 *  await soloPrepare → append → open pane. */
export function appendSoloMcpArgs(
  command: string | undefined,
  argv: string[] | undefined,
  mcpArgs: string
): { command?: string; argv?: string[] } {
  if (argv) return { argv: [...argv, ...splitQuotedTokens(mcpArgs)] };
  return { command: [command, mcpArgs].filter((s) => s && s.trim()).join(" ") };
}

/** Extract the session id a spawn command carries via `--session-id <id>` or
 *  `--resume <id>` — plus, on a line this module has identified as opencode's,
 *  `--session <id>` (#1563 A1; opencode has no other spelling). Both the space
 *  and `=` forms. Used to populate an
 *  orchestration pane's recorded session id from its backend-built command
 *  (which embeds the id rather than passing it as a field), so `capture()` can
 *  persist it for a whole-group resume (#194.5). Null when the command carries no
 *  session flag. Prefers the string command; falls back to structured argv. */
export function sessionIdFromCommand(command: string | null, argv: string[] | null): string | null {
  // WHICH flag names a session is a per-CLI question, answered exactly where
  // `stripSessionFlagsFromCommand` and `agentResumeCommand` already answer it:
  // opencode names one with `--session <id>` and has no `--session-id` at all,
  // while `--session` on a claude or copilot line is a flag this project has
  // never established the meaning of (see OPENCODE_SESSION_FLAG_NAMES). Before
  // #1563 this function did not ask at all, so a RESUMED opencode pane — whose
  // backend-built line is `opencode --session <id>` — captured no id, and its
  // group came back from the next restart with `sessionId: null` and a Resume
  // card that had nothing to resume.
  //
  // One program derivation covers BOTH representations: `programFromRestore`
  // already falls back to argv when there is no string command, which is the
  // same ground the scan below falls through to — the asymmetry `hasForkSession`
  // was fixed for (review NB1), avoided here by never deriving it twice.
  const names =
    programFromRestore(command, argv) === "opencode" ? OPENCODE_SESSION_FLAG_NAMES : SESSION_FLAG_NAMES;
  const scan = (tokens: string[]): string | null => {
    for (let i = 0; i < tokens.length; i++) {
      const t = tokens[i];
      // Exact match for the bare form, `<name>=` for the attached one — the two
      // shapes `isBareSessionFlag`/`isEqSessionFlag` test, and the reason the
      // `--session`/`--session-id` prefix relationship is a non-issue: no token
      // can satisfy both names in either shape.
      if (names.includes(t)) return tokens[i + 1] ?? null;
      for (const name of names) {
        if (t.startsWith(`${name}=`)) return t.slice(name.length + 1) || null;
      }
    }
    return null;
  };
  if (command && command.trim()) {
    const id = scan(command.trim().split(/\s+/));
    if (id) return id;
  }
  if (argv && argv.length) return scan(argv);
  return null;
}

/** True when a command/argv line carries `--fork-session` — checked across
 *  BOTH representations unconditionally, not just whichever one is non-empty
 *  (review NB1: a command-only check misses an argv-only fork flag, since
 *  `sessionIdFromCommand`'s own id-extraction already falls through to argv
 *  when the command yields nothing — the fork check must scan the same
 *  ground the id extraction does, or a line like `argv: ["claude",
 *  "--resume", "x", "--fork-session"]` with a non-empty but flag-free
 *  `command` string would slip the id through while missing the flag).
 *  Shared by `adoptableSessionId` below (refuses to name an id on a forking
 *  line) and by the #440 reconciler/D2 exclusion in main.ts (refuses to
 *  EVER attach a *learned* id to a forking line either — see the design
 *  note's fork-session section for why exclusion, not id-stripping, is the
 *  chosen fix). */
export function hasForkSession(command: string | null, argv: string[] | null): boolean {
  const has = (tokens: string[]): boolean => tokens.includes("--fork-session");
  if (command && command.trim() && has(command.trim().split(/\s+/))) return true;
  if (argv && argv.length && has(argv)) return true;
  return false;
}

/** Extract the session id a CUSTOM command line names, for ADOPTING it as the
 *  pane's recorded session id (#440 D1/D1c) — a human-typed `claude --resume
 *  <id>` or `claude --session-id <id>` line already names its own session, so
 *  loomux can learn it without minting anything or rewriting the line.
 *
 *  Unlike `sessionIdFromCommand` above (used UNGUARDED for orchestration
 *  capture, where the command line is backend-built and never carries
 *  `--fork-session`), this refuses to adopt when `--fork-session` is also
 *  present: per the CLI reference (`--fork-session` — "When resuming, create a
 *  new session ID instead of reusing the original"), the id named on the line
 *  is NOT the id the running process ends up with, so adopting it would record
 *  a wrong id — worse than recording none, since a wrong id silently resumes
 *  into the wrong (or someone else's) transcript next boot. */
export function adoptableSessionId(command: string | null, argv: string[] | null): string | null {
  if (hasForkSession(command, argv)) return null;
  return sessionIdFromCommand(command, argv);
}

/** Normalize a launch command's first token into the bare CLI program name
 *  (#457, closing #452's concrete named case): strips a directory prefix
 *  (`C:\tools\copilot.exe` → `copilot.exe`, `/usr/local/bin/claude` →
 *  `claude`) and a trailing executable extension (`.exe`/`.cmd`/`.bat`,
 *  case-insensitive — Windows PATH resolution accepts any of them), then
 *  lowercases. Without this, a path-qualified or `.exe`-suffixed command
 *  silently fails every `=== "claude"`/`=== "copilot"` check downstream —
 *  per-CLI restore behavior (the autopilot watcher, the D2 resume-candidate
 *  card, `Pane.agentCli`) just doesn't apply, with no error, for a pane that
 *  is unambiguously one of those two CLIs.
 *
 *  This is the ONE place that answers "what CLI program does this raw first
 *  token name" — `programFromRestore` below, `main.ts`'s D2 dormant-card
 *  sniff, and `Pane.agentCli` (`pane.ts`) all call it now rather than each
 *  re-deriving the same first-token-lowercased logic independently (#452:
 *  three duplicate, and until now identically-incomplete, derivations). Not
 *  a full #452 convergence — the quoting/tokenizing grammar
 *  (`QUOTED_OR_BARE_VALUE`) is a different axis, already converged per #471,
 *  and the D2 card's OWN `.command`-vs-`.argv` extraction and the launcher's
 *  `plan.command.split(/\s+/)[0]` probe step are untouched — this only
 *  converges what happens to an already-extracted raw token. */
export function normalizeAgentProgram(raw: string): string {
  const base = raw.split(/[\\/]/).pop() ?? raw;
  return base.replace(/\.(exe|cmd|bat)$/i, "").toLowerCase();
}

/** The CLI program a restored `command`/`argv` would invoke. Used (#456) to
 *  tell whether a `resume-agent`/`fresh-agent`/`dormant-agent` restore
 *  action is a copilot pane, so the autopilot-dialog watcher can be wired in
 *  for it the same way a fresh launch gets it. */
export function programFromRestore(command: string | null, argv: string[] | null): string | null {
  const first = command?.trim().split(/\s+/)[0] || argv?.[0];
  return first ? normalizeAgentProgram(first) : null;
}

/** Which session-STORE CLI a launch command names, or null when it names none
 *  — `Pane.agentCli`'s body, moved here (#722) so it is unit-testable and so
 *  this file's single first-token derivation is followed by a single
 *  membership test, rather than that set being spelled a second time in
 *  `pane.ts` (the #452 theme this module already carries for the derivation
 *  itself).
 *
 *  The set is exactly `Cli` — `SessionInfo["source"]`, the CLIs loomux can
 *  scan sessions for — and that identity is the point rather than a
 *  coincidence: this answer is matched against `listSessions()` rows, so a CLI
 *  the scanner lists but this returns `null` for is a pane that can never
 *  adopt the session sitting in the sidebar under its own cwd. Everything else
 *  — codex, gemini, a plain shell, a program loomux does not know — is null,
 *  because there is no store to match it against. */
export function sessionCliFromCommand(command: string | null | undefined): Cli | null {
  const first = command?.trim().split(/\s+/)[0];
  if (!first) return null;
  const program = normalizeAgentProgram(first);
  return program === "claude" || program === "copilot" || program === "opencode" ? program : null;
}

/** Whether a restored copilot pane's command/argv actually carries
 *  `--autopilot`, and so needs the fail-soft dialog watcher a fresh launch
 *  gets (#456 review NB1) — checked across BOTH representations
 *  unconditionally, the same asymmetry guard `hasForkSession` above uses:
 *  `programFromRestore` already falls back to argv when there's no string
 *  command, so the flag check has to scan the same ground or an argv-only
 *  copilot autopilot pane would silently skip the watcher (latent today —
 *  solo copilot panes are always command-string — but not by design).
 *  Single source for the three identical inline checks that used to live in
 *  `main.ts`'s `resume-agent`/`fresh-agent`/`dormant-agent` cases. */
export function shouldWatchCopilotOnRestore(command: string | null, argv: string[] | null): boolean {
  if (programFromRestore(command, argv) !== "copilot") return false;
  const hasAutopilot = (tokens: string[]): boolean => tokens.includes("--autopilot");
  if (command && command.trim() && hasAutopilot(command.trim().split(/\s+/))) return true;
  if (argv && argv.length && hasAutopilot(argv)) return true;
  return false;
}

/** The runtime backstop decision (#194 BUG-1): a resumed agent pane whose PTY
 *  just exited — should we respawn it FRESH in place instead of stranding a dead
 *  pane? Yes for any UNEXPECTED non-zero exit — a `--resume` against a missing/
 *  deleted transcript exits non-zero ("No conversation found …"), and any other
 *  resume-time CLI failure is handled the same honest way. A loomux-initiated kill
 *  (`expected`) or a clean exit (0, the human quit the resumed session) is left
 *  alone. Pure so the caller can unit-test it; the caller makes it one-shot so a
 *  fresh respawn (which is not a resume) never loops. */
export function shouldRespawnFresh(exit: { exit_code: number | null; expected: boolean }): boolean {
  return !exit.expected && exit.exit_code !== null && exit.exit_code !== 0;
}

/** One `grid.openPane` call in a layout rebuild — enough to reconstruct ANY
 *  nested split tree, including telling a 2×2 grid apart from four stacked panes
 *  (which a flat leaf list cannot).
 *
 *  - `relativeTo` — the index (into the returned array) of an EARLIER step whose
 *    pane is the anchor this one splits from; null for the first pane, which
 *    fills the empty grid (`dir`/`relativeTo` are then ignored). This anchor is
 *    what a flat `{dir, weight}[]` dropped, making nested layouts unreconstructible.
 *  - `dir` — the split direction to open in. Only the SECOND+ child of a split
 *    carries its split's direction; the split's first child is an anchor reused
 *    from an earlier step, never re-opened.
 *  - `weights` — the flex-grow chain from the inserted subtree's OUTERMOST slot
 *    down its left spine to this entry leaf (length 1 for a plain leaf child).
 *    `grid.openPane` resets flex to equal shares as it splits, so restore applies
 *    these afterward: the outermost entry is the weight of the (possibly new)
 *    split element this insertion creates, and each deeper entry is the weight one
 *    level in — exactly the values `grid.layoutSnapshot()` would read back. This
 *    is how the saved 25/75 divider drag survives instead of snapping to 50/50.
 *
 *  A serialize → planLayoutRestore → replay round-trip is structure- AND
 *  weight-identical; test/panerestore.test.ts pins that with a pure model of
 *  grid's `insertBeside`. */
export interface RestoreOpenStep {
  action: RestoreAction;
  relativeTo: number | null;
  dir: "row" | "column";
  weights: number[];
}

/** The pane at a subtree's entry (its leftmost leaf) — the one leaf a split's
 *  first child contributes as the anchor its siblings open relative to. */
function entryLeafPane(node: PersistedLayoutNode): PersistedPane {
  return node.kind === "leaf" ? node.pane : entryLeafPane(node.children[0]);
}

/** The flex-grow chain from a node's own slot down its left spine to the entry
 *  leaf: `[node.weight, firstChild.weight, …, entryLeaf.weight]`. Carries every
 *  split weight the old flat list discarded (only leaf weights survived it). */
function entryWeightChain(node: PersistedLayoutNode): number[] {
  return node.kind === "leaf"
    ? [node.weight]
    : [node.weight, ...entryWeightChain(node.children[0])];
}

/** Flatten a persisted layout tree into the ordered `grid.openPane` plan that
 *  rebuilds it EXACTLY. Pure tree walk (no live panes, no DOM): the first child
 *  of each split stays put as the anchor, and its siblings open beside it in the
 *  split's direction — so a split's direction and its subtree's weights ride on
 *  the sibling steps, never collapsing distinct nestings into one sequence.
 *  main.ts turns each step into `grid.openPane(opts, dir, relativeTo)` and then
 *  applies the `weights`. */
export function planLayoutRestore(
  layout: PersistedLayoutNode,
  resumable?: SessionResumable
): RestoreOpenStep[] {
  const steps: RestoreOpenStep[] = [
    {
      action: planPaneRestore(entryLeafPane(layout), resumable),
      relativeTo: null,
      dir: "row",
      weights: entryWeightChain(layout),
    },
  ];
  const expand = (node: PersistedLayoutNode, anchorIndex: number): void => {
    if (node.kind === "leaf") return;
    // c0 keeps the anchor slot; c1..cn open beside it in this split's direction.
    // Each sibling anchors to the PREVIOUS one, not to c0: grid.insertBeside
    // splices a same-direction sibling in AFTER its anchor, so anchoring every
    // child to c0 would replay [A,B,C,D] as [A,D,C,B]. Walking the anchor forward
    // keeps insertion order.
    const childAnchors = [anchorIndex];
    for (let i = 1; i < node.children.length; i++) {
      const prevAnchor = childAnchors[i - 1];
      childAnchors.push(steps.length);
      steps.push({
        action: planPaneRestore(entryLeafPane(node.children[i]), resumable),
        relativeTo: prevAnchor,
        dir: node.dir,
        weights: entryWeightChain(node.children[i]),
      });
    }
    // Recurse to subdivide every child (a child that is itself a split gets its
    // own siblings opened relative to the anchor we just recorded for it).
    node.children.forEach((child, i) => expand(child, childAnchors[i]));
  };
  expand(layout, 0);
  return steps;
}

// ---------- #361 whole-group resume: locating the resumed pane ----------

/** Just enough of a live pane to decide whether it's the one a just-resumed
 *  session id belongs to — a plain-data projection, not a live `Pane`, so the
 *  decision below can be unit-tested without a DOM or a grid. */
export interface ResumedPaneCandidate {
  isDormant: boolean;
  sessionId: string | null;
}

/** Pick which candidate a just-resumed session id actually belongs to, among
 *  every pane currently in the tab (`main.ts`'s `resumeDormantGroup` maps
 *  `ws.grid.allPanes()` to this shape and calls it, once `resumeOrchSession`
 *  resolves — the pane itself isn't returned, only the group id, so it has to
 *  be located by scanning afterward).
 *
 *  MUST exclude dormant candidates. The tab's own dormant ORCH placeholder
 *  for this same member carries the IDENTICAL captured session id — that's
 *  the whole point of a captured record (`planPaneRestore`'s `dormant-group`
 *  case, above) — and it is still in the tree at the moment this runs (the
 *  placeholder cleanup happens later, after every member has been resumed).
 *  An unfiltered first-match would find that stale placeholder before the
 *  real, freshly-resumed pane; returning `-1` in that case (rather than the
 *  placeholder's index) is the one behavior this function exists to pin —
 *  see `test/panerestore.test.ts`'s dormant-shadow case, which fails on a
 *  version of this predicate that doesn't check `isDormant`. */
export function findResumedPaneIndex(
  candidates: readonly ResumedPaneCandidate[],
  sessionId: string
): number {
  return candidates.findIndex((p) => !p.isDormant && p.sessionId === sessionId);
}
