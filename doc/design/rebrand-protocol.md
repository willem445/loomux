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

**The clause holds within a language, and there is exactly one place it cannot.** Tab
restore parses a recorded launch command in TypeScript, across a process boundary no
Rust constant reaches, so `panerestore.ts` carries its own copy of the accepted MCP
identities. That duplication is asserted rather than argued away
(`the_frontend_accepts_every_mcp_identity_the_backend_still_mints`), and the asymmetry is
worth knowing: Rust dropping a spelling the frontend still strips is harmless, while the
frontend dropping one Rust still mints leaves a dead `--mcp-config` path in a replayed
command.

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

- **`orchestration::refusal_was_resent`** compares two sender names read out of one
  `audit.jsonl`, and that file spans the flag day. Both of its comparisons go through
  `same_audit_sender`, and they fail in opposite directions, which is why one rule for
  both matters: the first would tell an orchestrator to re-send a delivery that landed,
  the second would drop a genuinely-refused delivery off the roster entirely.
- **The persona `tools:` reader** (`ResolvedPersona`) — the one surface here where the
  answer is *not* "accept every spelling", argued in the next section.

`brand::is_host_actor` exists so that last class of question — *did we write this?* — has
one implementation. A hand-written `== AUDIT_ACTOR` at a call site is a record from before
the flag day silently reclassified as somebody else's.

## The one reader that must NOT accept every spelling

Every surface above reads a record **this app wrote** and cannot rewrite, so accepting the
old spelling is simply reading our own past output. A repo's `.github/agents/*.md`
`tools:` list is a different kind of thing: it is the **user's statement of intent**, and
the rename moved the ground under it. That makes the accept-both reflex wrong in one
direction and mandatory in the other, and the split is per-question rather than per-file:

| The file says | Question | Answer | Why |
| --- | --- | --- | --- |
| `loomux/*`, or bare `loomux` | *does this GRANT the server?* | **No** — current spelling only | The author asked for the whole server. Reading the stale name as a live grant keeps the native path and hands the delegate a filter matching no server we declare: it launches with **no orchestration tools at all**. Calling it a gap sends it to the repair path, which adds `orrerix/*` — the author's own intent, spelled the way the server is spelled now. |
| `loomux/report` | *is this a per-tool DECISION?* | **Yes** — every spelling | The author asked for exactly one tool, and that decision did not change when our server's name did. Reading the stale name as "never mentions the server" makes it an omission, and the repair then appends the full-server grant — **this app widening a narrowing nobody widened**, which is the move #222's capability closure forbids. |

So `grants_loomux_tools` takes the current name and `scopes_mcp_server_per_tool` takes
`brand::MCP_SERVERS`, evaluating `mentions && !grants` **per spelling** — which is exactly
what separates the two rows. A well-meaning collapse of the two predicates into one
"accept both everywhere" reintroduces one row's defect whichever way it collapses, so
both are pinned, including the negative control.

