# Harness adapters: a pane that reports instead of a pane that is read

#84 slice R0. The roadmap this implements is #84's nine-part plan
(comment [5466290210](https://github.com/willem445/orrerix/issues/84#issuecomment-5466290210)
onward — §2 vocabulary, §8.1 slices, §9 risks). The human has chosen **Claude
Code as the first structured harness**; R1 builds against this note and R2 wires
it.

This note is a **public contract**. Everything R1 and R2 may rely on is here;
anything not here is not agreed. Changing a name or a shape below is an edit to
this file first.

Amends: `doc/design/engine-extraction.md` §2 (what `PaneHost` hands back) and
`doc/design/remote-engine-protocol.md` §4 (two additive events, one roster
field). Reads on: `doc/design/opencode.md` (how a harness is driven today),
`doc/design/session-id-learning.md` (#440), `doc/design/human-questions.md`
(#946), `doc/design/needs-you-items.md` (#1151),
`doc/design/group-cost-tracking.md` (#42), `doc/design/workflows.md` (#222).

**Every Claude Code fact below is cited to the official docs by URL, read
2026-09-03.** No CLI was run (CLAUDE.md constraint 3). Where the docs do not
settle something it is in §9 as a live-validation item for the human, never
guessed.

---

## 0. What changes and what does not

| | today (PTY) | structured |
|---|---|---|
| how orrerix talks to the CLI | bytes into a ConPTY, echo-verified paste, blind Enter | one NDJSON line on the child's stdin |
| how orrerix learns what happened | scrape the pane's ring: readiness markers, question grids, statusline | the child's NDJSON stdout |
| what the human sees | the vendor's TUI, byte for byte | orrerix's VT rendering of the same events, in the same xterm pane |
| who admits a delivery | `loomux_engine::queue` | **unchanged** — `queue` is the front door for both |
| what a block may grant | nothing (#222 capability closure) | **unchanged** — `driver:` selects a transport, never a capability |

**The one-line summary of the cut:** `PaneHost::request_pane` hands back a
`Box<dyn AgentPane>` — a driver object — instead of a byte pipe. A PTY pane and
a structured pane are two implementations of one trait, so there is one spawn
path, one delivery door, one idle model, and one thing the daemon's `PaneHost`
(#888 C3) has to implement.

---

## 1. The vocabulary: `AgentPane` and `HarnessEvent`

### 1.1 The trait

```rust
pub enum PaneKind { Pty, Structured(Harness) }
pub enum Harness  { Claude }          // R1's only variant; opencode/ACP are R3/R4

pub trait AgentPane: Send + Sync {
    fn kind(&self) -> PaneKind;
    fn send(&self, turn: Turn) -> Result<SendReceipt, String>;
    fn answer(&self, req: RequestId, d: Decision) -> Result<(), String>;
    fn interrupt(&self) -> Result<(), String>;
    fn events(&self) -> EventRx;                 // Receiver<HarnessEvent>
    fn session_id(&self) -> Option<String>;      // None == not known yet
}

pub enum Turn { Kickoff(String), Prompt(String), Notice(String), Human(String) }
```

`Turn` is the delivery vocabulary orrerix already has
(`Delivery::{FreshKickoff, ResumeKickoff, MidSession}` plus the `[orrerix]`
notice channel and the #43 compose strip), named once so the drainer's last step
is `pane.send(turn)` on both kinds.

### 1.2 The event

```rust
pub enum HarnessEvent {
    Booted { session: Option<String>, model: Option<String>, capabilities: Vec<String> },
    TurnStarted { turn: TurnId },
    Text { turn: TurnId, delta: String },
    ToolCall { turn: TurnId, id: ToolUseId, name: String, input: serde_json::Value },
    ToolResult { turn: TurnId, id: ToolUseId, ok: bool },
    PermissionRequest { id: RequestId, tool: String, input: serde_json::Value },
    PermissionSettled { id: RequestId, decision: Decision, by: DecisionSource },
    TurnEnded { turn: TurnId, usage: Option<Usage>, cost: Option<Cost>, stop: StopReason },
    Compacted { trigger: CompactTrigger, pre_tokens: Option<u64> },
    Exited { code: Option<i32> },
    Observed(ObservedEvent),
}

pub enum ObservedEvent { QuestionSuspected { .. }, ReadyMarker, Quiet, Painted }
```

### 1.3 How a PTY pane maps onto it — and how "unknown" is spelled

A PTY pane implements the **same** trait and emits the **same** enum. It is not
given a parallel vocabulary, because a second vocabulary means a second consumer
for every feature that reads panes.

**Unknown has exactly two spellings, and neither is a value.**

1. **A fact the pane does not have is `None`**, never a sentinel. An `"unknown"`
   string or a `-1` is a value every `match` arm can forget to check, and it
   reads as data. `session_id() -> Option<String>` and `usage: Option<Usage>`
   are the shape; a PTY pane answers `None` until the session watcher binds an
   id (`session-id-learning.md`).
2. **A fact the pane only INFERRED is `Observed(..)`**, never the reported
   variant. Scraped evidence and reported fact must not share a constructor: a
   consumer written for `PermissionRequest` would otherwise silently accept a
   grid heuristic, and the whole point of the structured path is that it does not
   have to. This is the one place the enum is deliberately not symmetric.

| event | structured (claude) | PTY |
|---|---|---|
| `Booted` | `system/init`: `session_id`, `model`, `capabilities` | emitted on the readiness gate; `session` `None` until learned, `model`/`capabilities` empty |
| `TurnStarted` / `TurnEnded` | per turn, from the stream | **never emitted** — a PTY pane cannot see a turn boundary |
| `Text` | assistant text and `stream_event` deltas | **never** — the bytes go to the ring; there is no message structure to report |
| `ToolCall` / `ToolResult` | `tool_use` / `tool_result` blocks | **never** |
| `PermissionRequest` / `PermissionSettled` | the permission-prompt tool call and its answer | **never** — a suspected question is `Observed(QuestionSuspected{..})` |
| `TurnEnded{usage, cost}` | the `result` message (§7) | **never** — usage is polled out of band, not evented |
| `Compacted` | hook events in the stream (`--include-hook-events`) | **never** — today's marker hooks stay, and they drive the existing detector, not this event |
| `Exited` | child exit | child exit — the one variant both kinds emit identically |
| `Observed(..)` | **never emitted** | readiness marker, question grid, quiet/painted evidence |

**The two "never emitted" columns are the contract, not an omission.** A feature
that needs `ToolCall` is a feature that works on structured panes and degrades
— visibly, by absence — on PTY panes; it may not fall back to scraping one out of
the ring, because that is the machinery this issue exists to retire.

---

## 2. The `driver:` block key

### 2.1 The key

```yaml
blocks:
  - id: worker-adv
    kind: worker
    cli: claude
    driver: structured      # pty | structured   — default: pty
```

`.orrerix/workflow.yml`, with `.loomux/` still read as the legacy name
(`brand::pick_repo_path`, unchanged by this note).

### 2.2 What it changes

- Which `AgentPane` implementation `PaneHost::request_pane` returns for panes
  spawned from that block.
- The argv that implementation builds (§6).
- Whether the attention scan and the screen-scraped question gate run for that
  pane: **off** for `Structured` (§5.4).
- Whether `write_pty` is accepted for that pane: **refused** for `Structured`
  (§5.5).

### 2.3 What it must NOT change, and why each is mechanical

**It can never grant a capability.** `workflows.md`'s rule is absolute and this
key is inside it: `driver:` is a selection from a closed two-value enum, and
`Role::containment()` — the deny flags, the cwd/worktree rule, the MCP tool scope
— is selected by `kind:`, never by a repo file. Both drivers emit the same
`--disallowedTools`/`--allowedTools`/containment argv for a given class. A
structured driver that dropped a deny flag would be a capability grant by
transport, so R2 owes a test that the containment argv for one block is
byte-identical across both driver values.

**It is refused, never coerced.** Two stages, the `cli:`/`effort:` precedent:
`parse_workflow` refuses `driver: structured` on a `cli:` whose `CLI_CAPS` row
has no `structured_driver`, and `spawn_agent` refuses it again at spawn. An
unknown value (`driver: acp`, before R4 exists) is a `deny_unknown_fields`-class
validation error, not a fall-through to `pty`. Today that means: refused on
`copilot`, `gemini` and `opencode`; accepted on `claude` only.

**It is pinned at launch (#222 consent).** `create_group` reads
`.orrerix/workflow.yml` on `Launch::Fresh` only; a resume runs the blocks
persisted in `group.json`. So editing `driver:` in the repo cannot change the
pane kind of a running or resumed group — the human sees the new roster at the
launcher, or at the live toggle, before it takes effect, and drift on resume is
audited (`workflow-changed-since-launch`), never applied. **A `driver:` change is
exactly the kind that must not arrive silently**: it changes what the human's own
pane shows them.

**It joins the schema manifest.** `src/workflow-schema.json` gains a `driver`
field on the block section — `values: ["pty","structured"]`, `default: "pty"` —
and both enforcers apply: `src-tauri/tests/orchestration.rs` (field names off
`workflow_schema_keys()`, values off `workflow_schema_field_facts()`, and the
refuse-vs-clamp behaviour driven through `parse_workflow`) and
`test/workflowschema.test.ts` (parsed, serialized, and either claimed by a form
control or explicitly listed as not yet having one).

### 2.4 The roster field

`agents.json` gains `pane_kind` per agent row — additive, absent means `pty`, so
every roster written before R2 reads correctly. It is a **record of what was
spawned**, not a control: nothing reads it to decide how to drive a pane, only to
render it and to answer `PaneKind` for a pane this process did not spawn.

---

## 3. `permissions.json` — the policy, and the prompt the policy cannot decide

A structured pane has no dialog. Every prompt Claude Code would have drawn
becomes a call into orrerix's own MCP server
(`--permission-prompt-tool mcp__orrerix__permission_prompt`, built from
`brand::MCP_TOOL_PREFIX`, never a literal). That call has to be answered by
somebody, and this section says who.

### 3.1 Two things in one file

`<group dir>/permissions.json`, beside `questions.json` and `needs-you.json`,
following `humanq.rs`'s idiom (atomic write, one serializing mutex per group, a
closed source enum, audited settles and refusals):

- **`policy`** — rules that auto-decide, keyed by **agent id**. An agent's cwd is
  its worktree, so "per-worktree policy" and "per-agent policy" name the same
  scope; the file keys on the id because the id is a validated `PathSegment`
  family member and a path is not.
- **`pending`** — requests no rule decided: the tool name, the arguments, when it
  arrived, and once settled the decision and its source.

### 3.2 The decision ladder

1. **Deny rules first**, from the block's class containment. A deny here is final
   and is never raised to the human — a human cannot grant what the class
   forbids, and offering the choice would be the self-served-gate shape CLAUDE.md
   constraint 9 refuses.
2. **Allow rules** — the same patterns the argv path emits today, but applied at
   **decision time**. That is the whole gain: argv can only express what was known
   at launch, while the prompt tool sees the actual arguments, so a policy can
   allow `Bash(git status)` and refuse `Bash(git push)` on the concrete call.
3. **No rule matched → the human.** This is the fail-closed direction, and it is
   §3.3.

### 3.3 How an undecided prompt reaches the human

**Through the needs-you registry, never a pane dialog.** `needs-you.json` gains
an item linking the pending request; the NEEDS-YOU panel renders it beside
questions and demos; the answer arrives through a trusted Tauri command whose
source is a property of the entry point, not an argument. **Every agent may be
prompted. No agent may ever answer a prompt** — the `questions.json` trust
boundary, verbatim, for the same reason: an agent that could answer its own
permission request has a gate that is theatre. R2 owes the boundary test
(`no_agent_token_can_answer_a_permission_request_through_the_mcp_surface`, ending
in a positive control that settles one through the trusted path).

**Why not a dialog in the pane:** INVARIANT 2. A dialog on an orchestrator's
screen stops that pane taking *any* delivery, which strands every agent reporting
to it — the #946 incident. A structured pane has no dialog to draw, and this
design must not reintroduce one on the orrerix side.

**Parking is scoped by role, and the asymmetry is deliberate:**

| block kind | undecided prompt | why |
|---|---|---|
| `worker`, `reviewer`, `planner` | the tool call **parks** until answered; that pane's turn waits | one pane waits on a decision only a human may make — the correct scope, and the same shape as `ask_human` |
| `orchestrator`, `manager` | **denied immediately**, with a message naming the registry, and a needs-you item raised | a parked orchestrator is #946 one level down: machine progress must never stop on human absence |

A parked request is bounded by a **per-pane cap** on pending entries; past the
cap, further prompts are denied with a message naming the cap. It is **not**
bounded by a timer: a timed auto-deny is a decision nobody made, recorded as
though somebody had.

### 3.4 Three Claude Code facts this section is built on

- **With `--permission-prompt-tool` set, Claude Code emits no `permission_denied`
  stream event at all — "not even for the rule denials it decides on its own"** —
  and `permission_denials` on the `result` message is "the authoritative record"
  ([typescript SDK reference, `SDKPermissionDeniedMessage`](https://code.claude.com/docs/en/agent-sdk/typescript#sdkpermissiondeniedmessage)).
  So orrerix's audit of denials is read off `result.permission_denials`, not off
  a per-denial event. A driver that audited only what it was asked about would
  silently under-record every rule denial.
- **An MCP tool marked `_meta["anthropic/requiresUserInteraction"]` cannot be
  approved this way**: "an `allow` result from the prompt tool for a flagged tool
  is converted to a deny"
  ([MCP reference](https://code.claude.com/docs/en/mcp#require-approval-for-a-specific-tool)).
  Consequence: orrerix must not mark its own MCP tools that way, and a `policy`
  allow for such a tool is not honoured — R2 says so in the refusal message
  rather than leaving the allow looking effective.
- **Claude Code waits for the prompt tool's MCP server before the first turn**,
  up to `MCP_TIMEOUT` (30 s by default)
  ([cli-reference, `--permission-prompt-tool`](https://code.claude.com/docs/en/cli-reference)).
  orrerix's MCP server is already listening before a pane spawns, so this is a
  constraint on ordering rather than new work — but the driver must fail the
  spawn loudly if it is not, rather than producing a pane whose every prompt
  times out.

**`AskUserQuestion` stays denied** for adapter agents, as it is for the
orchestrator today, and clarifying questions go through `ask_human`. Whether that
tool even reaches `--permission-prompt-tool` is undocumented — §9.

---

## 4. The per-pane event log

### 4.1 Path and shape

`<group dir>/panes/<agent-id>.events.jsonl` — one JSON object per line, append
only.

`<agent-id>` is interpolated into a **file name**, so it is inside the
`src-tauri/tests/pathseg.rs` scan's trigger shape (an interpolation plus a
file-extension literal in a `format!` template): R2 adds the allowlist row and
the proof that row names, and the id is already a `pathseg::check_segment` family
member (CLAUDE.md constraint 6). `panes/` is created under the group dir, which
is the single `group_dir_at` join — no second join is added.

Each line carries a monotonic `seq`, a wall-clock `ts`, and the `HarnessEvent` as
emitted.

**Unknown harness message types are ignored, not fatal** (the protocol note's
§4.4 rule applied inward) — but the raw line is written to this log, truncated to
4 KiB, under an `unknown` kind. A silently discarded message is a message nobody
can add support for later.

### 4.2 Rotation

Size-based: **8 MiB per segment, 4 segments retained** (`.events.jsonl`,
`.events.1.jsonl`, …), oldest dropped — a 32 MiB per-pane ceiling. Deleted with
the group directory; there is no separate retention policy and no compaction.

### 4.3 What the audit log gets, and what stays here

The group audit log is a small, permanently retained, human-read record of
decisions. It gets, per pane:

- `ToolCall` — name plus a bounded argument preview;
- every `PermissionRequest` with its `PermissionSettled` — decision **and**
  source;
- `TurnEnded` usage and cost;
- `Exited`.

It does **not** get assistant text, tool results, `stream_event` deltas, or
`Booted` detail. Those stay in the per-pane log. A transcript in the audit log
would drown the decisions the log exists for, and would put model prose in the
file a human reads to reconstruct what happened.

### 4.4 Relationship to the remote protocol's H4

H4's reattach contract — "the live screen and a recent tail (256 KiB per pane),
not infinite scrollback" — is a fact about the **PTY ring**. A structured pane
can replay from this log instead, so #888's C5 reattach is *stronger* for
structured panes: full transcript back to the oldest retained segment. That is a
ceiling, stated: 32 MiB of events, not infinite either. This is the one place
this roadmap raises a #888 contract rather than consuming one.

---

## 5. The rendering contract

### 5.1 One surface

A structured pane renders into **the existing xterm surface**. A pure
`transcript::render` in the engine turns each `HarnessEvent` into VT bytes, and
those bytes go into that pane's `OutputBuf` ring through the same coalescer that
feeds `pty-output` today.

Because they land in the ring, `get_output`, termgrid replay, thumbnails,
`last_exit_tail` and C5 replay-on-attach keep working with no API change. That is
why the alternative — a DOM transcript view beside the terminal — is rejected: it
breaks all five for exactly the panes the feature is for.

### 5.2 Constraint 1 is satisfied by construction, and by one rule

There is no PTY behind a structured pane, so no code path can call a ConPTY
resize for one. The failure constraint 1 exists to prevent — a repaint that
pollutes scrollback — can still arrive by a different road, so:

> **The renderer never rewrites bytes it has already emitted.** Lines are wrapped
> to the pane's cols at emit time; a later width change re-wraps nothing. Reflow
> of already-emitted output is xterm's own, exactly as for a PTY pane.

A renderer that re-emitted its transcript on a resize would be constraint 1's
failure without a ConPTY — worse, because it would not even be findable by
grepping for the resize call.

### 5.3 H4's ring rules apply unchanged

The coalescer bound (at most one emit per pane per 16 ms, 64 KiB batch), the
**tee ordering** (append to the ring *before* sending to the pump channel, so no
client's link quality can affect orchestration), and the remote translation of P6
(a bounded per-client send buffer; on overflow, drop that client's stream and send
a `0x02` resync marker) are properties of the **sink**, not of the source. They
hold for renderer output verbatim. The two new event names join
`test/perfpolicy.test.ts`'s `STREAMS` manifest with a rate class and a bound
(§5.6) — INV-3 is keyed by event name, and a stream that arrives undeclared is the
failure that manifest exists to stop.

### 5.4 The attention scan and question gate are OFF for `Structured`

orrerix must never scrape what orrerix itself rendered. Beyond being pointless,
it is forgeable by construction: model prose shaped like a question grid would be
read as one. Attention and pending-request state for a structured pane come from
the event stream and from `permissions.json`, which is also what makes them
answerable from the board and from a remote client — the thing scraping could
never give #888.

### 5.5 Input

`write_pty` on a structured pane is **refused**, not silently accepted: there is
no PTY, and a write that appears to succeed and reaches nothing is the worst of
the three options. Human input arrives as `Turn::Human` from the #43 compose
strip, which is already the serialized human path for the orchestrator pane. On
the wire this is a typed refusal (`invalid_argument`), and the roster's
`pane_kind` lets a client grey the keyboard out rather than discover it by
trying.

Notices are `Turn::Notice`. Nothing scrapes a structured pane, so nothing needs
masking: `mask_loomux_notices_with_record` and the one-row maskability contract
stay PTY-only, and the `[orrerix]` marker survives on a structured pane as a
display prefix.

### 5.6 The two additive protocol events

`remote-engine-protocol.md` §4.4 is additive-only, and these are additive:

| event | rate class | bound |
|---|---|---|
| `orch-pane-transcript` | `producer` | backend-coalesced — the same 16 ms / 64 KiB coalescer as `pty-output`, because it is the same sink |
| `orch-pane-request` | `lifecycle` | one event per permission request and one per settle; a pane cannot produce them faster than it produces tool calls |

No new **frame kind**: both ride the existing `{"t":"ev","name":…}` shape. A
client that does not know them ignores them, which is §4.4's rule doing its job.

---

## 6. Session id and resume for Claude Code under stream-json

### 6.1 The launch line

```
claude -p
  --input-format stream-json --output-format stream-json --verbose
  --include-partial-messages
  {--session-id <uuid> | --resume <id>}
  --mcp-config <file> --strict-mcp-config
  --permission-prompt-tool mcp__orrerix__permission_prompt
  --permission-mode <mode> --allowedTools ... --disallowedTools ...
  --settings <hooks file> --agent <handle> --effort <level>
```

Every flag is on
[cli-reference](https://code.claude.com/docs/en/cli-reference) today:
`--input-format` ("options: `text`, `stream-json`"), `--output-format` ("`text`,
`json`, `stream-json`"), `--include-partial-messages` ("Requires `--print` and
`--output-format stream-json`"), `--session-id` ("must be a valid UUID"),
`--resume`, `--mcp-config`, `--strict-mcp-config`, `--permission-prompt-tool`,
`--permission-mode`, `--allowedTools`, `--disallowedTools`, `--settings`,
`--agent`, `--effort`, `--verbose`.

Two flags are deliberately **not** used:

- **`--bare`** — it "skip[s] auto-discovery of hooks, skills, custom commands,
  subagents, plugins, MCP servers, auto memory, and CLAUDE.md"
  ([headless](https://code.claude.com/docs/en/headless#start-faster-with-bare-mode)).
  A pane that does not load the repo's CLAUDE.md is not the pane orrerix launches
  today.
- **`--no-session-persistence`** — it makes the session unresumable
  ([cli-reference](https://code.claude.com/docs/en/cli-reference)), and resume is
  the feature.

### 6.2 The id is minted, not learned

`--session-id` pre-assigns the id, exactly as the PTY path does today, and
`system/init` reports `session_id` back
([`SDKSystemMessage`](https://code.claude.com/docs/en/agent-sdk/typescript#sdksystemmessage)).
So for a structured claude pane there is nothing to discover: **the session
watcher does not run**, and `session-id-learning.md`'s reconciliation and its
"refuse, never guess" ambiguity policy stay scoped to the panes that still need
them — copilot, opencode, and every PTY claude pane.

**Decision — a mismatch kills the pane.** If `system/init` reports a `session_id`
different from the one orrerix minted, the driver terminates the pane and audits
it rather than adopting the reported id. A mismatch means the flag did not take
effect, so the pane is not the session orrerix believes it is, and every
downstream record — usage, resume, transcript — would key on the wrong one. Fail
closed; do not reconcile.

### 6.3 Resume

`--resume <session-id>` resumes by id. The docs are explicit that a `-p` session
is out of the picker and out of `--continue` but "You can still resume one by
passing its session ID to `claude --resume <session-id>`", and that the search
covers "the current project directory and its git worktrees, then every other
project on this machine"
([sessions](https://code.claude.com/docs/en/sessions#resume-a-session)).

**Three resume facts, and they are the whole reason this section exists:**

1. **Configuration flags are NOT restored.** "If the session depended on
   `--mcp-config`, `--settings`, `--plugin-dir`, `--fallback-model`, or
   directories added with `--add-dir`, pass them again when you resume"
   ([sessions](https://code.claude.com/docs/en/sessions#what-a-resumed-session-restores)).
   So the resume argv repeats **every** flag of the fresh launch, and R2 owes a
   test that the two argv lines differ in exactly one element
   (`--session-id <uuid>` becoming `--resume <id>`).
2. **The permission mode is NOT restored under `-p`.** "Non-interactive: `claude
   -p --resume` or `claude -p --continue`. Claude Code starts the run in the
   permission mode a new `claude -p` run would start in"
   ([sessions](https://code.claude.com/docs/en/sessions#permission-mode-on-resume)),
   and for `-p` "the built-in starting permission mode is Manual on every plan,
   so pass the permission mode you want"
   ([headless](https://code.claude.com/docs/en/headless#auto-approve-tools)).
   `--permission-mode` is therefore mandatory on both lines, not an optional
   posture knob.
3. **One session, one owner.** "If you resume the same session in two terminals
   without forking, messages from both interleave into one transcript"
   ([sessions](https://code.claude.com/docs/en/sessions#branch-a-session)). A
   structured pane and a PTY pane must never hold one session id at once. The
   roster's single-owner rule already enforces that; the consequence of breaking
   it is a corrupted transcript, so it is stated rather than assumed.

`--fork-session` ("When resuming, create a new session ID instead of reusing the
original") is the documented way to branch, and is **not** used by R2: a forked id
is a different pane identity, and orrerix's identity is the minted one.

### 6.4 Termination

"If you stop a `claude -p` run with SIGTERM … Claude Code exits with code 143 …
leaves the turn that was in progress unfinished and records no result for it. To
end the turn instead, send SIGINT"
([headless](https://code.claude.com/docs/en/headless#stop-a-run-with-sigterm)).

So `kill_agent` on a structured claude pane ends the turn first and kills second.
What "send SIGINT" means on this project's Windows baseline is §9's item 4.

---

## 7. Usage and cost, per harness

### 7.1 What stream-json gives directly

The `result` message — the last line of a turn's stream
([headless](https://code.claude.com/docs/en/headless#stream-responses)) — carries
the numbers in band, with no file to find:

| field | what it is |
|---|---|
| `usage` | token counts for **that turn**, **main loop only** — "Excludes subagent and auxiliary model calls, and is per-turn in streaming-input sessions" |
| `modelUsage` | per model: `inputTokens`, `outputTokens`, `thinkingTokens?`, `cacheReadInputTokens`, `cacheCreationInputTokens`, `webSearchRequests`, `costUSD`, `costBasis?` — subagents included |
| `total_cost_usd` | cumulative estimated USD for the call, subagents included |
| `permission_denials` | the authoritative denial record (§3.4) |

([`SDKResultMessage`](https://code.claude.com/docs/en/agent-sdk/typescript#sdkresultmessage),
[`ModelUsage`](https://code.claude.com/docs/en/agent-sdk/typescript#modelusage),
[cost-tracking](https://code.claude.com/docs/en/agent-sdk/cost-tracking).)

**Four traps, each a decision R2 implements rather than discovers:**

1. **`modelUsage` and `total_cost_usd` are CUMULATIVE for the call; `usage` is
   per turn.** "read the latest result for call totals rather than summing across
   results". Summing `total_cost_usd` across a long-lived pane's turns would
   multiply its cost by roughly the turn count.
2. **They reset on `/clear`, `/reset` and `/new`**, and the `/clear` turn's result
   "carries a new `session_id`". orrerix does not send those to a delegate pane,
   but a human at the compose strip can — so the collector reads the reset
   boundary rather than assuming monotonicity.
3. **`thinkingTokens` is already inside `outputTokens`** — "don't add the two
   together". orrerix's four buckets take it folded into `output`, the same fold
   `opencode.md` already argued for reasoning tokens, so one bucket keeps meaning
   one thing across harnesses.
4. **`usage` undercounts as soon as subagents run**; `modelUsage` is the
   whole-tree figure, and it is the one R2 reads.

### 7.2 Basis, honestly labelled

`total_cost_usd` and `costUSD` are, in the vendor's own words, "client-side
estimates, not authoritative billing data … Do not bill end users or trigger
financial decisions from these fields"
([cost-tracking](https://code.claude.com/docs/en/agent-sdk/cost-tracking)).

So a structured claude pane's dollars land in `group_usage`'s **estimated** basis
— the same basis its PTY sibling uses — not in `reported`. What changes is the
**source tag**, from `transcript`/`statusline` to `stream`. Nothing else in
`group-cost-tracking.md` moves.

### 7.3 The #2167 class

#2167 is a **source-resolution** failure: the collector derives
`~/.claude/projects/<cwd-slug>/<session>.jsonl` from a pane's cwd, that stopped
resolving for worktree cwds, and the fallback is a statusline that reads `$0.00`
on subscription plans — so Claude delegate panes recorded 0 tokens.

A structured pane derives no path at all. The numbers arrive on the stream the
driver is already reading, so the slug derivation, the projects-root scan and the
statusline fallback are all out of the loop for those panes. **This does not fix
#2167**, which is about PTY claude panes and stays open; a structured pane is
simply not exposed to it. (Out of scope here, and recorded because it is the same
class: the docs now describe `CLAUDE_CODE_PROJECT_DIR_NAME` alongside
`CLAUDE_CONFIG_DIR` as a way to *pin* the project directory instead of deriving
it — [sessions](https://code.claude.com/docs/en/sessions#name-the-project-directory-yourself)
— a candidate fix for #2167's PTY half.)

### 7.4 Per harness

| harness | usage source today | under a structured driver |
|---|---|---|
| claude | transcript JSONL, statusline fallback (#2167) | **the `result` message**, in band, `estimated` basis, source `stream` |
| opencode | session DB row plus a recursive `parent_id` rollup, `reported` basis | unchanged — R3 keeps the DB reader; the HTTP API is the control path, not the meter |
| copilot | no readable token record; statusline only | unknown until R4 reads the ACP surface — not claimed here |
| gemini | statusline | PTY only |

---

## 8. The R1 → R2 slice contract

### 8.1 R1 — `feat/harness-claude-core`, an engine LEAF

**Files:** `crates/loomux-engine/src/harness/mod.rs`, `harness/claude.rs`,
`harness/transcript.rs`, plus one `pub mod harness;` line in
`crates/loomux-engine/src/lib.rs`.

**Ships:** §1's vocabulary; the stream-json decoder (NDJSON line →
`HarnessEvent`); a child-process driver owning stdin/stdout/stderr and the
child's lifetime; the VT renderer (`HarnessEvent` → bytes).

**Does NOT ship, and R1 is not done by shipping any of it:** an edit to
`src-tauri/src/orchestration/mod.rs` (**zero** — this is what makes R1 parallel
with the A4 chain); a `#[tauri::command]`; the `driver:` key or any `CLI_CAPS`
change that makes it parseable; `permissions.json`; the MCP tool; any wiring into
a spawn path; any role-template edit (so no `pre222` re-bless).

**Dependencies: none.** `serde_json` and `std` only. Nothing that pulls
`getrandom` (constraint 2) — the pane's `--session-id` UUID is minted by the
existing `RandomState` path, not by a new crate.

**Decoder rules:**

| stream line | `HarnessEvent` |
|---|---|
| `system` / `init` | `Booted { session, model, capabilities }` |
| `assistant`, text block | `Text` |
| `assistant`, `tool_use` block | `ToolCall` |
| `user`, `tool_result` block | `ToolResult` |
| `stream_event` | `Text` delta |
| `system` / `compact_boundary` | `Compacted { trigger, pre_tokens }` |
| `result` | `TurnEnded { usage, cost, stop }` |
| `system` / `api_retry` | a log line only; not a `HarnessEvent` |
| anything else | ignored as an event; the raw line goes to the pane log (§4.1) |

**Tests.** Fixtures under `src-tauri/tests/fixtures/harness/claude/*.jsonl`,
**recorded once by the human from a real CLI** and committed with a version pin
(`opencode.md`'s labelling discipline). Agents never regenerate them and never run
a real CLI (constraint 3). The driver is exercised against a **fake harness
script** that cats a fixture on a schedule. Decoder tests are table-driven; a
planted unknown message type must be ignored and logged rather than panic; the
renderer is pinned on golden VT output. Red-before-green is captured on CI, since
agents may not run cargo locally.

### 8.2 R2 — `feat/harness-claude-wire`

Waits on A4-18′ (`PaneHost::request_pane -> Box<dyn AgentPane>`) and edits
`mod.rs`'s spawn path, so it serializes against the A4 chain.

**Ships:** `driver: structured` honoured at parse and at spawn, with the
`CLI_CAPS` row (`structured_driver: Option<Harness>`) and the schema-manifest
entry (§2); the drainer's last step becoming `pane.send(turn)`; the
`permission_prompt` MCP tool (five-place registration per the `add-orch-tool`
skill) and `permissions.json` (§3); the needs-you wiring for an undecided prompt;
the per-pane event log (§4); renderer output into the ring plus the two additive
events (§5.6); `pane_kind` on `agents.json`; the usage collector's `stream` source
(§7); and a `docs/` page for the workflow key.

**The contract between the two slices:** R1 defines the types; **R2 may not
change a name or a shape in §1, §4.1 or §5.6 without amending this note in the
same PR.** That is what makes R0 worth reviewing before R1 exists.

**Behavioural-silence bar, inherited from the extraction note:** the existing
integration suite green with **zero test edits** for everything that is not a
structured pane. A group with no `driver:` key is byte-for-byte what it is today,
`default_roster_command_lines_match_legacy` and the `pre222` pins included.

---

## 9. Live-validation items for the human

Each is a fact the official docs do **not** settle. None is guessed anywhere
above; R1 depends on none of them, and R2 depends on 1 to 3.

1. **The `--permission-prompt-tool` wire contract.** The docs state that the flag
   "specif[ies] an MCP tool to handle permission prompts in non-interactive mode"
   and that an `allow` result exists
   ([cli-reference](https://code.claude.com/docs/en/cli-reference),
   [mcp](https://code.claude.com/docs/en/mcp#require-approval-for-a-specific-tool)),
   but **the tool's input JSON schema and its expected return shape are not
   documented** on cli-reference, headless, mcp, agent-sdk/permissions or
   agent-sdk/user-input. The SDK's in-process analogue returns
   `{ behavior: "allow", updatedInput }` or `{ behavior: "deny", message }`
   ([typescript SDK](https://code.claude.com/docs/en/agent-sdk/typescript)); the
   channels relay uses a third shape again — `request_id`, `tool_name`,
   `description`, `input_preview` in, `{request_id, behavior}` back
   ([channels-reference](https://code.claude.com/docs/en/channels-reference#relay-permission-prompts)).
   **Needed:** one real session, one prompt, the exact request and the accepted
   response recorded as a fixture.
2. **Does `AskUserQuestion` reach the prompt tool?** The SDK docs say it "always
   fall[s] through to the callback"
   ([agent-sdk/permissions](https://code.claude.com/docs/en/agent-sdk/permissions))
   and say nothing about the `--permission-prompt-tool` path. orrerix's plan
   denies the tool either way, so this decides only whether the denial is visible
   as a prompt or silent.
3. **Do the containment flags compose with the stream-json line?** `--agent`,
   `--settings`, `--allowedTools`, `--disallowedTools` and `--effort` are
   documented as flags with no stated mode restriction, but no page shows them
   alongside `-p --input-format stream-json --output-format stream-json`. §2.3's
   "byte-identical containment argv across both drivers" test only means something
   if the CLI honours them there.
4. **Interrupt on Windows.** The docs prescribe SIGINT before SIGTERM
   ([headless](https://code.claude.com/docs/en/headless#stop-a-run-with-sigterm)),
   which this project's Windows baseline does not have as such. Whether the
   documented `interrupt` control request over stdin is the right substitute — and
   whether this CLI build advertises `interrupt_receipt_v1` in
   `system/init`'s `capabilities` — is a live check.
5. **The auth-policy sentence — a human call, not an engineering one.** The Agent
   SDK overview states: "Unless previously approved, Anthropic does not allow
   third party developers to offer claude.ai login or rate limits for their
   products, including agents built on the Claude Agent SDK. Use the API key
   authentication methods"
   ([agent-sdk/overview](https://code.claude.com/docs/en/agent-sdk/overview)).
   orrerix would call **no SDK**: it launches the user's own `claude` binary with
   the user's own login, exactly as the PTY path does today. Whether that sits
   inside or outside that sentence is the human's read. Nothing in R1 depends on
   it; R2 ships a structured pane that does.
