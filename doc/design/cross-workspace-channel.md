# Design: cross-workspace communication channels (#271)

Status: backend implemented (W1, PR #285); the human-facing connect UI (W2) stacks on
top (both merged into this feature branch). **W3 (this revision)** adds standalone-pane
membership and a directional (sender/receiver) model — see the two sections near the end
of this document; they supersede the "standalone launcher panes" scope-out below and
revise the undirected `channel_send`/`connect_agents` contract W1 shipped.

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

- ~~**Standalone launcher panes** (not part of an orchestration group) have no MCP
  identity — `write_mcp_config` only runs for group agents.~~ **Superseded by W3** (see
  "W3: standalone panes as first-class members" below) — the human's two follow-up hard
  requirements (any pane can connect; sender/receiver direction) required lifting this
  exclusion. Kept here, struck through, so the historical "why v1 didn't do this" reasoning
  stays visible instead of silently vanishing.
- **Cross-OS-process channels.** Single-window/single-process app; nothing to bridge. A
  broker-file extension point is the natural follow-up if loomux ever goes multi-process.
- **Persistence across an app restart.** In-memory only, the `watches` (#243) precedent —
  see **Persistence** below. Unaffected by W3: a solo pane's identity is exactly as
  ephemeral as an orchestration agent's.
- **A pull-based `channel_read()` inbox.** Rejected: breaks the "visible prompt" delivery
  principle, adds polling, and the pane transcript already is the inbox.
- **Per-CLI full-membership seams for codex/gemini/opencode** (W3): their own MCP
  mechanisms are repo/user config files (`~/.codex/config.toml`, `.gemini/settings.json`,
  `opencode.json`), not a `--mcp-config`-style spawn flag — a genuine per-CLI integration,
  not a channel-feature change. Those panes ship delivery-only in W3; the seam is tracked
  as a follow-up issue (referenced in the capability matrix below).

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

    struct Channel { id: String, members: Vec<ChannelMember>, created_ms: u64, sender: String, display_number: u32 }
    struct ChannelMember { group: String, agent_id: String, name: String, role: Role, may_reply: bool }

(`sender`/`may_reply` are the W3 directional addendum below; `display_number` is the
display-number follow-up at the end of this doc.)

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
(`{kind: "connected"|"disconnected"|"closed", channel_id, display_number, members}`) so
cross-tab UI can update without polling — payload shape frozen in `src/orchestration.ts`'s
`OrchChannelEvent`, consumed by the follow-up UI PR. (`display_number` added by the
display-number follow-up at the end of this doc — W1 originally shipped without it.)

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

## UI implementation (W2)

Builds entirely on W1's frozen contract (`ChannelMember`/`OrchChannel`/`OrchChannelEvent`,
the four `orch_channel_*` commands, the `orch-channel` event) — no backend changes in this
slice, aside from d2a0a44's pre-existing review fix (`orch_channel_connect`'s return shape,
already reflected in the contract this PR builds against).

**The gesture, in code.** Two new pure, DOM-free modules (node:test, no DOM simulation —
CLAUDE.md convention):

- `src/panemenu.ts` — `buildPaneMenu(pane, pending)`: the menu SHAPE for a pane's current
  state (free / connected / planner / non-capable) crossed with the global pending-arm
  state. Arming is only ever offered on a FREE pane; an ALREADY-CONNECTED pane is still a
  valid completion target for a pending arm elsewhere — that asymmetry is how a free third
  pane joins an existing channel (multi-party), matching `connect_agents`' join rules
  without the frontend needing to special-case it.
- `src/channel.ts` — `reduceConnect(action, pending)`: the pure state-transition function
  (arm/complete/cancel/self-click) plus the per-channel color/number/chip derivation.
  Channel ids are backend-minted `chan-N` (a monotonic counter), so — unlike
  `orchbadge.ts`'s per-group palette (arbitrary ids, needs an insertion-order cache) — a
  channel's color/number is a pure function of its own id: no cache, nothing to reset
  between tests.