The human-facing half matters as much: from inside the repo that `tools:` line looks
correct — it names a server and scopes it — and the only thing that changed is a name its
author never chose and cannot see. The warning says the file names the pre-rename server
and what to write instead, rather than the bare "does not grant the MCP server" that
would send them hunting a typo they did not make.

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
- **Both `agent-*` label descriptions** (`gh.rs`'s table: *"Managed by a loomux
  orchestrator"*, *"Groomed and ready for a loomux agent to build"*). Brand prose on a
  GitHub label, matched by *name* and never by description; flipping them would leave
  every existing label describing itself differently from every new one, for no reader
  benefit. A prose-sweep item.
- **Both of the merge queue's git namespaces.** `refs/remotes/loomux-mq/*` and the
  `loomux-mq-<batch>` remote name (`mqloop.rs`) on the fetch side; `loomux/mq/<group>-<id>`
  (`mergeq.rs`) for the scratch branch pushed to the user's remote, which `mqdriver`
  guards by exact prefix so the queue can only ever force-push and delete inside it.
  Refs in somebody else's repository, with fetch, prune, lease and delete semantics
  attached: renaming either orphans every ref a live queue is holding, and renaming the
  second without its prefix check is a queue that cannot clean up after itself. Phase 4's
  class of problem — an identity on somebody's disk — and it wants the same kind of
  argument, not a sweep. (The PR *prose* the queue posts to GitHub did move: nothing
  matches it, and every human and agent who opens the PR reads it.)
- **The `<repo>-worktrees/` convention.** Derived from the repository's name, not the
  product's.
- **Rust and TypeScript identifiers** — `mask_loomux_notices`, `is_loomux_notice`, the
  `loomux-engine` crate, `loomux_lib`. Code identity follows the crate rename (phase 5, or
  never). Their *values* moved; their *names* did not, and mixing the two would have made
  this diff unreviewable for no agent-visible gain.
- **`localStorage` keys** (`loomux.defaultAgent`, …) and the workflow file's
  `authored_with:` stamp. Persisted user state — phase 4's class.
- **Brand prose that is not matched on** — roughly a thousand mentions across
  `src-tauri`. Phase 0 called for a lazy rename of narrative surfaces, and a sweep
  here would bury the protocol diff.

  **This includes agent-facing prose, and saying otherwise would be false.** Some of
  what is left is read by a delegate on every spawn: `copilot_agent_body`'s system
  prompt (*"the worker for this loomux-orchestrated group"*), `queued_text`'s
  lock-queue MCP reply (*"loomux types an `[orrerix]` notice into this pane"* — both
  brands in one sentence), and the delivery- and queue-notice text around
  `mod.rs:13912`, `:14103`, `:17352-17416` and `:18502`. Nothing matches on any of
  it, which is why it is out of scope under the governing line at the top of this
  list — not because an agent never sees it.

  `mcp.rs` moved under that same line rather than an extra one: it is agent-facing
  end to end, and the audit-actor rename had made one of its tool descriptions
  **actively wrong** — `queue_orphans` told an orchestrator to look for a `from`
  value this app no longer stamps, which is a string an agent acts on. The generated
  Copilot agent file's `description:`, the merge-queue PR body and the shim refusals
  moved with it.

## A branch in flight when the flag day lands

A feature branch rebased across a phase boundary is authoring NEW strings into surfaces
this phase has already split. It does not get to guess which side they belong on, and it
does not get to flip its whole diff either: the class is the phase's, not the branch's.
So the branch **measures its own surface on `main` and matches what it finds** — and says
so in the PR, because a silent divergence in either direction is unreviewable.

#1176's routing feature (#1209) landed just after this phase and did exactly that, with
**opposite answers on two surfaces of one feature**:

- **Its new `gh` shim refusals joined the renamed class** — the five *branded* ones, of
  nine new call sites; the other four name no product at all. On `main` every branded
  refusal message already said `orrerix` and none said `loomux`, so those five moved with
  it.
- **Its two new Rust status strings did not** — and they reached that answer by different
  routes, which is the part to take from this, because the sibling is only the easy case:
  - One sits in `gate_status_line_with`, directly beside `main`'s own *"loomux cannot
    resolve the PR's current head commit"*. Its sibling answered for it.
  - The other is in `pr_changed_files`, **a function this feature created**. There was no
    sibling to read, so the *surface* answered instead: the Rust gate-status line is not
    this phase's renamed class, and a string authored onto a surface joins the surface it
    is on, not the diff that carries it.

  Both had been renamed first and were then reverted, on the governing observation:
  **a function emitting two spellings of its own product name is worse than a surface
  uniformly on the old one.** A surface moves as a surface, when its phase comes — so
  where there is no sibling, the question is what the SURFACE says, never what the
  branch would prefer.

Identifiers were untouched on both sides (`loomux_block_wf`, `loomux_audit`,
`LOOMUX_GROUP_DIR`, `loomux_route_scan`) — this phase renamed strings, not symbols.

### The census that decides it is a measurement, and a blind one is easy to take

#1209 first reported that census as **32 call sites / 18 saying `orrerix`**, and it was
wrong on both figures. The counting regex captured the refusal code as `([a-z-]+)` — an
alphabet with no digit in it — so `loomux_block_wf "no-sha256" …` never matched at all,
and that site is one of the ones whose message says `orrerix`, which is exactly why both
numbers were light by one rather than only the total. Recount by walking the real quotes
and cross-check against a raw occurrence count of the call itself. The measured answer,
still true at `6b521ae4`: **33 call sites, 19 `orrerix`, 0 `loomux`, 14 mentioning
neither.** A census that cannot see one of its own subjects is not a census.

## Where the vocabulary lives

`crates/loomux-engine/src/brand.rs`, beside phase 4's filesystem and environment seam,
under one module doc. It is one module because it is one deprecation ledger: every
`LEGACY_` constant in it is somebody's working setup, and deleting one is a deliberate,
separately-argued break rather than tidying.
