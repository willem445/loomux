# Remote engine: the wire protocol and its security model

Issue #888, slice B1 of plan-463. **Status: PROPOSED — this note is a gate.**
Nothing server-side (tracks C, D and E of that plan) starts until the human has
answered §1 and approved this document.

It ships *before* any listener code exists, deliberately. The protocol is a
public wire contract, and — more to the point — the security argument here **is**
the design rather than a hardening pass bolted on afterwards. A listener written
first and secured second is a listener secured under schedule pressure, in the
same PR as the thing that made it urgent.

Reading order: **§1 is the only part that needs an answer today.** §2 is the
premise, and everything from §3 down follows from it.

Prior art this builds on rather than restates: the architecture proposal and the
build plan on #888 itself, `doc/design/groupid-and-path-roots.md` (#904, layer 2
of this model, landed), `doc/design/engine-transport.md` (#905, the client-side
cut line, landed), `doc/design/pty-output-coalescing.md` (#712/#714),
`doc/design/performance.md` (the invariants this transport must not quietly
escape), `doc/design/lock-resources.md` (#858) and `doc/design/ssh-panes.md`
(#887 — the *other* remote seam, and why it is not this one).

---

## 1. Decisions this note needs from the human

Nine decisions. Each is one the design cannot make for itself: it either sizes a
slice, picks a dependency, or sets a policy that is the human's to set. The
recommendation column is what this note would pick absent an answer, and the
"blocks" column names what stays parked until the answer lands.

They are numbered **H1–H9** — "H" for human — precisely so they never read as
plan-463's track-D slice ids, which are also called D1 and D2. Anywhere below
that a track-D slice is meant, it is written out as "track-D slice D1".

| # | Decision | Options | Recommendation | Blocks |
|---|---|---|---|---|
| **H1** | **Auth mechanism** — where user identity comes from | (a) built-in store: loomux mints users, hashes passwords, issues connection sessions; (b) delegated identity: an identity-aware proxy (OIDC / SSO / Tailscale / Authelia) authenticates and loomux consumes a verified identity, mapping it to roles; (c) VPN + mTLS client certificates | **(b)** if the team already runs any IdP or identity proxy — loomux then stores **no credentials at all**, and SSO/MFA/offboarding are someone else's solved problem. **(a)** only if there is no IdP; it costs C1 roughly one to two extra weeks and gives loomux a permanent password-storage surface. §6.1 defines an `Identity` seam so this choice does not rewrite C1 either way | **C1** (server skeleton + auth) — cannot start |
| **H2** | **TLS termination** | reverse proxy vs native TLS in the daemon | **Reverse proxy.** Battle-tested, zero TLS code and zero cert-rotation logic in loomux, and it is the same box that terminates H1(b). Native TLS is a v2 answer for a deployment with no proxy | C1 shape, E1 docs |
| **H3** | **Remote filesystem exposure** (proposal §11 Q3) | server-declared roots only for `ft_*`/`fm_*` (registered repos, worktrees, group dirs) vs full server-FS browsing for an owner-tier caller | **Server-declared roots only.** An owner-tier escape hatch can be added later behind an explicit config opt-in; it cannot be *removed* later | #925 scope, C2 roster |
| **H4** | **Reattach scrollback contract** (Q4) | live screen + 256 KiB ring tail vs persisted scrollback on disk | **Live screen + tail** for v1. Persisted scrollback changes the server's storage design (retention, disk budget, per-pane files) and is a feature in its own right | C5 |
| **H5** | **One container per workspace session** | v1-blocking vs fast-follow after E1 (docker-ready packaging) | **Fast-follow.** Containers are defence in depth *over* §7's process-level validation, never a replacement for it (T8), so gating the first end-to-end on E2 buys no security and costs months | E2 scheduling |
| **H6** | **Protocol sign-off** | confirm one authenticated WebSocket per client (§3) | **Confirm now**, so this note describes one protocol instead of three | C2, track-D slice D1 fixtures |
| **H7** | **Hosting facts** (not guessable) | who administers the server; where it sits (cloud / on-prem / office LAN); expected concurrent users; the Pi testbed network's subnet, addressing and discovery story | — | E1 docs, §8 |
| **H8** | **What a "session" is, plus its visibility default and lifecycle** | first: is a **workspace session** one orchestration group + its checkout, or something spanning several groups? then: is a new one **team** or **private** by default; may private be promoted to team; may team be demoted to private; who may do either? | **One group + its checkout** (§6.0 — it reuses the boundary that already carries membership, audit, MCP scoping and path scoping instead of adding a fourth). **Private by default, promote-only, the creator promotes.** Demotion cannot un-see what teammates already saw, so it is a lie unless it also ends the workspace session | §6.0, §6.4, C1 data model |
| **H9** | **Whose credentials do server-side agents act as?** — `gh` auth, agent-CLI subscriptions, push rights | one shared service account for the daemon vs per-user credentials injected per session | **Decide before C3.** This is not a detail: today an agent pushes as *the human sitting at the machine*, and on a shared server every agent's commit, push and PR is attributable to whatever the daemon holds. A shared service account is simplest and makes the audit log the only per-user attribution there is; per-user credentials preserve attribution but need a secret store loomux does not have | C3, E2 |

**#857 (repo split / licensing) stays HELD.** This design takes no dependency on
it and blocks none of its outcomes; if the engine crate boundary later becomes a
licensing boundary, `loomux-server` sits on the licensed side. Flagged, not
depended on.

Decisions already made by the human on #888, carried here so the note is
self-contained: **Linux-only daemon** (desktop client stays cross-platform);
**multi-user connection-session auth from day one** (a team connects, dispatches and
observes concurrently — the single-operator pairing-token envelope is off the
table); **desktop client first, browser client eventually, same wire**;
**detach/reattach on the fly is a hard acceptance criterion**, not crash
recovery; **docker-ready host** with eventual per-workspace-session isolation and
**Pi-testbed egress**; **team vs private workspace sessions** as a first-class
authorization dimension. (Those last two are the human's own words; §6.0 pins
down which of the three meanings of "session" they carry.)

---

## 2. The premise that breaks

Every trust derivation in loomux today reduces to **process locality**. CLAUDE.md
constraint 6 said orchestration commands may trust `group_id` as a path segment
"because the webview is trusted", and `OrchRegistry::group_dir` was
`self.root.join(group)` with no validation. Read carefully, that never claimed
the *id* was safe — it claimed the *caller* was, and the only thing that could
invoke a `#[tauri::command]` was our own webview, in this process.

That is a fact about the transport. Nobody issued it, nothing checks it, and it
cannot be revoked. Reproduce the same command surface over a socket and it
evaporates: every peer that can open a connection becomes "the webview".

And the surface is worse than one identifier. Today's 141 commands
(`src-tauri/src/command_manifest.rs`, the ACL manifest's single source of truth)
include, by design:

- `spawn_pty` — executes an arbitrary command line;
- `ft_read_file` / `ft_write_file` / `ft_replace` — read and write any file the
  process can reach, both directions, under a caller-supplied `root`;
- `fm_open` / `fm_open_with` / `fm_reveal` — hand a path to the OS shell;
- `open_in_editor` — spawns a client-named executable;
- `orch_grant_merge` / `orch_grant_release` / `orch_set_dangerous_mode` — mint
  the grant files the `gh` shim honours. A merge grant is a per-PR, single-use,
  expiring file under the group's `merge_grants` dir, written with actor
  `"human"`; that file **is** the enforcement behind constraint 7. Anything that
  can write it can merge, whatever any instruction document says.

A naive "mirror the IPC over a socket" port is therefore not a feature with a
security caveat. It is an unauthenticated remote-code-execution service that
happens to also show terminals. Everything in this note exists to make sure the
port is never naive.

**Three independent layers, all required, none sufficient alone:**

1. **Authenticate the caller** before anything dispatches (§6, §7 T1).
2. **Validate every caller-supplied identifier server-side**, independent of any
   client trust (§7 T2). #904 did this for `group_id` — one validating
   constructor, deserialization through the same gate, one path-assembly point.
   #925 is the same treatment for what remains, and this note upgrades it from
   cleanup to a **merge blocker for any listener slice**.
3. **Keep caller classes structurally separate** (§6.3, §7 T3). Humans and agents
   are distinguished today by *transport* — trusted IPC versus MCP tokens. That
   separation must survive as two listeners with two credential namespaces, or
   agent calls launder into human authority.

Containers (§8) are a fourth layer over these three. They are not a substitute
for any of them.

---

## 3. Transport: one authenticated WebSocket per client

**Decision (pending H6): one authenticated WebSocket connection per client**,
carrying three frame kinds — JSON RPC frames, JSON event frames, and **binary**
PTY frames.

WebSocket is very nearly forced rather than chosen:

- The human's eventual **web frontend** requirement means a browser must be able
  to speak this protocol. A browser speaks WebSocket natively and speaks nothing
  else on the candidate list.
- It is **zero new client-side dependencies** — the webview's own `WebSocket`
  API. That matters more than it sounds: the desktop tree is where CLAUDE.md
  constraint 2's getrandom audit applies, and the client half of this feature
  adds nothing to audit.
- It gives server push, which the event and PTY streams require.

Rejected, with reasons, so the question stays answered:

- **gRPC (tonic/prost).** Browsers cannot speak native gRPC; a grpc-web proxy
  would become a permanent deployment component, and the dependency tree is
  heavy and permanent on both sides.
- **HTTP request/response.** No server push. Polling for `pty-output` defeats the
  entire coalescing design (§9) and multiplies latency on every stream.
- **Reusing the MCP `tiny_http` listener.** Wrong shape — thread-per-request, no
  streaming — and, more importantly, **wrong trust domain**. Agents and display
  clients must remain structurally separate listeners with separate credential
  namespaces (§7 T3). This is the one rejection that is a security decision, not
  an engineering one.
- **SSH as the transport** (i.e. "just use #887"). Different seam entirely; see
  §14.

---

## 4. The wire protocol

### 4.1 Connection lifecycle

```
  TCP → TLS (terminated per H2)
      → HTTP upgrade   ← Origin check, credential presented here
      → server hello   ← protocol version, capabilities, command roster, identity
      → ready          ← RPC / event / PTY frames, full duplex
```

Two rules at the upgrade, both cheap now and load-bearing the day a browser
client exists:

- **Credentials travel in a header or the handshake body, never in the URL.**
  Query strings land in proxy logs, browser history and `Referer`.
- **`Origin` is checked on upgrade, from day one.** A browser will happily let
  any page open a cross-site WebSocket carrying the user's cookies; `Origin` is
  the only defence, and retrofitting it after the web client ships means
  shipping the vulnerable window first.

Authentication happens **at the upgrade, before the socket is considered
established** — not on the first RPC frame. An unauthenticated peer must never
reach a state where it can send a frame the dispatcher looks at (§7 T6: nothing
expensive happens before authentication).

### 4.2 Frame kinds

**RPC frames** mirror today's `invoke` shape, so the 128 wire-reachable commands
port mechanically behind the existing `EngineTransport` seam:

```jsonc
// client → server
{ "t": "rpc", "id": 17, "cmd": "orch_tasks", "args": { "group": "loomux-68435179" } }

// server → client, success
{ "t": "ret", "id": 17, "ok": true, "v": { /* the command's return value */ } }

// server → client, failure
{ "t": "ret", "id": 17, "ok": false, "err": { "code": "unauthorized", "msg": "role viewer < operator" } }
```

**Event frames** carry the backend→frontend stream. The authoritative census is
`test/perfpolicy.test.ts`'s `STREAMS` manifest — 19 declared streams today,
which is also the list a new stream has to join before it can exist:

```jsonc
{ "t": "ev", "name": "orch-attention", "v": { /* today's payload, unchanged */ } }
```

**PTY frames are binary**, and are the one place the wire shape deliberately
differs from today's:

```
  byte 0        frame kind   0x01 = output, 0x02 = resync marker
  bytes 1..5    pane id      u32, little-endian
  bytes 5..     payload      raw pty bytes (kind 0x01 only)
```

No base64. The 33 % inflation and the byte-at-a-time client-side decode exist
today only because Tauri's `emit` inlines the payload into a JS source string
(`pty.rs` wraps each batch in `B64.encode`); a binary WebSocket frame needs
neither. The `0x02` resync marker is how backpressure surfaces (§9).

**PTY *input* stays on the RPC path** as `write_pty`. Keystrokes are tiny, and a
single connection already orders them. The reason to resist a second, faster
input path is authorization: one write path means one place that checks the
caller's role and derives the `human` flag (§6.3), and no second place to forget.

### 4.3 The hello frame

```jsonc
{
  "t": "hello",
  "protocol": 1,
  "engine_id": "…",              // stable per daemon install
  "server_version": "0.9.3",
  "identity": { "user": "…", "session": "…", "role": "operator" },
  "capabilities": { "reveal": false, "open_with": false, "delete_mode": "permanent",
                    "persisted_scrollback": false, "containers": false },
  "commands": [ "orch_tasks", "write_pty", … ]   // the caller's roster, role-filtered
}
```

`commands` is the **authority for feature detection** — not the version number.
A client asks "is `fm_reveal` in my roster?", never "am I talking to ≥ 0.9.3?".
That is what makes additive evolution safe, and it is also why the roster is
role-filtered: a viewer's client can grey out what it genuinely cannot do
without a round trip that would just fail.

`capabilities` generalizes the mechanism that already exists rather than
inventing a second one: `fm_capabilities` returns `Caps { delete_mode, open_with,
reveal, reveal_selects }`, today computed from `cfg!(windows)` at compile time.
In remote mode those must describe the machine that *does the thing*, and the
two halves split: `delete_mode` is a property of the **server's** filesystem
(the files are there), while `open_with` and `reveal` are **client-shell**
operations and go `false` in remote mode regardless of what OS the server is.
Answering all four from the server's `cfg!` would be a quietly wrong answer that
shows the human a menu item that cannot work.

`engine_id` exists for tab persistence: `tabs.json` records pane `cwd`s with no
notion of *which machine* they are on, and a restored layout must reattach to
the engine it came from instead of reading server paths as local ones (track-D slice D1's
engine binding).

### 4.4 Versioning and evolution

- `protocol` is an integer that bumps **only** on a breaking change. There is no
  minor version, because the roster and capability map carry everything a client
  would use a minor version for.
- **Additive only.** New commands, new events, new fields, new capability keys.
  A field is never repurposed and never changes type.
- **Both sides ignore what they do not know.** An unknown event name, an unknown
  field, an unknown capability key is discarded silently, not an error. This is
  the rule that makes independent client/server updates survivable.
- A client whose `protocol` the server does not support is refused **at the
  handshake** with a typed error naming both numbers, not left to fail
  mysteriously on the fourth frame.
- The compatibility window is stated in the user docs when D-track ships.

### 4.5 Error taxonomy

One closed set of codes. The point of a closed set is that the client can branch
on behaviour (reconnect? re-login? show the user?) without string-matching, and
that a reviewer can check the dispatcher never invents a fall-through.

| code | meaning | client behaviour |
|---|---|---|
| `unauthenticated` | no valid connection session (missing, invalid, expired, revoked) | drop to the login/connect screen |
| `unauthorized` | authenticated, role too low for this command | surface; do not retry |
| `not_found` | the named thing does not exist **or the caller may not know it does** | surface as absent |
| `invalid_argument` | server-side validation refused (a `GroupId::parse` failure, an out-of-root path) | bug in the client or a hostile peer; log, do not retry |
| `unsupported_command` | the command name is not on this caller's roster | bug in the client; do not retry |
| `unsupported_version` | handshake only | tell the human to update |
| `rate_limited` | connection or login throttle | back off |
| `internal` | anything else | surface generically; details go to the server log, never the wire |

The `not_found` row is a security decision, not tidiness. A private workspace
session a caller may not see returns `not_found`, **never** `unauthorized` — otherwise the
error code is an oracle that confirms existence, and enumeration by guessing ids
becomes a feature (§7 T4).

### 4.6 Contract fixtures

The frame schemas ship as **JSON fixtures in the repo**, and both consumers read
the same files: the server encodes/decodes them in Rust tests, the client decodes
them in `node:test`. One contract, two implementations, and drift fails on
whichever side drifted. This is what lets track-D slice D1 (the remote transport) be built and
tested with **zero server code in existence** — the payoff the `EngineTransport`
seam was cut for.

---

## 5. The command roster

### 5.1 The rule

**A command is reachable over the wire only by explicit classification. Never by
default.** A new `#[tauri::command]` is unreachable remotely until someone
writes down which class it is in and what role tier it needs — and that is
enforced, not asked for: a test scans `command_manifest::APP_COMMANDS` and fails
if any name lacks exactly one classification, the way `tests/acl_manifest.rs`
already fails when a command has no ACL grant.

That mechanism is not optional bookkeeping, and the manifest itself is the
evidence. The plan-408 census counted 134 commands; `APP_COMMANDS` today lists
**141**. Seven arrived in the interval, and under a hand-maintained allowlist
that nobody re-derived, every one would have been silently wire-reachable or
silently broken. Worse: the file's own per-family count comments have already
drifted — it says `// orchestration (64)` above **66** entries. A hand-kept
number went stale inside the very file whose job is to be the single source of
truth. Default-deny plus a failing test is the only version of this that
survives contact with a year of feature work.

### 5.2 Four classes

| class | meaning |
|---|---|
| **wire** | crosses the socket, subject to a role tier and to server-side root scoping |
| **client-local** | never crosses; runs in the desktop client's own Rust (the hardware or the window is there) |
| **disabled** | meaningless or dangerous remotely; absent from the roster and advertised as absent |
| **retargeted** | the server computes, the client acts (URLs) |

### 5.3 Role tiers

Three tiers, ordered: **viewer** ⊂ **operator** ⊂ **owner**.

- **viewer** — read and observe. Boards, rosters, audit reads, git/gh reads, pane
  output. Cannot type into a pane, cannot dispatch, cannot write a file.
- **operator** — everything a person doing the work needs: spawn and drive panes,
  write files, run git/gh writes, create and steer groups, manage tasks.
- **owner** — **grant-writing only**, and human connection sessions only: task approval,
  merge and release grants, dangerous mode, autonomy raises and budget. These are
  the writes that *are* the enforcement behind constraints 7 and 9, so they are
  the tier that must never be reachable by anything but an authenticated human
  connection session (§7 T3).

### 5.4 The roster, by family

Counts are **counted from `APP_COMMANDS`**, not copied from its comments (which
are where the 64-vs-66 drift above lives).

| family | n | class | tier | notes |
|---|---|---|---|---|
| **pty** (9) | | | | |
| `spawn_pty`, `kill_pty`, `write_pty`, `resize_pty` | 4 | wire | operator | `spawn_pty` executes by design — the single most dangerous name on the wire. `write_pty`'s `human` flag is **derived from the connection session's caller class**, never read from the frame (§6.3) |
| `dir_info`, `change_dir` | 2 | wire | viewer / operator | path arguments root-scoped (#925) |
| `pty_backend_info`, `discover_git_bash`, `discover_ssh` | 3 | wire | viewer | answered with **server** facts; the client must not report its own shell discovery for a server pane |
| **sessions** (3) | 3 | wire | viewer / operator | agent-CLI store scans are server-side; `record_*_launch_posture` is operator |
| **git** (22) | 6 | wire | viewer | the reads: `git_repo_root`, `git_log`, `git_status`, `git_diff`, `git_branches`, `git_worktree_list` |
| | 16 | wire | operator | every write: stage/unstage/commit/commit_files/checkout/discard/worktree_add/fetch/push/pull/tag/branch_create/cherry_pick/revert/merge/rebase. `repo` is root-scoped (#925). Note H9: these push as whoever the daemon is |
| **gh** (10) | 6 | wire | viewer | `gh_auth_status`, `gh_issue_list`, `gh_issue_view`, `gh_pr_list`, `gh_pr_view`, `gh_activity` |
| | 4 | wire | operator | `gh_issue_create`, `gh_issue_set_labels`, `gh_issue_comment`, `gh_pr_comment` — these write to GitHub as the daemon's credential (H9) |
| **gitwatch** (2) | 2 | wire | viewer | |
| **orchestration** (66) | 18 | wire | viewer | reads: `orch_tasks`, `orch_audit`, `orch_merge_queue`, `orch_autonomy`, `orch_group_usage`, `orch_group_summary`, `orch_workflow_status`, `orch_workflow_preview`, `orch_group_watches`, `orch_lock_state`, `orch_group_paused`, `orch_notify_enabled`, `orch_spawn_expanded`, `orch_session_roles`, `orch_channel_list`, `orch_channel_for_pane`, `agent_autopilot_flags`, `agent_cli_knobs` — **all filtered by caller visibility** (§6.4) |
| | 38 | wire | operator | group lifecycle, binding, steering, task CRUD, `orch_request_changes`, attention acks, spawn/solo flow, channel connect/disconnect/set-sender, and the `orch_set_*` knobs that are **not** autonomy raises |
| | 9 | wire | **owner** | `orch_approve_task`, `orch_approve_tasks`, `orch_grant_merge`, `orch_grant_release`, `orch_set_autonomous`, `orch_set_auto_merge`, `orch_set_auto_release`, `orch_set_dangerous_mode`, `orch_set_autonomy_budget` |
| | 1 | **retargeted** | viewer | `orch_open_ref` — the server resolves the ref to a URL (its `open_external_url` helper is the local half today) and returns it; the **client** opens it in the human's browser |
| **cliprobe** (1) | 1 | wire | viewer | `probe_agent_cli` probes the **server's** CLIs |
| **editor** (1) | 1 | **disabled** | — | `open_in_editor` spawns an editor on the machine holding the files; remotely that is either a no-op on a headless box or an arbitrary-process-spawn primitive. The file-editor pane is what covers this case |
| **fileedit** (7) | 4 | wire | operator | `ft_list_dir`, `ft_read_file`, `ft_search_start`, `ft_files_start` — reads, but reads of **server** files, so operator not viewer (H3) |
| | 3 | wire | operator | `ft_write_file`, `ft_replace`, `ft_search_cancel` |
| **filemgr** (9) | 2 | wire | viewer / operator | `fm_list`, `fm_capabilities` (capabilities answered per §4.3's split) |
| | 4 | wire | operator | `fm_new_folder`, `fm_new_file`, `fm_rename`, `fm_delete_start` |
| | 3 | **disabled** | — | `fm_open`, `fm_open_with`, `fm_reveal` — `ShellExecuteW`/`xdg-open` on the server |
| **filehash** (1) | 1 | wire | operator | |
| **obs** (1) | 1 | **client-local** | — | `take_startup_notice` is about the client's own launch |
| **uistate** (6) | 4 | **client-local** | — | `load/save_ui_tabs`, `load/save_settings` — client UI state, plus the `engine_id` binding of §4.3 |
| | 2 | wire | operator | `load/save_ssh_profiles` — **named consequence:** in remote mode an SSH pane is opened *by the engine*, so the hosts it can reach and the identity files it names are the server's, not the client's. The profile store follows the panes. The no-secrets invariant of `sshprofile.ts` is what makes this survivable |
| **voice** (3) | 3 | **client-local** | — | mic capture and whisper are client hardware; the transcript rides `write_pty` like any other keystrokes |

Totals, and they add up to the manifest exactly: **128 wire**, **8 client-local**
(`take_startup_notice`, the four `uistate` UI-state commands, the three
`voice_*`), **4 disabled** (`open_in_editor`, `fm_open`, `fm_open_with`,
`fm_reveal`), **1 retargeted** (`orch_open_ref`) = 141.

The authoritative per-command list is the generated roster the C2 test pins —
this table is the *argument* for it, not a second copy to drift. Which is the
whole point of §5.1: a table in a design note is exactly the artifact that goes
stale, so the note argues and the test enforces.

Two families deserve a sentence they do not get from the table:

**`ft_*`/`fm_*` are a server-filesystem browser** the moment they cross the wire.
That is fine and intended — it is how a human edits a server file — but it means
H3's answer is load-bearing, and #925 is what makes the answer enforceable
rather than aspirational.

**The dialog plugin never crosses.** A folder picker picks *client* folders,
which are meaningless to the engine. Remote mode browses with `ft_list_dir` /
`fm_list` instead. `EngineTransport` already separates these — `pickDirectory`
and `onCloseRequested` sit beside `invoke`/`listen` in that interface precisely
because the display half stays local — and this note is the moment
`engine-transport.md` predicted, when the split stops being a comment and starts
being two implementations.

---

## 6. Identity, roles, and visibility

### 6.0 Terminology, because "session" is already taken three ways

This has to be settled before the model can be written down, and it is the kind
of ambiguity that survives into an implementation as a bug. "Session" already
means at least two things in this codebase, and the human's team/private
requirement adds a third:

| term used in this note | meaning | what it already is in the tree |
|---|---|---|
| **agent session** | one agent CLI's own conversation | `list_sessions`, `session_id`, `resume_orch_session`, `doc/design/session-restore.md` — **unchanged by this note** |
| **connection session** | one authenticated client login: a token, an expiry, a role tier | new; what `Principal.session` names |
| **workspace session** | the unit that is **team or private**, and the unit a container wraps | new |

Below §6.0 the note always writes one of those three phrases and never a bare
"session" — including in the frame schemas, where `Principal.session` is a
connection session. The only unqualified uses are in §1, quoting the human's
original requirement wording verbatim.

**What a workspace session actually is** is itself part of H8, and this note's
recommendation is: **one workspace session = one orchestration group plus the
checkout it works in.** That mapping is not arbitrary — the group is already the
unit of membership, of the audit trail, of MCP token scoping and of `GroupId`
path scoping, so making it the unit of visibility and of container isolation
adds no fourth boundary to reason about. The alternative (a workspace session
spanning several groups, closer to a "project") is coherent but needs a new
membership object that nothing today has, and the human should say so explicitly
if that is what was meant.

### 6.1 Three questions, three mechanisms

Authorization here is not one question but three, and conflating them is how
systems grow holes:

1. **Who are you?** — authentication (H1).
2. **What may you do?** — role tier (§5.3).
3. **What may you see?** — workspace-session visibility (§6.4).

(3) is not a special case of (2). A teammate with operator rights on their own
work has *no* rights over someone else's private workspace session, including the
right to know it exists.

**The `Identity` seam.** Whatever H1 picks, the daemon reduces it to one value
before anything else runs:

```rust
struct Principal { user: UserId, session: SessionId, role: Role, class: CallerClass }
enum CallerClass { Human, Agent }
```

The **source** of `user` is the pluggable part: a built-in store's login, a
verified header from an identity proxy, a client certificate subject. Everything
downstream — roster filtering, visibility filtering, audit actor — reads
`Principal` and never the mechanism. That is what keeps H1 a configuration
decision rather than a rewrite of C1, and it is why C1 can begin the moment H1 is
answered rather than after an auth stack is chosen and built.

**`role` is global to the user; membership is what is per-workspace-session.**
A user has one tier across the deployment, and which workspace sessions they can
see or act in is the *separate* dimension of §6.4. The alternative — per-group
role grants — is a full ACL system (grant tables, inheritance, delegation, an
admin UI to manage it), and v1 does not need one: a team where everyone doing
the work is an operator, a few people are owners, and stakeholders are viewers is
the shape the human described. If per-group roles turn out to be wanted, they
are an additive change: `Principal` grows a resolver, and every call site
already asks "may this principal do this **here**" rather than "what is this
principal's tier". Writing the check that way now costs nothing and keeps the
door open — writing it as a bare tier comparison would nail it shut.

One rule for H1(b) that is easy to get catastrophically wrong: **a proxy-supplied
identity header is trusted only when the connection arrived through the proxy.**
The daemon binds a loopback address or a unix socket and additionally requires a
shared secret from the proxy; a daemon that trusts `X-Forwarded-User` on a
directly-reachable port has no authentication at all, only the appearance of it.

### 6.2 Sessions and tokens

- Login yields a **connection session** with an explicit expiry and a role tier.
  Connection sessions are server-side records, so **revocation is a v1 feature, not v2** — a token
  that cannot be revoked is a permanent credential that happens to have a date on
  it.
- Revocation takes effect **mid-connection**: an established socket re-checks its
  connection session on every RPC dispatch, not only at the upgrade. Otherwise revoking a
  departing teammate leaves their open laptop connected until they close it.
- Tokens keep the existing minting pattern — 128-bit hex from std's OS-seeded
  `RandomState`, exactly as `new_token()` mints MCP tokens today. One fewer
  divergence, and the desktop tree's constraint-2 getrandom prohibition stays
  simple to reason about even though the server crate is Linux-only and would not
  strictly need it.
- **Token values are never logged.** The MCP breadcrumb discipline is the
  precedent and is verbatim what is wanted: its auth-failure breadcrumb records
  `method=… token_present=true|false` and never the value.

### 6.3 Humans and agents stay structurally separate

Today the separation is transport-shaped: the webview reaches `#[tauri::command]`s
in-process, agents reach `mcp::serve()`'s `127.0.0.1:0` listener and are
identified by an `X-Loomux-Agent` token that `resolve_token` maps to
`Caller { agent_id, group: GroupId, role, … }`.

That shape survives verbatim, and the reasons are worth stating because "one API
for every caller" is the tempting simplification:

- **Two listeners, two credential namespaces.** The display WebSocket and the MCP
  server stay separate: a display credential can never authenticate an MCP call,
  and an `X-Loomux-Agent` token can never authenticate a display connection.
- **The MCP tool roster carries no grant-writing tool** — task approval, merge
  and release grants, dangerous mode and autonomy raises are all webview-side
  command writes. That invariant is load-bearing for constraints 7 and 9, and it
  must hold after this change exactly as it holds now. A merged API would launder
  an agent call into human authority, which is precisely the failure the merge
  gate exists to prevent.
- **The MCP server stays engine-local.** It binds loopback on the machine the
  agents run on, and in remote mode that machine *is* the server — so agents
  reach it exactly as they do today. This is the structural reason #888 works
  where #887's SSH panes deliberately refuse orchestration membership: a remote
  agent under SSH cannot reach the loopback MCP server or the `gh` shim at all,
  so it would run with **no merge gate**. Move the whole engine and both come
  along.

**Corollary — the `human` flag.** `write_pty` carries a `human` boolean that
feeds delivery-hold decisions. Over the wire that must be **derived from
`Principal.class`**, never read from the frame. A client-settable "I am a human"
field is an authority claim with no check behind it.

### 6.4 Team and private workspace sessions

A **workspace session** (§6.0) is **team** — shared visibility and control across
the team — or **private** (solo). This is an authorization dimension, not a UI
toggle.

One naming trap to disarm first: the user who created a workspace session is its
**creator**, and that word is used throughout instead of "owner", because `owner`
is already the top **role tier** (§5.3) and the two are independent. A viewer
cannot create anything; an operator can create a workspace session and is its
creator; an `owner`-tier user is not thereby the creator of everyone else's.

The model, subject to H8:

- **Creation.** Any operator may create either kind. Default is **private**
  (H8) — a default of "team" leaks by accident, and accidental sharing is not
  recoverable.
- **Promotion.** The **creator** may promote private → team. **Demotion
  team → private is not offered**: teammates who already observed it cannot
  un-observe it, so a demotion would advertise a privacy it cannot deliver. If
  the intent is "stop sharing", the honest operation is to end the workspace
  session.
- **What a non-member sees of a private workspace session: nothing.** Not a
  greyed-out entry, not a count, not a name. Enumeration APIs — rosters, boards,
  pane lists, audit reads, the merge queue — filter by `Principal`
  **server-side**, and a direct request for its id returns `not_found` (§4.5).
  Client-side filtering of a full list is not filtering; it is a UI convention
  over a data leak.
- **An `owner`-tier user is not exempt from visibility filtering.** This is worth
  stating because the instinct is to make the top tier see everything. It must
  not: `owner` is defined in §5.3 as *grant-writing* authority, which is a
  different axis from *whose work you may look at*. An owner who could read every
  private workspace session would make "private" mean "private from peers", which
  is not what the human asked for. Administrative access to a private workspace
  session, if it is ever wanted, is a break-glass feature with its own audit
  entry — not a silent property of a tier.
- **Composition with containers (§8):** the container boundary is per workspace
  session, so a private one is *also* process- and filesystem-isolated once E2
  lands. Visibility filtering is what protects it before then, and remains the
  answer for team workspace sessions, which share a container by definition.
- **The cross-workspace channel rides unchanged.** `channel_send`/`channel_status`
  take no id arguments, membership is built by human-tier commands, and the
  `SOLO_GROUP` (`__solo__`) model is already a validated `GroupId` rather than a
  caller-supplied string. Nothing here needs a new rule — but a channel that
  crosses a private/team boundary is a visibility decision and must be refused at
  connect time by the same filter.

### 6.5 Audit

Every wire-initiated action logs an **actor identity**: user, connection session and role,
alongside the group and agent the audit already records. Two consequences:

- The audit becomes the only per-user attribution that exists if H9 picks a
  shared service account. That raises its integrity from "nice" to "the record".
- Token values never appear (§6.2). Actor identity is a user id, not a
  credential.

---

## 7. Threat model

Eight threats. Each names what it is, where it is answered, and — because a
mitigation nobody can fail is a mitigation nobody has — how it is *tested*.

| id | threat | primary answer | lands in | test |
|---|---|---|---|---|
| **T1** | unauthenticated peer ⇒ remote code execution | authn before dispatch; default-deny roster; role tiers | C1, C2 | missing / invalid / expired / revoked credential each rejected **before registry state is touched** |
| **T2** | path & identifier injection | validated newtypes + server-declared roots | #904 (done), **#925 (blocker)** | traversal / separator / absolute / empty cases per identifier family |
| **T3** | authority laundering (agent ⇒ human) | two listeners, two namespaces; grant writes owner-human only; `human` derived | C1, C2 | an agent-namespace credential is rejected on the display listener; no grant-writing tool exists in the MCP roster |
| **T4** | cross-user exposure (team vs private) | server-side visibility filtering; `not_found`, never `unauthorized` | C1 | a non-member's enumeration omits the private workspace session; a direct fetch returns `not_found` |
| **T5** | transport attacks | TLS; `Origin` check on upgrade; credentials never in URLs; revocation | C1, C2 | cross-origin upgrade refused; a revoked connection session dies mid-connection |
| **T6** | resource exhaustion | authn before expensive work; bounded per-client buffers; connection and login rate limits | C1, C4 | fake-slow-sink drops and resyncs rather than growing a queue |
| **T7** | audit integrity | actor identity per entry; token values never logged | C1 | a wire action's audit entry names user + connection session + role; a rejection breadcrumb carries no token value |
| **T8** | lateral movement between workspace sessions | containers as defence **in depth over** T2, plus declared egress | E2 | (E2) container cannot reach another workspace session's filesystem; egress reaches only the declared device network |

**T1 — unauthenticated peer means RCE.** This is the catastrophic one and the
reason for everything in §2. `spawn_pty` executes arbitrary commands, `ft_write_file`
writes arbitrary files, `fm_open` hands paths to the shell. The answer is three
things at once, and any one alone is insufficient: authentication before dispatch
(fail-closed, mirroring MCP's "resolve the token to a `Caller` before any tool
runs"), the default-deny roster (§5.1 — a command is reachable because someone
classified it, never because it existed), and role tiers so that spawn/write is
operator-and-above and grant-writing is owner-human-only.

**T2 — path and identifier injection.** `GroupId` is closed: one `parse`, a
strict `[A-Za-z0-9_-]` alphabet that makes `..`, `/`, `\`, `:`, NUL and every
non-ASCII byte *unspellable*, a leading-dash refusal, a Windows reserved-device
refusal, a 64-byte cap, `Deserialize` routed through the same gate, no
`AsRef<Path>`, and a source-scanning test pinning that the orchestration root is
joined with a group in exactly one place. That is the pattern; #925 is the same
pattern applied to what #904 deliberately left: the `ft_*`/`fm_*` roots, the git
`repo` argument, and the `session_id` joined onto a session-state root in the
Copilot digest arm. **No listener slice merges while any caller-supplied
identifier still reaches a path join unvalidated** — this note is the citation a
reviewer uses to make that a blocking finding.

**T3 — authority laundering.** Covered in §6.3. The one-line version: the day
there is one API for humans and agents, the merge gate is decorative.

**T4 — cross-user exposure.** New with multi-user, and the mistake here is
implementing visibility in the client. See §6.4. The error-code choice in §4.5 is
part of the mitigation, not a detail.

**T5 — transport attacks.** TLS (H2). `Origin` checks from day one, because the
browser client is a stated future and cross-site WebSocket hijacking is exactly
what `Origin` exists for. Credentials in headers, never URLs. Revocation as a v1
feature that takes effect mid-connection.

**T6 — resource exhaustion.** Authentication gates every expensive path, so an
unauthenticated peer can consume a handshake and nothing more. Per-client send
buffers are bounded with drop-and-resync (§9), so a slow or hostile viewer cannot
make the server grow memory on its behalf. Connection and login-attempt rate
limits live in the listener — login throttling in particular, because a
password-based H1(a) without it is an offline-speed online guessing oracle.

**T7 — audit integrity.** §6.5.

**T8 — lateral movement.** §8.

---

## 8. Isolation, containers, and the Pi testbed network

### 8.1 Containers compose with the process-level model; they never replace it

The human's requirement is a docker-ready host with the eventual goal of one
container per workspace session. The tempting reading is that a container makes
§7 T2 unnecessary — if each workspace session is confined, who cares whether a
path escapes its root?

That reading is wrong in both directions, and the design must say so:

- **Path scoping is what holds until E2 exists, and E2 is a fast-follow (H5).**
  Between the first end-to-end and the day containers ship, one daemon serves
  every workspace session out of one filesystem, and the *only* thing keeping
  one group's caller out of another's state is T2's validation. A security
  property whose enforcement arrives in a later slice is not a property.
- **A container does not help where the daemon spans it.** Under §6.0's
  recommended one-group-per-workspace-session mapping the container wraps a
  single group — but the RPC dispatcher does not. A caller authenticated for
  workspace session A and a path argument naming workspace session B's group dir
  meet inside **one** daemon process, on the host side of every container
  boundary that exists. The container confines the *agents*; only validation
  confines the *caller*. (And under H8's alternative mapping — a workspace
  session spanning several groups — a container does not even separate the
  agents.)
- **Path scoping does not help when the confinement itself is what you need** — a
  compromised agent process, a malicious dependency in a repo the team cloned, a
  runaway that fills the disk. That is the container's job, and validation cannot
  do it.

They are **layers over the same asset**, and the sequencing follows: T2's
validation lands first (it is cheap, desktop-side, and independently valuable);
containers are defence in depth added over it (H5 — fast-follow). Neither is a
reason to defer the other, and neither is a reason to weaken the other.

The remaining question containers raise, which E2 owes an answer to and this
note explicitly does not guess: **does the engine spawn and manage containers, or
does it run inside one that something else placed?** Both are coherent. The first
makes loomux a container orchestrator, with all that implies about the daemon's
own privileges — a daemon that can create containers can generally escape to the
host. The second keeps the daemon unprivileged and pushes container-per-workspace-session
into the deployment (one daemon per workspace session, fronted by a router), which is more
boxes and less loomux code. This note's recommendation is the **second** for E2's
starting hypothesis, precisely because the first requires giving the daemon the
one privilege whose compromise makes every other layer here moot.

### 8.2 The Pi testbed network

The team runs hardware testers on Raspberry Pis, and agents need to reach them
**from inside their container**. So the isolation model is not "isolated
workspace sessions" but:

> mutually isolated workspace sessions, each with a **deliberate, declared, granted** hole
> to a shared device network.

Two mechanisms, and the note's answer to the plan's open question ("consider
whether device access is a lock resource") is **yes for coordination, no for
isolation** — they are different problems and need both:

- **Reachability is a network policy**, not a loomux feature: the container gets
  an egress rule to the device subnet and nothing else. That is enforced by the
  container runtime and the network, where enforcement actually holds. Loomux
  declares the requirement; the deployment implements it (constraint 8 —
  operator setup does not get baked into product code).
- **Contention is a lock resource**, and here loomux already has exactly the
  right mechanism. `resources:` in `.loomux/workflow.yml` declares named
  resources with `slots` and `max_hold_minutes`, agents take turns via
  `acquire_lock`/`release_lock`, holds are bounded and reclaimed, and the whole
  thing is **advisory by design and says so**. A Pi is a singular device two
  agents must not drive at once — the same shape as `build` or `gpu`. It needs no
  new mechanism, and it must not be described as enforcement, because it is not:
  `doc/design/lock-resources.md` is emphatic that an advisory lock described as
  enforcement is a defect in its own right. The same honesty applies here.

H7 asks for the topology because none of this can be written down as
documentation without knowing the subnet, the addressing and how a Pi is
discovered.

---

## 9. Streaming, backpressure, and the performance invariants

The performance invariants survive the hop, and **the existing coalescer output
is the unit of transmission**.

Today `pty_output_pump` wraps an `OutputCoalescer(PTY_EMIT_MIN_INTERVAL_MS = 16,
PTY_EMIT_MAX_BATCH = 64 KiB)` and its **sink is a closure parameter** — in
`pty.rs` that closure is the `app.emit("pty-output", …)` call. The remote sink is
a different closure writing a binary frame. That is the entire change; the module
was built clock-injected and sink-parameterized for exactly this reuse.

**Coalescing must stay server-side and per client.** The local cost model is
per-*event* (each emit compiles a JS source string on the GUI thread); the wire
cost model is per-message plus per-byte, and the same bounds cover both terms —
and the remote client's webview *still* pays a per-message cost on receipt, so
moving coalescing to the client would reintroduce the flood it exists to prevent.

**The ring tee stays ahead of every client, by construction.** The reader thread
appends to the 256 KiB `OutputBuf` ring **before** it sends to the pump channel
(`pty.rs`, the reader loop). Everything orchestration does — attention scan,
question detection, `get_output`, termgrid replay — reads that ring. So no
client's link quality can affect orchestration correctness, and no client can
make the PTY reader park. This is not a property to preserve carefully; it is a
property of the ordering already in the code, and the design note's job is to say
that it must not be reordered.

**Backpressure is the genuinely new invariant, and it does not translate
literally.** `performance.md` P6 is "Backpressure, not queues, for pipes — a full
pipe parks the writer; that is the bounded-memory answer." Note what it prescribes:
*park the writer*. That is right for the input pipe P6 was written about, where
the writer is a keystroke path and parking it costs one human a moment. It is
exactly wrong here, where the "writer" is the PTY reader thread and parking it on
a slow **viewer** would let one bad link stall the pane — and with it every
detector reading that pane's ring.

So the remote translation keeps P6's *principle* (bounded memory, never an
unbounded queue) and deliberately inverts its *mechanism* (drop the reader's
consumer instead of parking the reader):

> Each client gets a **bounded** send buffer. On overflow the server **drops that
> client's pty stream and sends a resync marker** (`0x02`); the client
> re-attaches via §10's replay. It never grows a queue, and it never parks the
> producer.

That is affordable only because of the tee ordering above: the ring already has
the bytes, so a dropped client loses its *stream position*, not data. Three
options and why this is the one — park the producer: one slow viewer stalls
orchestration, unacceptable. Grow a queue: unbounded memory, which is the failure
P6 names. Drop and resync: bounded memory, and the viewer sees one clean resynced
screen instead of unbounded lag with no way out. A queue trades a visible glitch
for an invisible, unbounded liability.

When P6 is next revised it should carry this second case, so the next reader does
not have to re-derive that "park the writer" was scoped to input pipes.

**Bandwidth, stated so nobody has to guess.** Worst case per pane is 60 × 64 KiB
≈ 3.8 MiB/s — that is a `cat` of a huge file, and it is the cap working as
designed. A busy agent CLI streams roughly 1–50 KB/s. The number that actually
matters is frames per second: ≤ 60 per pane, only while mid-stream, and the
leading edge keeps a quiet pane's echo latency at one round trip. Binary frames
drop the base64 tax outright.

**INV-3 must extend to the socket.** `test/perfpolicy.test.ts`'s `STREAMS`
manifest requires every `listen()` in `src/` to declare a rate class and a bound.
A remote transport that delivers the same 19 streams through a different code
path must not become a way for a stream to arrive undeclared — the manifest is
keyed by event name, so the remote implementation inherits it as long as the
client still subscribes by string literal, which the `EngineTransport` seam
preserves. Say it out loud in track-D slice D1's PR, and check it.

*Not v1, noted so it is not re-invented:* per-pane stream subscription (stream
only the panes a client is displaying). The ring keeps every engine feature
working for unsubscribed panes, so this is a pure bandwidth optimization with no
correctness content.

---

## 10. Detach, reattach, and what a reattach actually restores

The engine never notices a client leaving. Panes, timers, deliveries, watchdog,
the merge queue all continue. **That is the feature** — the human's requirement
promotes it from crash recovery to a primary use case and a hard acceptance
criterion.

On (re)attach:

- **Control state is re-derived by fetch, not replayed.** Boards, rosters, audit,
  merge queue and watches are file-backed reads, and the frontend already
  refetches on hint events. So there is no event-replay log and the protocol
  stays stateless about client history: hello, then fetch. This is worth
  protecting — a replay log is the kind of thing that gets added "for
  correctness" and then owns retention, ordering and compaction forever.
- **Pane content is the one thing that needs new machinery.** Nothing today
  replays a pane: scrollback lives only in xterm.js, the ring is orchestration's,
  and `last_exit_tail` is a corpse snapshot. v1's answer is the composed current
  screen via `termgrid` VT replay (which exists — #520, and is what `get_output`
  already uses) plus the raw ring tail, then the live stream.
- **The honest contract, stated as a contract** (H4): reattach restores **the
  live screen and a recent tail (256 KiB per pane), not infinite scrollback.**
  Priced alternatives, neither in v1: a larger display-purpose ring (server RAM,
  a config knob) or persisted scrollback (disk, plus a retention policy).
- **The offline human.** Desktop toasts have no remote equivalent in v1. The v1
  answer is that pending work is on the board and in the attention set when the
  human reconnects — which is correct behaviour for consent-gated work rather
  than a gap. A server-side notification channel is R4/#848 territory.
- **Multi-client.** v1 is one interactive client plus read-only viewers per pane;
  per-pane input ownership between two operators is genuinely unsolved (the
  human-typing hold machinery assumes one keyboard) and is flagged research, not
  a v1 promise.

---

## 11. What cannot move, and what that costs

| capability | where it stays | remote-mode behaviour |
|---|---|---|
| file dialogs | client | replaced by server-side browsing (`ft_list_dir` / `fm_list`) |
| clipboard | client | unchanged — `navigator.clipboard` plus the OSC 52 bridge inside xterm, and OSC 52 rides inside the pty byte stream |
| voice | client | mic + whisper local; the transcript rides `write_pty` |
| open in editor / reveal / open-with | client | **disabled**, advertised as absent |
| external URLs | split | server resolves, client opens |
| system metrics | server | in remote mode this reports the **server's** load, because that is where the compute is |
| the human's local IDE | client | out of scope by design: clones and worktrees live server-side, and a human who wants a local IDE uses their own clone and push/pull. The editor pane covers the quick fix. Half-supporting this would be worse than declining it |

**Version drift becomes real.** Client and server update independently the day
this ships. The handshake's version and capability exchange (§4.3, §4.4) is the
mechanism, additive-only evolution is the rule, and the user docs state the
compatibility window rather than leaving it folklore.

---

## 12. Not precluding the browser client

The human's requirement is explicit: a web frontend hosted on the remote machine
is out of scope now, but **the transport must not preclude it**. Concretely, that
forbids four things this design therefore does not do:

1. A transport a browser cannot speak (§3 — this is most of why WebSocket wins).
2. An auth mechanism that only a native client can perform (a keychain-bound
   credential, an OS-level handshake). H1's options are all browser-reachable;
   H1(c)'s mTLS is the one that would hurt, which is part of why it is not
   recommended.
3. Frames that assume a Tauri-shaped runtime — hence a plain binary frame for pty
   output rather than anything modelled on `emit`'s payload conventions.
4. Skipping `Origin` checks "because a native client has no origin". They go in
   from day one (§4.1).

The browser client is then a third `EngineTransport` implementation against the
same wire, not a second protocol.

---

## 13. Sequencing, and what blocks what

```
  #904  GroupId + one path-assembly point         ── landed
  #925  remaining identifier families             ── BLOCKER for any listener slice
  A1-A4 engine crate extraction (#847 Ph. 0-2 +)  ── the crate boundary C1 consumes
  B1    this note                                 ── GATE for all of C/D/E
        └─ human answers §1 ──► C1 ► C2 ► C4 ► C5
                                 C3 (parallel, waits A4)
                                 D1 (waits B1 fixtures) ► D2
                                 E1 (waits C2) ► E2 (waits H5)

  A/B/C/D/E + digit = plan-463 build slices.  H<n> = the §1 human decisions.
```

Two blockers stated as blockers, not preferences:

- **#925 is a merge blocker for listener code.** Not "should land first" —
  a reviewer citing this section should treat a listener PR merging ahead of it
  as a blocking finding (§7 T2).
- **A command reachable over the wire without all three of authn, roster
  classification and root scoping is a blocking review finding**, citable from
  §2 and §5.1.

And one thing that is *not* a blocker but is easy to mistake for one: the
engine-crate extraction (track A) is a build-shape prerequisite — a headless
Linux daemon cannot depend on a lib that links Tauri and wants webkit2gtk — not a
security one. C1's auth work is gated on H1, not on A4 finishing.

---

## 14. Interactions with neighbouring work

- **#887 (SSH panes)** cuts at the **pane**: remote compute, local engine, and
  SSH panes are deliberately display-only and refused orchestration membership,
  because a remote agent cannot reach the loopback MCP server or the `gh` shim —
  it would run with **no merge gate**. #888 cuts at the **engine**: everything
  remote, local display, and both come along because the engine moves with them.
  The two do not conflict and neither is a stepping stone to the other.
  `ssh-panes.md`'s own test for a follow-up — "does it need a loomux process, or
  loomux-owned state, on the remote host?" — routes here, and this note is where
  those follow-ups land.
- **#848 (platform: 24/7 server, web UI, manager agent, multi-tenant,
  containers)** is the superset. #888 stays "engine daemon plus thin desktop
  client"; the web UI, manager agent and multi-tenancy are #848's ledger. Shared
  prerequisites get filed once and referenced from both rather than duplicated.
- **#857 (repo split / licensing)** stays HELD. Flagged in §1; no dependency
  taken in either direction.
- **#925** as above.

---

## 15. Constraint 3, restated for this feature

Agents cannot live-validate this. No agent spawns a real agent CLI to test the
daemon — tests fake the agent side, as `src-tauri/tests/` already does. The TUI
detectors are believed OS-independent (they are tuned to CLI versions, not
operating systems) but that belief is **unvalidated on Linux**, and the human
validates it on a real Linux PTY once C3 exists. The same applies to the
real-network behaviour of §9's resync path: a fake-slow-sink test pins the
policy, and only a real WAN link tells you whether the policy feels right.
