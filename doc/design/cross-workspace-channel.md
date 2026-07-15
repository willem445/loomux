# Design: cross-workspace communication channels (#271)

Status: backend implemented (this PR); the human-facing connect UI (pane context menu,
header chip, cross-tab indicators) stacks on top in a follow-up PR against this branch.

## Problem

An orchestration group is isolated per repo/tab by design — that isolation is what keeps
one group's agents from stepping on another's context. But the human sometimes has two
related repos open (a library and its consumer, a backend and its frontend) and wants a
narrow, explicit way to let one agent tell another "the API changed" or "I'm blocked on
your PR" without relaying the message through themselves.

## The one architectural fact that shapes everything

Loomux is **one OS window, one process, one `OrchRegistry`, one MCP server** —
`mcp::serve()` binds a single `127.0.0.1:0` port; every group's agents hit the same `/mcp`
and are told apart only by their `X-Loomux-Agent` token (`Caller{agent_id, group, role}`).
A "workspace" is a **project tab**, and each tab owns at most one orchestration group. So
"cross-workspace" means **cross-group inside this one process**, not cross-OS-process. That
rules out a shared-broker-file/poller design (the obvious "two processes talking" shape):
there is no second process to bridge to. A channel is simply shared in-memory state in
`OrchRegistry` — a `channels`/`agent_channel` map beside `watches` (#243) — and a message is
delivered through the **same** `deliver_prompt(..., Delivery::MidSession)` visible-prompt
path every other agent-to-pane delivery already uses (`report`, `send_prompt`, a fired
watch notice). No new transport, no polling loop, no filesystem watcher.

## Scope of this PR (W1 — backend)

In: the `channels` registry, human-only connect/disconnect/list Tauri commands, the two
agent-facing MCP tools (`channel_send`, `channel_status`), delivery + sanitization + audit,
role templates, and typed frontend command wrappers (`src/orchestration.ts`) so the UI slice
has a frozen contract to build against.

Out (follow-up PR, stacked on this branch): the pane context-menu connect/disconnect
gesture, the header chip + cross-tab indicators, and the `orch-channel` event listener that
wires them up. The wrapper functions and the event payload shape are already defined here
(`ChannelMember`, `OrchChannel`, `OrchChannelEvent`) so that PR needs no backend changes.

Out of v1 entirely, each with a reason (unchanged from the issue's plan):

- **Standalone launcher panes** (not part of an orchestration group) have no MCP identity —
  `write_mcp_config` only runs for group agents. v1 covers every orchestration-group role,
  which is the issue's actual "two related repos" use case.
- **Cross-OS-process channels.** Single-window/single-process app; nothing to bridge. A
  broker-file extension point is the natural follow-up if loomux ever goes multi-process.
- **Persistence across an app restart.** In-memory only, the `watches` (#243) precedent —
  see **Persistence** below.
- **A pull-based `channel_read()` inbox.** Rejected: breaks the "visible prompt" delivery
  principle, adds polling, and the pane transcript already is the inbox.

## The trust boundary (constraint 6) — the crux of this feature

`channel_send` takes **only `text`**. The caller's peers are resolved from the membership
graph a human built via `connect_agents`/`disconnect_agent` — both Tauri commands, reached
only from the trusted webview (constraint 5), never MCP tools. There is no `channel_open` /
`connect` / `close` tool at all: connection is a human capability, full stop.

- An agent can reach exactly the panes a human connected it to and nothing else — the
  membership graph **is** the capability. No agent-supplied `agent_id`/`group_id` ever
  reaches a cross-group lookup; `channel_send`'s only argument is the message body.
- Every crossing text is scrubbed with `notify::sanitize_gh_text` (#243) before it enters a
  peer's pane: control characters (including newlines) are stripped so an embedded newline
  can't forge a second `[loomux] …`-prefixed line, and `[`/`]` are mapped to `(`/`)` so the
  literal token `[loomux]` can't survive even mid-line. Same sanitizer every other
  crossing-text boundary in this codebase uses — no new mechanism.
- The identity line prefixed to every delivered message (`[loomux] channel chan-3 - w-2
  (worker, C:/repo): <text>`) is built by loomux from the **caller's own backend-resolved**
  identity (`OrchRegistry::channel_member_label`) — name, role, and repo — never from
  agent-supplied text. A peer can neutralize/garble its own message with hostile input, but
  it can never forge who sent it (see `channel_message_text`, a pure function pinned
  directly in `tests/orchestration.rs`).
- Channel ids are backend-minted (`chan-N`, an `AtomicU32` sequence — no `getrandom` crate,
  CLAUDE.md constraint 2), never caller-supplied, never a path segment.
- Membership mutation is always human-authorized, per-edge, and revocable (disconnect).

## Membership model

- **A pane belongs to at most ONE channel at a time.** The feature exists to give bounded,
  explicit sharing between otherwise-isolated workspaces; a pane in two channels would
  silently bridge them, re-introducing the cross-talk the isolation exists to prevent. It
  also keeps `channel_send` argument-free (no session id to pick) and the eventual UI chip
  unambiguous (one channel, one color). Enforced by `agent_channel: HashMap<agent_id,
  chan_id>` — a pane can appear in that map at most once, structurally.
- **Multiple channels concurrently: yes.** The only rule is one pane, one channel.
- **A pane talking to two peers is a 3-member channel, not a pane in two channels.**
  `connect_agents(from, to)`:
  - both free → mints a new channel with both as members.
  - one free, one already connected → the free pane **joins** the connected pane's channel
    (multi-party).
  - both already in the **same** channel → idempotent no-op.
  - both already in **different** channels → rejected (`"already connected — to different
    channels"`) — joining would silently bridge two otherwise-isolated sessions.
- **Planners are never members.** A planner's pane closes the instant it reports `done`
  (#203); a channel member that can vanish mid-session mid-conversation is a liability, and
  it mirrors the #243 notification-tools exclusion. Enforced in `connect_agents` (both
  sides) and `require_not_planner` (mcp.rs) for the two MCP tools — the same double-gate
  pattern (cosmetic listing filter + real dispatch check) #243 established.
- **Death tears a channel down like a disconnect.** `mark_dead` (idle-kill, `kill_agent`, a
  crash, planner auto-close — all four funnel through it) calls `cleanup_agent_channel`,
  which reuses `disconnect_agent`'s below-2-members teardown so a channel down to one live
  member closes exactly as it would from a human gesture, and the stranded peer is notified.

## Data model

`OrchRegistry` gains, beside `watches`:

    channels: Mutex<HashMap<String, Channel>>       // chan_id -> channel
    agent_channel: Mutex<HashMap<String, String>>    // agent_id -> chan_id (the invariant)
    channel_seq: AtomicU32                            // chan-1, chan-2, ...

    struct Channel { id: String, members: Vec<ChannelMember>, created_ms: u64 }
    struct ChannelMember { group: String, agent_id: String, name: String, role: Role }

`name`/`role` are cached on the member at connect/join time (not re-looked-up per read) so
`channel_status`/notices still work sensibly if a member's agent entry later changes.

## MCP tools (agent-facing) — mcp.rs, listed beside the #243 notification tools

Both denied to a planner (`require_not_planner`, the exact function #243 added):

- **`channel_send(text)`** — errors if the caller isn't connected. Otherwise, for every
  other member: sanitize `text`, format `channel_message_text(chan_id, sender_label,
  sanitized)`, deliver via `deliver_prompt(peer, ..., MidSession)` (best-effort — a headless
  peer or a torn-down pane never blocks the sender), and audit `channel-message` in the
  peer's group (and the sender's own group, if different). Returns `"sent to N peer(s) in
  chan-N"`.
- **`channel_status()`** — read-only: `{connected, channel_id, peers: [{agent_id, role,
  name, repo}]}`.

## Tauri commands (human-only) — mod.rs, registered in lib.rs

`orch_channel_connect(from_group, from_agent, to_group, to_agent)`,
`orch_channel_disconnect(group, agent)`, `orch_channel_list()`,
`orch_channel_for_pane(group, agent)`. Connect/disconnect emit an `orch-channel` event
(`{kind: "connected"|"disconnected"|"closed", channel_id, members}`) so cross-tab UI can
update without polling — payload shape frozen in `src/orchestration.ts`'s
`OrchChannelEvent`, consumed by the follow-up UI PR.

## Audit records

Reuse `self.audit(group, actor, action, detail)`, written to **both** endpoints' group logs
where they differ:

- `channel-connect` — `{channel_id, members: [{group, agent_id, name, role}]}`
- `channel-message` — `{channel_id, from, to, text}` (text already sanitized) — same shape
  as the existing `prompt` audit record, so the Alt+A viewer renders it with no changes.
- `channel-disconnect` — `{channel_id, agent, remaining}`

Human-sentence rendering for the audit/summary viewer is out of scope for this PR (the UI
slice's job, per the issue's worker split) — the field shapes above are the frozen contract.

## Persistence: in-memory only, deliberately

Same rationale as #243's watches: a channel is a live, in-session connection, not durable
state the way `state.json`/the task board/the PR itself are. Persisting membership across a
restart would mean rebinding it across a group-resume where agent ids are re-minted per
run — real complexity for a feature the issue itself frames as brief, explicit sharing. After
a restart the human re-connects; `channel_status()` on session start tells an agent whether
it's still connected to anything (mirroring the notification backend's `list_notifications()`
re-sync convention, now in the orchestrator/worker/reviewer templates).

## Test strategy

`src-tauri/tests/orchestration.rs`, driving real `mcp::dispatch()` for the two MCP tools
(exercising authz for real, exactly like `register_notify`) and the registry methods
directly for connect/disconnect (Tauri-command-backed, exactly like `pause_group`/
`mark_dead` elsewhere in that file):

- cross-group connect + `channel_send` delivery, with a hostile payload (embedded newline +
  literal `[loomux]` marker + raw ESC byte) pinning that the delivered text is sanitized and
  the sender line cannot be forged.
- the one-channel-per-pane invariant: join vs. reject-different-channels vs. idempotent
  same-channel.
- planner denial, both at `connect_agents` and at the two MCP tools (listing + dispatch).
- no MCP tool reaches connect/disconnect/open/close/join under any name.
- disconnect stops delivery and strands (and notifies) the remaining peer.
- a 3-member channel fans a `channel_send` out to both other members.
- two concurrent channels never cross.
- `channel-connect`/`channel-message`/`channel-disconnect` audit records, in both groups.
- `mark_dead` tears a channel down and notifies the survivor.

`channel_message_text` (the sender-line formatter) is a pure function, pinned directly —
mirrors how `notify.rs`'s notice-text functions are unit tested. The sanitization pin was
mutation-verified by hand (temporarily bypassing `sanitize_gh_text` in `channel_send` and
confirming the hostile-payload test fails for the expected reason) under an isolated
`CARGO_TARGET_DIR`, so it never shares — or corrupts — the shared incremental build other
concurrent workers in this repo's worktrees use.

## Known interactions (stated, not fixed here)

- **The watchdog does not know about channels**, exactly as it doesn't know about live
  notification watches (#243's own stated limitation). A worker idle because it's waiting on
  a channel peer will still trip the stall notice after `watchdog_stall_minutes`. Acceptable
  for v1; teaching the watchdog about channels is a follow-up.
- **Delivery reuses `deliver_prompt` as-is**, so it inherits that path's existing
  weaknesses/guarantees unchanged: the per-pty serialized delivery lock, the pause
  suppression, the #111 human-typing hold, and the #112 false-confirm caveat (a fired message
  landing unsubmitted in a peer's input box can still be recorded as delivered). No new
  delivery semantics are introduced by this feature.
- **Security**: no new execution capability (no subprocess at all, unlike #243's `gh`); no
  `group_id`-as-path-segment exposure (channel ids are backend-minted and never used as a
  path); the membership graph is the sole capability boundary, and it can only be edited from
  the trusted webview.
