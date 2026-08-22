# Protocol identities: `loomux` → `orrerix` (#1153 phase 3)

Phase 4 renamed the things on **our** disk and the operator's. This phase renames the
things **agents** match on: the marker every notice opens with, the MCP server they call
tools on, the token header their CLI presents to it, the actor this app signs its own
records with, and the environment it exports into every pane.

The whole phase turns on one distinction, and it is not one the two phases share.

## A protocol identity is a string somebody else already holds a copy of

Phase 4's identities were *lookups*. `.loomux/workflow.yml` is a file we can still find
after the rename; `LOOMUX_DATA_DIR` is a variable we can still read. Dual-**discovery**
works there because we control the reading, and reading is all that was ever at stake.

None of that is true here. Every identity in this phase is already written down somewhere
we may never rewrite:

| Identity | Whose copy is out there, and where |
| --- | --- |
| The notice marker | A **recorded transcript** the agent CLI wrote, months ago. A **pane's scrollback**, spanning the upgrade. A `queue.json` entry persisted before the app restarted. |
| The MCP server name and token header | A **generated config file in a live group's directory**, written once at group create, that every agent in that group presents on every call it will ever make. A **recorded launch command line** in a saved tab. |
| The audit actor | Every `audit.jsonl` line, every queued delivery, and every collapsed-note placeholder in `board.json` written before the flag day. |
| The env exports | An operator's shell profile and CI config; a repo persona; **and this app's own shims, already on disk in a live group.** |

So the rule here is strictly stronger than dual-discovery, and it is stated once, in
`crates/loomux-engine/src/brand.rs`:

> **Emit exactly one spelling. Accept every spelling on every reading surface. Write the
> accepted set down exactly once.**

The third clause is the one that is easy to skip and expensive to skip. Two lists that
agree today — a detector's `starts_with` chain and a sanitizer's neutralize set — only
have to disagree once, and the disagreement is not a compile error. It is a marker one
side treats as a host notice while the other leaves un-scrubbed, which *is* the forgery
hole. `NOTICE_MARKERS` and `AGENT_TOKEN_HEADERS` are arrays both sides iterate, and
`brand::tests::every_accepted_marker_is_also_neutralized` asserts the two halves against
the array itself rather than against a list retyped in a test.

## The readers, and what dropping each one would actually cost

Not one of these is a courtesy to a user with a stale config. Each is a live defect if it
is missing, and not one of them fails loudly.

- **`sessions::detect_orch_signature`** scrapes three kickoff phrases and the notice
  marker out of transcripts *the agent CLI wrote in the past*. Nothing rewrites those
  files. Drop either product name and every session recorded before the flag day silently
  loses its role and its group — no error, no red, just a user's group that stops
  offering to resume. **This is the one dual-accept in #1153 that can never be retired.**
- **`notify::sanitize_pane_text`** neutralizes `[` → `(` in untrusted text so nothing
  outside this app can produce a notice-shaped row. It neutralizes *any* bracketed marker
  by construction, which is why it needed no edit — but an agent briefed before the
  rename still reads `[loomux] …` as a host notice, so the guard has to keep covering the
  old marker for as long as such an agent can exist. The test asserts that over the
  array rather than trusting the construction.
- **`mcp.rs`'s token-header read.** An agent's MCP config is written once, at group
  create, and lives in that group's dir. A group created before the flag day presents the
  old header on every call it will ever make; a server reading only the new one would
  fail *every tool call in every live group* the moment the app updated underneath it.
- **`queue::is_loomux_notice`** requires both a host `from` and a leading marker, and
  reads entries persisted before the restart — which are precisely the stuck notices
  #590's diagnosis exists to find.
- **`notes_represented`** (`board.json`) asks whether a collapsed-note placeholder is
  ours. An `!=` against today's spelling would count each pre-rename placeholder as one
  note and reset a task's accumulated collapse total — #245's review finding, reopened.
- **`panerestore.stripSoloMcpFlags`** reads a *recorded* launch command. Not recognising
  the old MCP identity leaves a dead `--mcp-config` path in the replayed command, so the
  pane boots against a file agent exit already deleted: the exact failure the excision
  exists to prevent, reintroduced for every tab a user had open across the upgrade.

`brand::is_host_actor` exists so that last class of question — *did we write this?* — has
one implementation. A hand-written `== AUDIT_ACTOR` at a call site is a record from before
the flag day silently reclassified as somebody else's.

## The flag day, and what it does and does not promise

**Emitted names are not per-group.** There is no generation stamp and no per-group marker
table, deliberately: a stamp would have to be threaded through every notice emitter, and
what it would buy — a live pre-rename agent seeing the marker its instruction file named —
is already bounded by the operational rule below.