**Where the state actually lives.** There is at most one armed connect source live at a
time, globally, across every tab — a module-level `pending`/`pendingPane` pair in
`orchestration.ts`, mirroring how `cancelledSpawns` already lives there (DOM/backend glue
state that doesn't belong in a pure module). `reduceConnect` is the pure core;
`handlePaneMenuAction` is the thin shell that calls it, makes the resulting backend call
(`channelConnect`/`channelDisconnect`), and toasts on failure.

**`contextmenu.ts` made generic.** It was already commented "deliberately generic … so a
second caller can reuse it" but wasn't literally generic yet (hardcoded to filemenu.ts's
`MenuAction`). Changed `MenuItem`/`showContextMenu`/`buildLevel` to `MenuItem<A>`/
`showContextMenu<A>`/`buildLevel<A>`; filemenu.ts's own `MenuItem`/`MenuAction` types are
untouched and satisfy `MenuItem<MenuAction>` structurally, so `fileexplorer.ts`'s existing
call site needed no change. contextmenu.ts itself carries no test (DOM wiring, validated by
hand per CLAUDE.md), so this had no test-visible blast radius.

**Indicators.** `Pane.setConnected(info | null)` mirrors `setBadge`/`setAttention`: a
`.pane-channel` chip (solid background in the channel's color, like `.pane-badge`) before
the title, plus a `.pane.connected` outline — deliberately `outline`, not another
`box-shadow` layer, so it composes with the existing `.grouped`/`.needs-attention`
box-shadow stripes without a combinatorial CSS explosion. An armed (pending-source) pane
gets a separate pulsing dashed outline (`.connect-pending`), satisfying the "visible
pending state" requirement without a transient toast being the only cue — a toast fires
once at arm time for the immediate confirmation, but the outline persists until the
gesture resolves. All of this is header/chip chrome only — never a PTY resize (constraint
1; see `groupview.ts`'s watch-line precedent).

Cross-tab: the `orch-channel` listener (added to the existing `initOrchestration`) matches
each event's members against every pane in every grid by `orchAgentId` — the same
cross-tab match `orch-spawn-cancelled` already uses, valid because agent ids are globally
unique in the registry (orchbadge.ts's `agentSeq` comment). A minimized pane's chip mirrors
to its dock chip (`Pane.channelBadge` getter, read in `grid.ts`'s `renderDock`, exactly
paralleling `pane.attention`/`dockChipAttention`). A tab holding a connected pane gets a
small `⇄` dot on the tab strip even while hidden — implemented for free by extending
`tabcounts.ts`'s already-deterministic `TabPaneInfo`/`TabCounts` (a `connectedChannel`
field, counted into `connectedChannels`) rather than adding a second event-driven map
alongside `TabManager`'s existing `attn`; `TabManager.touch()` forces the one re-render a
channel-only mutation wouldn't otherwise trigger (the existing 4s status poll only
re-renders on agent/cost/paused deltas).

**Rehydration.** `orch-channel` only fires on live mutations, so a pane that reopens (a
respawn/rejoin) while its channel is still live in the registry needs a separate read:
`openAgentPane` calls `channelForPane(group, agent)` right after a successful `bind_agent`
and sets the chip if one comes back. Best-effort (a failed read just leaves the chip off
until the next mutation) — not worth surfacing to the human.

**Easy close, unambiguously.** Disconnect is reachable two ways — the pane menu's
`Disconnect` item, and a single click on the chip itself (`Pane.onDisconnectChannel`) —
both routed through the same `handlePaneMenuAction({kind: "disconnect", …})` path, so
there is exactly one disconnect behavior, not two to keep in sync. There is no separate
"leave vs. close" choice for the human to make: a channel is structurally never left with
one member (the backend tears it down below 2), so disconnecting IS closing once only two
remain, and the audit sentence (`auditsummary.ts`) says which happened.

**Tests.** `test/panemenu.test.ts` and `test/channel.test.ts` pin the menu shape across
every pane/pending state and the reducer's four transitions; `test/auditsummary.test.ts`
gained three new exact-equality pins (`channel-connect`/`channel-message`/
`channel-disconnect`) mutation-verified by hand (temporarily deleting each `summarize()`
arm and confirming its pin reddens against the raw-JSON fallback, not a silent pass — the
same discipline the existing watch-* pins established, PR #252). DOM wiring (the
`contextmenu` listener on the pane header, `setConnected`/`setPendingConnect`, the dock/tab
mirrors) is validated by hand — see the PR description's checklist.

## W3: standalone panes as first-class members

*Folds in the human's follow-up hard requirement: any agent pane — orchestrator, worker,
reviewer, or a plain standalone launcher pane — can be a channel member, not just
orchestration-group roles.*

**The core consequence.** Delivery is hard-keyed to an `AgentEntry` with a `pty_id`
(`deliver_prompt: self.agent(id)?; a.pty_id.ok_or("agent has no terminal yet")?`), so
*every* channel member — full or delivery-only — needs one. The only thing that varies is
whether it *also* holds a **token**: a token is what lets a pane call `channel_send`; no
token means receive-only. The channel core (`Channel`/`ChannelMember`/`connect_agents`/
`channel_send`) does not move — it stays exactly the graph a human builds and an agent
reads/broadcasts against.

**Identity — a reserved pseudo-group + `Role::Solo`.** A backend-minted, fixed constant
`SOLO_GROUP = "__solo__"` (never produced by `group_id_for_repo`, which always emits
`{slug}-{8hex}` — constraint 6's path-segment safety holds by provenance), registered
lazily (`OrchRegistry::ensure_solo_group`) the first time a solo pane is created, with repo
label `"(standalone)"`. Each standalone pane is `AgentEntry{id: "solo-N", group:
"__solo__", role: Role::Solo, ...}`. **`Role::Solo`'s entire MCP surface is `channel_send` +
`channel_status`, full stop** — `mcp.rs::tool_defs` early-returns those two for it before
touching any other tier, and `call_tool` gates it a second time at the very top of dispatch
(`caller.role == Role::Solo && !matches!(name, "channel_send" | "channel_status")` →
denied) so a future tool addition can never silently leak onto a solo token. This is the
direct, one-line answer to "a solo token must carry zero group-scoped power": it is
neither listed nor dispatchable for anything else (pinned in
`solo_role_tool_surface_is_exactly_channel_send_and_channel_status` and
`solo_role_cannot_dispatch_any_group_scoped_tool`, `tests/orchestration.rs`).

*Rejected alternatives:* pane/pty-id-keyed membership (a second key scheme — forces
`ChannelMember` into an enum, branches every channel method, rewrites the just-approved
#285 core for no gain); reusing `Role::Worker` (its tool surface — `report`,
`list_agents`, `get_state`, `list_tasks` — all imply a group with an orchestrator/board/
state a solo pane doesn't have; a dedicated two-tool role is the honest scoping).

**MCP injection at spawn — per-CLI seam.** loomux cannot inject an MCP server into an
already-*running* CLI, but it fully controls a *newly launching* pane's command line.
Human-only Tauri commands (constraint 5), mirroring the orchestration group's spawn round
trip but launcher-initiated with no orchestrator involved:

- **`orch_solo_prepare(cli, cwd, name) -> {agent_id, mcp_args, delivery_only}`** —
  lazily ensures `__solo__`, mints `solo-N`. For claude/copilot (`SUPPORTED_CLIS`, the only
  CLIs with a config seam today) it writes the config via the **existing**
  `write_mcp_config(__solo__, solo-N, token, cli)` and returns the exact per-CLI flag
  string built in Rust (claude: `--mcp-config "<cfg>" --strict-mcp-config --allowedTools
  mcp__loomux`; copilot: `--additional-mcp-config "@<cfg>" --allow-tool loomux`) — one
  place per-CLI knowledge lives, next to `write_mcp_config`. For any other CLI it mints NO
  token and returns `delivery_only: true, mcp_args: ""` — the `AgentEntry` still exists.
- **`orch_solo_bind(agent_id, pty_id)`** — binds the pty (mirrors `bind_agent`, but direct
  bookkeeping rather than the async rendezvous `spawn_agent_ex` blocks on, since
  `solo_prepare` already returned synchronously): sets `pty_id`, `status: Running`, and
  registers `by_pty[pty_id] = agent_id` so the **existing** pty-exit path (`by_pty ->
  mark_dead -> cleanup_agent_channel`) tears the member down on pane close with **no new
  teardown code**.

The launcher (`src/launcher.ts`) calls `orch_solo_prepare` eagerly for every newly-spawning
agent pane, but **only when `cli` is claude or copilot** — every other CLI stays lazy,
minting nothing at launch time, so a codex/gemini/opencode/custom pane incurs no
`__solo__` identity nobody asked for. `src/main.ts` binds the pty right after each pane
opens.

**Channel tools toggle (PR #289 review round 2, N1).** Even scoped to claude/copilot,
unconditional eager minting is a broader live-token surface than "channels" strictly needs
— a token confers no group-scoped power (independently re-verified in review), but it's
still a real MCP identity sitting in `by_token` for a pane that may never be connected.
Two shapes were on the table: prepare **lazily** (mint at connect/bind time instead of
launch time) or gate the eager mint **behind an explicit setting**. Lazy-at-connect isn't
actually viable for full membership: claude/copilot's MCP flags must be on the command
line the process boots with, and by the time a human right-clicks Connect on an
already-running pane it's too late to hand it new flags (the "you cannot inject an MCP
server into an already-running CLI" constraint that motivates the whole adopt-on-connect
design). So the launcher gained a persisted **"Channel tools"** checkbox (`src/agents.ts`'s
`getChannelTools`/`setChannelTools`, the identical default-ON/explicit-"0"-off shape as the
existing autopilot toggle), shown only for claude/copilot, checked once at launch — no
prompt at launch (a persisted default) and no prompt mid-connect either way. Default ON:
the addendum's stated contract is "claude/copilot = full membership at spawn," and turning
the toggle off doesn't lose the pane's connectability, only its head start — an
unminted claude/copilot pane simply falls through to the same delivery-only
adopt-on-connect path every other CLI already uses, indistinguishable in the UI from a
codex/gemini pane until upgraded.

**Already-running / pre-feature panes — adopt-on-connect.** loomux owns the pty for any
live pane today, so inbound delivery works regardless of when the pane was launched;
refusing to connect a pane the human is looking at, when delivery would work fine, is the
worse UX than the alternative. **`orch_solo_adopt(pty_id, name, cwd) -> {agent_id}`**
registers a delivery-only `AgentEntry` (no token) and binds the pty — idempotent by pty, so
re-adopting the same pane returns its existing id rather than minting a second one.
`src/orchestration.ts`'s `showPaneConnectMenu` calls this on the first Connect gesture
against any **agent** pane (`pane.isAgentPane`, never a shell/content pane — those stay
`NOT_CAPABLE_REASON`) that has no channel identity yet.

**Delivery-only asymmetric membership is coherent, and represented honestly everywhere.**
An adopted or non-seam-CLI member needs an `AgentEntry`+pty (to be a `deliver_prompt`
target) but no token:

- `channel_status`'s peers and `channel_members_json` (mod.rs) carry both `can_send`
  (momentary: has a token AND currently holds the reply credit/is the sender) and
  `delivery_only` (structural: has a token at all) — two different facts a receive-only
  chip needs told apart from a plain receiver simply out of credit right now.
  `OrchRegistry::agent_has_token` is the single source of truth both read.
  - The member itself sees NO MCP tools at all (its token is empty, so it never even
    resolves a `Caller` — `resolve_token` returns `None` for an empty/absent token,
    pinned directly), so it never sees a `channel_send` it structurally can't use.
  - `src/pane.ts`'s channel chip renders a distinct dashed `.receive-only` CSS variant for
    `deliveryOnly`, separate from the solid chip a normal receiver gets between messages.

**Per-CLI capability matrix (what ships in W3):**

| CLI | membership | how |
|---|---|---|
| claude, copilot | full (token, `channel_send`) | `orch_solo_prepare` injects MCP flags at spawn |
| codex, gemini, opencode | delivery-only | no spawn-flag seam today — tracked in [#288](https://github.com/willem445/loomux/issues/288) |
| custom launcher command | delivery-only, permanently | no CLI identity to target a config format at |
| any pane adopted via Connect (`orch_solo_adopt`) | delivery-only | never gets a token, regardless of its actual CLI |

## W3: directional (sender/receiver) channel model

*Folds in the human's second follow-up hard requirement: two agents in a channel must not
be able to talk over each other. This is the one place W3 revises #285's already-approved
`channel_send`/`connect_agents` contract — flagged, not silently retro-edited into #285.*

Every channel is now **directed**: exactly one **sender** (the client that initiates) and
one-or-more **receivers** — a star topology, never optional.

**Data model (smallest sound extension, additive fields on #285's structs).**
`Channel.sender: String` (the sender's `agent_id` — the single source of truth for "who
drives", and the one-sender-per-channel invariant lives here) and `ChannelMember.may_reply:
bool` (a per-receiver reply credit, ignored for the sender — the request/response gate).

**Where direction is designated: at gesture COMPLETION, with an explicit arrow — not
implicit gesture order.** By completion both endpoints are known, so the choice is
meaningful and the human confirms who drives before committing, killing the "connected
them backwards" error class an implicit "armed = sender" rule would invite. The completion
menu (`src/panemenu.ts::buildPaneMenu`) offers, for a FRESH two-party connect, two explicit
items: `Connect: {armed} → sends to → {this}` and `Connect: {this} → sends to → {armed}` —
either disabled with a reason if that side has no token (a delivery-only pane can never be
the sender). For a JOIN onto a channel that already has a sender, only the compatible item
shows: `Join as receiver — driven by {sender}`. `orch_channel_connect` gained a
`sender_agent` parameter carrying this choice through to `OrchRegistry::connect_agents` —
whose meaning depends on which case applies (review round 2, B1: conflating the two broke
every join that completes on a receiver rather than the sender). For a **fresh mint**,
`sender_agent` **designates** the new sender and must be one of the two named panes (and
must hold a token). For a **join**, the channel's sender already exists;
`sender_agent` only **confirms** who that is, and is deliberately allowed to be neither of
the two panes this call names — the completion gesture can land on any existing member
(the sender itself, or a plain receiver), and the true sender is often a third pane
entirely (e.g. a newcomer completing onto a receiver in a 4+-member star). Requiring the
confirmation to equal `Channel.sender` — never requiring it to be one of the two call
arguments — is what makes B4's "a join can never reassign the sender" invariant hold while
still letting the human complete the gesture on any member, rejecting a mismatched
confirmation the same way `set_sender` rejects one
(`"this channel already has a sender — swap it first"`).

**What the roles mean at the tool layer: request/response.** A receiver that could never
answer would be useless; a receiver that could initiate re-creates the talk-over problem
this whole addendum exists to close.

- **Sender** — `channel_send` any time; **broadcasts** to every receiver, and each
  delivery sets that receiver's `may_reply = true`.
- **Receiver** — `channel_send` is **reply-only**: permitted only while `may_reply` is
  true (else `"you can only reply after the sender messages you"`), delivers **only to the
  sender**, and consumes the credit. A receiver never reaches another receiver — B4's star
  topology, enforced structurally in `OrchRegistry::channel_send` (mod.rs), not left to
  prompt etiquette.

*Rejected alternative:* "a receiver may always reply, no credit" — one field simpler, but a
chatty receiver could then interrupt the sender unsolicited; the human explicitly wanted
request/response, so the credit stays. Advisory-etiquette-only was rejected outright — a
guardrail belongs in code, not a prompt, per this codebase's norms.

**Mutable direction — human-only, no reconnect needed.** `orch_channel_set_sender(channel_id,
new_sender_agent)` reassigns `Channel.sender` (validated: member + token), **clears every
member's `may_reply`** (a swap invalidates in-flight "you may reply" state — the new sender
starts clean), notifies both roles' panes, and audits `channel-direction`
`{channel_id, from_sender, to_sender, by:"human"}`. Menu affordance: "Make this pane the
sender" on a token-holding receiver (`src/panemenu.ts`), never on the sender itself, never
on a delivery-only pane.

**Losing the sender collapses the whole channel — additive to #285's below-2-members
teardown.** A star with no hub leaves receivers that can never initiate and can never reach
each other; `OrchRegistry::disconnect_agent` now closes the channel when
`remaining.len() < 2 OR the disconnecting agent WAS the sender`, even with two-or-more
receivers left. No automatic promotion — that would bypass the human-only swap rule; a
human re-designates and (if wanted) reconnects.

**Honest representation everywhere.** `channel_status`, `channel_list`, `channel_for_pane`,
and the `orch-channel` event all carry `sender` plus, per member, `direction`
(`"sender"|"receiver"`) and `can_send` — computed by the one shared
`OrchRegistry::channel_members_json` helper so the four surfaces can never drift apart.
`channel_status` additionally reports the CALLER's own current `can_send` (sender: always
true; receiver: true only while holding the credit) so an agent can check before calling
`channel_send` and hitting the credit error. The pane header chip
(`src/pane.ts::setConnected`) renders a direction arrow (▲ sender / ▼ receiver) alongside
the existing channel color/number.

**Composition with standalone panes — one rule.** Sender requires a token, full stop:
**sender** (must hold a token: an orchestration agent, or a claude/copilot solo pane) may
broadcast any time; **full receiver** (token) may reply under the credit; **delivery-only
receiver** (no token — an adopted pane, or a codex/gemini/opencode/custom solo pane)
receives only, never replies — exactly the "a delivery-only standalone member is naturally
a receiver" shape the human asked for. Enforced identically at `connect_agents` (mint/join
time) and `set_sender` (swap time) via the same `token.is_empty()` check, each pinned by a
dedicated red-before-green test (`delivery_only_solo_pane_receives_but_can_never_send_or_
become_sender`, `set_sender_rejects_a_delivery_only_candidate_and_leaves_the_sender_
unchanged`).

**Revision to #285, stated plainly.** `channel_send` changed from unconditional broadcast
to role-aware (sender broadcasts, receiver replies-only-to-sender); `connect_agents` gained
a required `sender_agent` parameter. W1's `tests/orchestration.rs` channel tests are
updated **in place** to the new signature/semantics (every `connect_agents` call site now
names a sender); this is a deliberate, flagged contract revision on a stacked follow-up PR,
not a retro-edit of #285's merged commit.

## Follow-up: `display_number` (PR #285 live-testing feedback)

**The bug.** The pane chip's number/color were a pure function of `id` (`chan-N`,
`channel_seq`'s monotonic `AtomicU32`). `channel_seq` correctly never reuses a value — an
audit record for `chan-1` must never become ambiguous with a later, unrelated `chan-1` — but
that means `id`'s numeric suffix keeps climbing even as channels close. A human live-testing
#285 connected a pair (chip `⇄1`), disconnected it, then connected a fresh pair: the new
channel was `chan-2`, so the ONLY active channel read `⇄2` — the chip no longer represented
what was actually connected, just how many channels had ever existed.

**The fix.** `Channel` gains a second, independent field: `display_number: u32`, assigned
once at mint time by `OrchRegistry::next_display_number` — the lowest positive integer NOT
currently used by any other live channel. `chan-1` closing frees `"1"` for the very next
mint; with actives `{1, 3}`, the next mint fills the gap at `2`, not `4`. `id` stays exactly
as before (monotonic, immutable, the audit key); `display_number` is immutable for a given
channel's lifetime too, but reassignable — as a value — the moment a DIFFERENT channel
closes and something mints into the freed slot.

**Exposed everywhere the frontend learns about a channel**, so the four surfaces can't
drift apart the same way W3's `sender`/`direction` fields don't (`channel_members_json`'s
discipline, applied here as a sibling field alongside it rather than inside it, since
`display_number` is per-channel, not per-member): `connect_agents`'s return, `channel_list`,
`channel_for_pane`, `channel_status`, `set_sender`'s return, and every `orch-channel` event
(`connected`/`disconnected`/`closed`/`updated` — captured into a local before
`disconnect_agent` tears the `Channel` down, so the closing event still carries it).

**Frontend.** `channel.ts`'s `channelColor`/`channelChipLabel` now take the display number
directly instead of parsing it out of the id; `channelBadge` takes `channelId` (still shown
nowhere but kept for correlation) and `displayNumber` as separate parameters — never
re-derives one from the other. Restart hydration (`channelForPane`) round-trips the
backend's `display_number` rather than recomputing anything client-side, since it is state,
not a pure function of `id` any more.

**Audit sentences are unaffected.** `auditsummary.ts`'s `channel-connect`/
`channel-disconnect`/`channel-direction` arms render the immutable `channel_id` (`chan-N`)
they always have — that id is exactly what makes two disconnect records for the "same
numbered" channel unambiguous, which is the whole reason `id` and `display_number` are two
fields instead of one.