**The policy is: renamed protocol applies to new groups; drain live groups first.** A
group created before the upgrade keeps working — every reader above accepts what it holds
— but its agents were *briefed* with the old vocabulary, and notices they receive after
the upgrade carry the new marker. They will read those notices (they are plain text typed
into a pane); they may not match them against the phrase their instruction file taught
them. That is the residual, stated rather than argued away, and the remedy is the one the
phase-0 plan named: let a live group finish.

**Instruction files are re-rendered per spawn**, so an agent spawned after the upgrade —
even into an old group — reads the new vocabulary. It is the panes already running that
carry the old brief.

**The generated shims and hooks resolve both, and that is not symmetry for its own sake.**
`agent_pane_env` exports both spellings, but a pane spawned by an *older* build exports
only the legacy pair, and a shim this build regenerates has to keep working inside it. So
every generated script resolves `${ORRERIX_X:-$LOOMUX_X}` once, at the top: correct in
both directions of the upgrade, and a form that keeps working unchanged the day the legacy
export is dropped.

## The lessons file: a rename that was hiding a defect

The four role templates hard-coded `.loomux/lessons.md`. Phase 4 made repo config resolve
**per file** — `.orrerix/` preferred, `.loomux/` read when it is the only one there — and
left these templates alone deliberately, because a `pre222` re-bless wants a round where
nothing else moved.

Three of the four are *read* instructions ("skim it once at session start"), and in a repo
that has moved they point at a file that is not there: wrong, but visibly wrong.

`orchestrator.md`'s learning loop is a **write** instruction — commit an entry to that
file — and that one is not visibly wrong. It is silently ignored. `lessons_path` prefers
`.orrerix/lessons.md`, so in a repo holding both, the entry an orchestrator commits to
`.loomux/lessons.md` is never read into any future kickoff. The lesson is written, the PR
merges, and nothing ever loads it.

All four now carry `{{LESSONS_PATH}}`, a per-group VALUE variable — like `{{HOLD_LABEL}}`
and unlike `LIVE`'s workflow-conditional keys, so the golden fixture keeps the literal and
the pin's `render_with_legacy_vars` renders it — resolved by `lessons::lessons_path(repo)`,
the same function the kickoff's own lessons note already used to say where the bytes came
from.

## What this phase deliberately does not flip

Each of these was reachable and left alone on purpose. The line is: **a string an agent or
an operator matches on moves; a name that is code identity, a published identity, or
somebody else's filename does not.**

- **`GENERATED_AGENT_PREFIX`** (`loomux-<group>-<block>`, the `--agent` handle). It names
  files under the user's own `~/.claude/agents` and `~/.copilot/agents`. Moving the write
  prefix without teaching `end_group`'s reclaim *and* `sweep_orphaned_agent_files` both
  spellings is #502's file-accumulation incident reintroduced; moving all three is phase
  4's class of problem, not this one's, and wants its own argument.
- **The `loomux` launcher binary** and the self-launch refusal shim that names it. The npm
  package and the installed executable are still called that — phase 5 — and a refusal
  naming a command the user does not have would be useless. Only that shim's tool-surface
  sentence moved.
- **The `agent-managed` label description** (`"Managed by a loomux orchestrator"`). Brand
  prose on a GitHub label, matched by *name* and never by description; flipping it would
  leave every existing label describing itself differently from every new one, for no
  reader benefit. A prose-sweep item.
- **The `<repo>-worktrees/` convention.** Derived from the repository's name, not the
  product's.
- **Rust and TypeScript identifiers** — `mask_loomux_notices`, `is_loomux_notice`, the
  `loomux-engine` crate, `loomux_lib`. Code identity follows the crate rename (phase 5, or
  never). Their *values* moved; their *names* did not, and mixing the two would have made
  this diff unreviewable for no agent-visible gain.
- **`localStorage` keys** (`loomux.defaultAgent`, …) and the workflow file's
  `authored_with:` stamp. Persisted user state — phase 4's class.
- **Brand prose in Rust doc and code comments.** Roughly a thousand mentions across
  `src-tauri`, none of them read by an agent. Phase 0 called for a lazy rename of
  narrative surfaces, and a sweep here would bury the protocol diff.

## Where the vocabulary lives

`crates/loomux-engine/src/brand.rs`, beside phase 4's filesystem and environment seam,
under one module doc. It is one module because it is one deprecation ledger: every
`LEGACY_` constant in it is somebody's working setup, and deleting one is a deliberate,
separately-argued break rather than tidying.
