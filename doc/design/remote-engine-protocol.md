# Remote engine: the wire protocol and its security model

Issue #888, slice B1 of plan-463. **Status: RESOLVED — the human answered every
open decision on 14 Aug. The prototype build is unblocked.**

The shape that answer took matters more than any individual choice, so it goes
first: **v1 is a prototype with no authentication layer at all.** Its trust
boundary is not a credential but a deployment fact — an SSH tunnel to a local
workstation. Authentication, TLS, multi-user identity and everything built on
them move to a **documented hardening track** (§1.3) rather than gating the first
end-to-end.

This note therefore does two jobs at once, and every section says which it is in:

- **The v1 spec** — what the prototype builds now, including the parts of the
  security model that stay in because they are cheap and still load-bearing
  (server-declared path roots, the command roster, and the two controls in §1.2
  that make "SSH to a workstation" an actual boundary instead of an assumption).
- **The hardening spec** — the auth model (§6) and the threat model (§7) kept
  whole, as the roadmap out of the prototype. They are not deleted and not
  softened. §7.0 is the table that says, per threat, what v1 actually closes and
  what it knowingly leaves open.

It still ships *before* any listener code exists, and that is still deliberate:
the protocol is a public wire contract. What changed is that the security
argument is now a **staged** design rather than a precondition — with the stage
written down, so nobody has to reconstruct later which risks were accepted on
purpose.

Reading order: **§1 is the decision record.** §2 is the premise the whole design
rests on and is unchanged — read it before §1.3, because it is what the accepted
risk is a risk *of*.

Prior art this builds on rather than restates: the architecture proposal and the
build plan on #888 itself, `doc/design/groupid-and-path-roots.md` (#904, layer 2
of this model, landed), `doc/design/engine-transport.md` (#905, the client-side
cut line, landed), `doc/design/pty-output-coalescing.md` (#712/#714),
`doc/design/performance.md` (the invariants this transport must not quietly
escape), `doc/design/lock-resources.md` (#858) and `doc/design/ssh-panes.md`
(#887 — the *other* remote seam, and why it is not this one).

---

## 1. The decision record

### 1.1 What was decided

Nine decisions, all answered 14 Aug. They keep their **H1–H9** numbering — "H"
for human — because that numbering is what the PR, the issue thread and this
note all refer to, and because it never reads as plan-463's track-D slice ids,
which are also called D1 and D2. Anywhere a track-D slice is meant, it is written
out as "track-D slice D1".

The "v1?" column is the one to read while building: it says whether the decision
lands in the prototype or in the hardening track.

| # | Decision | Resolved as | v1? |
|---|---|---|---|
| **H1** | **Auth mechanism** | **No authentication layer in v1.** The trust boundary is a deployment fact — SSH to a local workstation — not a credential. The whole identity design (§6) moves to the hardening track. §1.2 and §1.3 are the terms of that, and they are the most important paragraphs in this note | **deferred** |
| **H2** | **TLS termination** | **Reverse proxy** when it lands — no TLS code or cert rotation in loomux, ever. Deferred alongside H1: an SSH tunnel already carries the transport encryption a local prototype needs, and terminating TLS in front of an unauthenticated socket would protect the wire while leaving the door open, which is theatre | **deferred** |
| **H3** | **Remote filesystem exposure** (proposal Q3) | **Server-declared roots only.** `ft_*`/`fm_*` resolve against registered repos, worktrees and group dirs; an arbitrary client-supplied absolute root is refused. **Stays in v1** — an escape hatch can be added later, never removed later | **v1** |
| **H4** | **Reattach scrollback contract** (Q4) | **Live screen + 256 KiB ring tail.** Persisted scrollback would change the server's storage design and is a feature in its own right | **v1** |
| **H5** | **One container per workspace session** | **Fast-follow**, not v1-blocking. Containers are defence in depth *over* §7's process-level validation, never a replacement (T8) | fast-follow (E2) |
| **H6** | **Protocol** | **Confirmed: one WebSocket per client**, three frame kinds (§3, §4). The prototype's socket is simply an *unauthenticated* one — the framing, the handshake and the roster are unchanged, so adding auth later is an additive change to the handshake, not a new protocol | **v1** |
| **H7** | **Hosting facts** | **Still open, and deliberately not guessed.** Servers are local workstations to begin with, which is what makes §1.2's boundary hold. Administration, the eventual real host, and the Pi network's subnet/addressing/discovery remain TBD from the human — they are inputs to E1's deployment docs and §8.2, not to the prototype | TBD |
| **H8** | **What a workspace session is** | **Confirmed: one orchestration group plus its checkout.** It reuses the boundary that already carries membership, audit, MCP token scoping and `GroupId` path scoping instead of inventing a fourth. The team/private *visibility* model (§6.4) needs identities to mean anything, so it defers with H1 | **v1** (the unit) / deferred (visibility) |
| **H9** | **Whose credentials do server-side agents act as?** | **A bot/service account is the target direction** — possibly reached later through a web or desktop `gh`-CLI auth flow. **For now: one normal single user account**, which is the status quo and identical to today's behaviour. **Per-user own-GitHub credentials are explicitly not the direction** and are ruled out rather than deferred | status quo now, service account later |

**#857 (repo split / licensing) stays HELD.** This design takes no dependency on
it and blocks none of its outcomes; if the engine crate boundary later becomes a
licensing boundary, `loomux-server` sits on the licensed side. Flagged, not
depended on.

Standing decisions from earlier on #888, carried here so the note is
self-contained: **Linux-only daemon** (desktop client stays cross-platform);
**desktop client first, browser client eventually, same wire**;
**detach/reattach on the fly is a hard acceptance criterion**, not crash
recovery; **docker-ready host** with eventual per-workspace-session isolation and
**Pi-testbed egress**; **team vs private workspace sessions** as a first-class
authorization dimension. (The last two are the human's own words; §6.0 pins down
which of the three meanings of "session" they carry.)

One earlier decision is **superseded**, and is called out rather than quietly
dropped: "multi-user connection-session auth from day one" was the answer to the
proposal's Q2. H1 replaces it. Multi-user auth remains the *destination* — §6 is
still written for a team — but it is no longer day one, and no slice waits on it.

### 1.2 The v1 trust boundary, and the two controls that make it real

The prototype's security rests on reachability rather than identity: the daemon
runs on a local workstation and is reached through an SSH tunnel. That is a
coherent boundary, and it is how a great many local dev services are run.

It is only coherent if two things are actually true of the code, and neither is
free-by-assumption — so both are **v1 requirements**, not recommendations:

1. **The listener binds loopback (or a unix socket) and refuses a routable
   interface** unless an explicit, loudly-named flag says otherwise. "We assume
   people will tunnel" is not a boundary; a bind address is. A daemon that
   defaults to `0.0.0.0` has no boundary at all the first time someone runs it on
   a laptop in a café, and the failure is silent — it works exactly as well,
   right up until it is catastrophic.
2. **`Origin` is checked on the WebSocket upgrade, and browser-originated
   connections are refused.** This is the hole the SSH boundary does **not**
   close, and it is worth being precise about because it is easy to miss: the
   tunnel keeps *network peers* out, but a web page the human visits in their own
   browser, on that same workstation, can open a WebSocket to `localhost` — and
   loopback-only binding does nothing against it. Against an unauthenticated
   socket with `spawn_pty` on the roster (§2), that is drive-by remote code
   execution from a malicious ad. The `Origin` check was already in this design
   (§4.1) for the future browser client; it costs nothing and it is the one
   control that must **not** defer with the rest of the auth work.

Both are cheap, and together they are what makes "SSH to a workstation" a
boundary rather than a hope. They land in **different slices**, which this note
originally assigned both to C2 and now records otherwise:

- **Control 1 landed in C1a**, the daemon skeleton, as **config-layer
  validation**: a config naming a routable address without
  `allow_routable_bind: true` does not load at all, and `ServerConfig` holds an
  already-classified `ListenTarget` so an unchecked one is unrepresentable —
  the `GroupId` shape (#904) applied to a bind address. It belongs there
  because the config schema names the address anyway, and a field whose unsafe
  value is spellable with the refusal deferred is a half contract; deciding it
  at parse time also needs no socket, which is what makes it C1a's to test.
  **C2 must not re-implement it**: it receives a checked value and binds it,
  and it must not re-derive the address from the config text.
- **Control 2 remains C2's** — there is no upgrade to check until a listener
  exists.

See `doc/design/remote-engine-daemon.md` §3 for the full argument.

### 1.3 The accepted risk, stated plainly

This note argues in §2 that a naive port of the command surface over a socket is
"an unauthenticated remote-code-execution service that happens to also show
terminals". That argument is not withdrawn, because it is correct: with no
authentication, **anything that can reach the socket has the full wire surface,
which includes `spawn_pty` (arbitrary command execution, by design), file read
and write, and the grant files behind the merge gate.**

The human has accepted that risk for a prototype whose reachability is bounded by
§1.2. Recorded explicitly, because an accepted risk that is not written down
becomes an unexamined one:

- **What is accepted:** no authentication, no authorization tiers, no per-user
  identity, no TLS, no revocation, no rate limiting, and an audit log that cannot
  attribute an action to a person.
- **On what basis:** the daemon is reachable only from the workstation it runs on,
  plus whoever holds SSH access to that workstation — a set that today is one
  person, and whose members already have shell on the box and therefore already
  have everything the socket would give them. *This is the crux of why the risk is
  acceptable: against that specific population the socket grants no privilege its
  callers do not already hold.*
- **What that basis depends on:** §1.2 exactly. The moment the daemon binds a
  routable interface, or is reached by anything other than SSH from a trusted
  workstation, this argument stops holding — not gradually, but immediately and
  completely.
- **What must therefore be true before the prototype outlives its boundary:**
  the hardening track (§6, §7) lands *before* the first non-workstation
  deployment, before the first additional user, and before any browser client.
  §7.0 is the checklist.

Two things stay in v1 despite the deferral, because they are cheap now and
expensive to retrofit: **path-root validation** (#904 and #925 for the
identifiers, #1042 for the roots — a connected client must not escape the
server-declared roots even when it is trusted to connect) and
**command-roster classification** (§5 — default-deny). Neither depends on knowing
*who* is calling, which is precisely why neither needs to wait for auth. They are
also the two that would be most tempting to skip "until auth lands", and the two
whose absence would be hardest to notice.

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

And the surface is worse than one identifier. Today's 146 commands
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
   #925 is the same treatment for the remaining *identifiers* (landed:
   `pathseg::PathSegment`), and #1042 is the root half — this note upgrades that
   remainder from cleanup to a **merge blocker for any listener slice**.
3. **Keep caller classes structurally separate** (§6.3, §7 T3). Humans and agents
   are distinguished today by *transport* — trusted IPC versus MCP tokens. That
   separation must survive as two listeners with two credential namespaces, or
   agent calls launder into human authority.

Containers (§8) are a fourth layer over these three. They are not a substitute
for any of them.

---

## 3. Transport: one WebSocket per client

**Decided (H6): one WebSocket connection per client**, carrying three frame
kinds — JSON RPC frames, JSON event frames, and **binary** PTY frames.

The design target is one *authenticated* connection, and §4/§6 are written that
way. **v1's is unauthenticated** (H1): same framing, same handshake, same roster,
minus the credential. That is deliberate and it is why the deferral is cheap —
adding auth later adds a field to the hello exchange and a check in front of the
dispatcher, not a new protocol and not a re-write of either side.

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
  TCP (bound loopback / unix socket — v1, §1.2)
      → TLS (terminated per H2 — hardening track, not v1)
      → HTTP upgrade   ← Origin check [v1], credential presented here [hardening]
      → server hello   ← protocol version, capabilities, command roster, identity
      → ready          ← RPC / event / PTY frames, full duplex
```

Three rules at the upgrade. The first is v1 and non-negotiable; the other two
describe the authenticated design:

- **`Origin` is checked on upgrade, and browser-originated connections are
  refused — in v1** (§1.2). A browser will happily let any page open a cross-site
  WebSocket; `Origin` is the only defence. Against v1's *unauthenticated* socket
  this stops being a future-proofing nicety and becomes the control that closes
  the one hole an SSH tunnel leaves open — a page in the human's own browser
  reaching `localhost`.
- **Credentials travel in a header or the handshake body, never in the URL.**
  Query strings land in proxy logs, browser history and `Referer`. (Moot in v1;
  there are no credentials.)
- **Authentication happens at the upgrade, before the socket is considered
  established** — not on the first RPC frame. An unauthenticated peer must never
  reach a state where it can send a frame the dispatcher looks at (§7 T6: nothing
  expensive happens before authentication). **Not in v1**: every peer that
  connects is, by construction, unauthenticated and dispatches freely. That is
  the accepted risk of §1.3, and the shape of the eventual check is stated here
  so C2's dispatcher is built with the seam in the right place.

### 4.2 Frame kinds

**RPC frames** mirror today's `invoke` shape, so the wire-reachable commands
(§5.4 counts them, and is the only place that should) port mechanically behind
the existing `EngineTransport` seam:

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
  "identity": { "user": "…", "session": "…", "role": "operator" },  // OMITTED in v1
  "capabilities": { "reveal": false, "reveal_selects": false, "open_with": false,
                    "delete_mode": "permanent",
                    "persisted_scrollback": false, "containers": false },
  "commands": [ "orch_tasks", "write_pty", … ]   // the caller's roster, role-filtered
}
```

**v1 omits `identity` entirely** rather than sending a placeholder. A field
carrying `"user": "local"` would be a claim the daemon cannot support, and the
client would grow code that reads it as though it meant something; an absent
field makes "there is no identity here" unambiguous, and §4.4's ignore-what-you-
do-not-know rule means adding it later is additive. `commands` is present in v1
and unfiltered, since there are no roles to filter by.

`commands` is the **authority for feature detection** — not the version number.
A client asks "is `fm_reveal` in my roster?", never "am I talking to ≥ 0.9.3?".
That is what makes additive evolution safe, and it is also why the roster is
role-filtered once roles exist: a viewer's client can grey out what it genuinely
cannot do without a round trip that would just fail.

`capabilities` generalizes the mechanism that already exists rather than
inventing a second one: `fm_capabilities` returns `Caps { delete_mode, open_with,
reveal, reveal_selects }`, today computed from `cfg!(windows)` at compile time.
In remote mode those must describe the machine that *does the thing*, and the
two halves split: `delete_mode` is a property of the **server's** filesystem
(the files are there), while `open_with`, `reveal` and `reveal_selects` are
**client-shell** operations and go `false` in remote mode regardless of what OS
the server is. Answering all four from the server's `cfg!` would be a quietly
wrong answer that shows the human a menu item that cannot work — and
`reveal_selects` is the sharpest case, because it exists precisely to let the
menu label itself honestly when reveal is approximate. All four appear in the
example above; a capability map that silently omits one is a client defaulting
to a guess.

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

The set is defined whole now and **kept whole in v1**, including the two codes a
prototype can never emit. Adding a code later is an additive protocol change the
client must already tolerate (§4.4), but a client written against a set that
never mentioned `unauthenticated` is a client whose reconnect logic has nowhere
to hang — and that logic is the expensive part, not the code.

| code | meaning | client behaviour | v1 |
|---|---|---|---|
| `unauthenticated` | no valid connection session (missing, invalid, expired, revoked) | drop to the login/connect screen | never emitted |
| `unauthorized` | authenticated, role too low for this command | surface; do not retry | never emitted |
| `not_found` | the named thing does not exist **or the caller may not know it does** | surface as absent | emitted (existence only) |
| `invalid_argument` | server-side validation refused (a `GroupId::parse` failure, an out-of-root path) | bug in the client or a hostile peer; log, do not retry | **emitted — this is T2 in v1** |
| `unsupported_command` | the command name is not on this caller's roster | bug in the client; do not retry | **emitted — this is default-deny in v1** |
| `unsupported_version` | handshake only | tell the human to update | emitted |
| `rate_limited` | connection or login throttle | back off | never emitted |
| `internal` | anything else | surface generically; details go to the server log, never the wire | emitted |

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
if any name lacks exactly one classification.

This is not a new mechanism, which is the point — it is the third instance of
one `tests/acl_manifest.rs` already runs three times over. That file pins that
`generate_handler!` and `APP_COMMANDS` agree
(`generate_handler_matches_app_commands`), pins the **total**
(`app_commands_len_is_<N>`, where N is the count itself and the assertion message
carries its per-delta provenance), and pins that every command is granted to
`main`. A remote roster is the same shape of guard
over the same list, and a reviewer already knows how to read it.

That mechanism is not optional bookkeeping, and this repo's own artifacts are the
evidence. The plan-408 census counted 134 commands; `APP_COMMANDS` today lists
**146**. Twelve arrived in the interval, and under a hand-maintained allowlist
that nobody re-derived, every one would have been silently wire-reachable or
silently broken.

Nearby numbers show what happens to the ones a test does *not* pin. When this
section was written the manifest's own per-family comment read
`// orchestration (64)` above **66** entries, and `architecture.md` described the
file as "the ACL manifest's 123 app-command names"; both were corrected in that
slice. Then it happened again, to this very passage: the three counts in these
paragraphs were bumped by hand — *relatively*, +1 per command added, rather than
re-derived from `APP_COMMANDS` — and drifted to 142 against a real 146 (#1018).
An unpinned number does not stay right merely because the prose around it argues
that unpinned numbers go wrong. Meanwhile `app_commands_len_is_<N>`, the number
that *is* pinned, stayed correct through every one of those additions. Default-deny plus a failing test is the only
version of this that survives contact with a year of feature work; a table
maintained by good intentions is the version that does not.

### 5.2 Four classes

| class | meaning |
|---|---|
| **wire** | crosses the socket, subject to a role tier and to server-side root scoping |
| **client-local** | never crosses; runs in the desktop client's own Rust (the hardware or the window is there) |
| **disabled** | meaningless or dangerous remotely; absent from the roster and advertised as absent |
| **retargeted** | the server computes, the client acts (URLs) |

### 5.3 Role tiers

> **Deferred with H1.** v1 has no tiers: with no identity there is nobody to
> assign one to, so every wire command is reachable by whoever holds the socket.
> The classification below is still written down now, and the tier column in §5.4
> is still filled in, for a reason that is not bookkeeping: **the classification
> is the cheap half and the enforcement is the expensive half.** Deciding
> `orch_grant_merge` is owner-tier costs a table cell today; discovering it was
> never marked, after a year of commands landing without anyone asking, costs an
> audit of all 146 of them. The roster ships in v1 (§5.1); the tier column is the
> hardening track reading from a table that was kept current all along.

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
| `dir_info`, `change_dir` | 2 | wire | viewer / operator | path arguments root-scoped (#1042) |
| `pty_backend_info`, `discover_git_bash`, `discover_ssh` | 3 | wire | viewer | answered with **server** facts; the client must not report its own shell discovery for a server pane |
| **sessions** (3) | 3 | wire | viewer / operator | agent-CLI store scans are server-side; `record_*_launch_posture` is operator |
| **git** (22) | 6 | wire | viewer | the reads: `git_repo_root`, `git_log`, `git_status`, `git_diff`, `git_branches`, `git_worktree_list` |
| | 16 | wire | operator | every write: stage/unstage/commit/commit_files/checkout/discard/worktree_add/fetch/push/pull/tag/branch_create/cherry_pick/revert/merge/rebase. `repo` is root-scoped (#1042). #925 routed exactly two arms through `safe_resolve` — `git_discard(untracked)` and `git_diff(untracked)`; `git_stage`/`git_unstage` `paths` still reach the git CLI directly and are contained by git own outside-repository refusal, not by #925. Note H9: these push as whoever the daemon is |
| **gh** (11) | 7 | wire | viewer | `gh_auth_status`, `gh_label_vocabulary`, `gh_issue_list`, `gh_issue_view`, `gh_pr_list`, `gh_pr_view`, `gh_activity`. `gh_label_vocabulary` reads the repo's label set (it is what stops the issues view hardcoding a vocabulary), so it is a read of the **server's** repo like the rest of this row |
| | 4 | wire | operator | `gh_issue_create`, `gh_issue_set_labels`, `gh_issue_comment`, `gh_pr_comment` — these write to GitHub as the daemon's credential (H9) |
| **gitwatch** (2) | 2 | wire | viewer | |
| **orchestration** (69) | 19 | wire | viewer | reads: `orch_tasks`, `orch_audit`, `orch_merge_queue`, `orch_autonomy`, `orch_group_usage`, `orch_group_summary`, `orch_workflow_status`, `orch_workflow_preview`, `orch_group_watches`, `orch_lock_state`, `orch_group_paused`, `orch_notify_enabled`, `orch_spawn_expanded`, `orch_session_roles`, `orch_channel_list`, `orch_channel_for_pane`, `orch_questions_list`, `agent_autopilot_flags`, `agent_cli_knobs` — **all filtered by caller visibility** (§6.4) |
| | 38 | wire | operator | group lifecycle, binding, steering, task CRUD, `orch_request_changes`, attention acks, spawn/solo flow, channel connect/disconnect/set-sender, and the `orch_set_*` knobs that are **not** autonomy raises |
| | 11 | wire | **owner** | `orch_approve_task`, `orch_approve_tasks`, `orch_grant_merge`, `orch_grant_release`, `orch_set_autonomous`, `orch_set_auto_merge`, `orch_set_auto_release`, `orch_set_full_autonomy`, `orch_set_dangerous_mode`, `orch_set_autonomy_budget`, `orch_question_answer` |
| | 1 | **retargeted** | viewer | `orch_open_ref` — the server resolves the ref to a URL (its `open_external_url` helper is the local half today) and returns it; the **client** opens it in the human's browser |
| **cliprobe** (1) | 1 | wire | viewer | `probe_agent_cli` probes the **server's** CLIs |
| **modelwire** (1) | 1 | wire | viewer | `list_cli_models` (#993) reads what the **server's** startup sweep found for a CLI. Operator under #993, when the command itself spawned the agent CLI and a viewer clicking `detect` could have spent the operator's credits. #1020 removed that: the command is a memo LOOKUP that cannot spawn anything, the sweep is the only spawn site and runs on the server's own schedule with no client able to trigger it, so the answer is now a read like `probe_agent_cli`'s. **The underlying cost claim is still unverified** (doc/design/model-catalog.md §Credit safety) — what changed is that no client gesture reaches it. Restoring a client-triggered ask would make this operator again |
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

Totals: **133 wire**, **8 client-local** (`take_startup_notice`, the four
`uistate` UI-state commands, the three `voice_*`), **4 disabled**
(`open_in_editor`, `fm_open`, `fm_open_with`, `fm_reveal`), **1 retargeted**
(`orch_open_ref`) = **146**, which is `APP_COMMANDS.len()` at the time of
writing. Every command has exactly one disposition, and the four buckets
partition the manifest — that is the property this table exists to assert.

The authoritative per-command list is the generated roster the C2 test pins —
this table is the *argument* for it, not a second copy to drift. Which is the
whole point of §5.1: a table in a design note is exactly the artifact that goes
stale, so the note argues and the test enforces.

**And until C2 ships, nothing enforces it, which is why this table has now gone
stale three times** (#1018 rounds 1–2). "The note argues and the test enforces"
describes the end state, not today: the roster test is part of C2, so between now
and then these numbers are exactly the unpinned kind §5's own opening paragraphs
warn about. A command added to `APP_COMMANDS` in the meantime does not fail
anything here — it silently leaves a name with no disposition, which is how
`gh_label_vocabulary`, `orch_set_full_autonomy`, `orch_questions_list` and
`orch_question_answer` all reached this file unplaced. **Re-derive these counts
from `APP_COMMANDS` when you touch them; never bump them relatively.** A relative
bump is what carried the error through two separate reviews.

**`orch_question_answer` is owner-tier for a different reason than the rest of
that row, and the difference matters.** The other ten are grant-writes — the
enforcement behind constraints 7 and 9. This one is not; it qualifies on the
*other* half of the owner definition, "human connection sessions only". The
command hard-codes `AnswerSource::Webview` rather than taking a `source`
parameter, precisely so that who answered is a fact loomux establishes rather
than one a caller asserts about itself (see its doc comment). Cross the wire at
any lower tier and that guarantee inverts: a remote peer's answer gets stamped as
a human's, and the registry can no longer distinguish the human it was waiting on
from anything else holding a socket. Owner is therefore the fail-closed reading,
not a comfortable one — if the tiers are ever restructured so that "grant-write"
and "human-session-only" stop travelling together, this command follows the
second, not the first.

Two families deserve a sentence they do not get from the table:

**`ft_*`/`fm_*` are a server-filesystem browser** the moment they cross the wire.
That is fine and intended — it is how a human edits a server file — but it means
H3's answer is load-bearing, and #1042 is what makes the answer enforceable
rather than aspirational.

**The dialog plugin never crosses.** A folder picker picks *client* folders,
which are meaningless to the engine. Remote mode browses with `ft_list_dir` /
`fm_list` instead. `EngineTransport` already separates these — `pickDirectory`
and `onCloseRequested` sit beside `invoke`/`listen` in that interface precisely
because the display half stays local — and this note is the moment
`engine-transport.md` predicted, when the split stops being a comment and starts
being two implementations.

---

## 6. Identity, roles, and visibility — the hardening spec

> **Not in v1.** H1 defers this whole section: the prototype has no
> authentication, no role tiers and no per-user identity. It is kept complete and
> unedited because it is the destination, and because the parts of the protocol
> that *are* in v1 were shaped to make it an additive change later — the roster is
> already per-caller (§4.3), the error taxonomy already distinguishes
> `unauthenticated` from `unauthorized` (§4.5), and §6.1's `Identity` seam means
> the eventual mechanism choice does not rewrite the listener. Read it as the spec
> for the track, not a description of the prototype.


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
  Connection sessions are server-side records, so **revocation ships in the auth
  layer's first release, never as a follow-up to it** — a token that cannot be
  revoked is a permanent credential that happens to have a date on it. (Said that
  way deliberately: "v1" in this note means the *prototype*, which has no tokens
  at all, so calling revocation a v1 feature would name the one release it
  provably is not in.)
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

- Token values never appear (§6.2). Actor identity is a user id, not a
  credential.
- The audit becomes **the only per-user attribution that exists** once H9's
  service account lands. See §6.6 — this is the load-bearing consequence of that
  decision, not a footnote to it.

**v1 status: degraded, knowingly** (§7.0 T7). The audit still records what
happened and against which group; it cannot record *who*, because there is no
who. Under H9's single account that costs nothing today — one account, one
person, no ambiguity to resolve. It stops being free the moment a second person
connects, which is one of the four triggers in §7.0.

### 6.6 Whose credentials the agents act as (H9)

Resolved, and worth its own subsection because it is a standing product decision
rather than a v1/deferred toggle:

- **Now: one normal single user account.** Identical to today's behaviour — the
  agents on the server use the machine's `gh` auth and agent-CLI subscriptions,
  the same way agents on a laptop use the human's. No change, no new mechanism,
  nothing to build.
- **Target: a bot/service account**, possibly reached through a web or desktop
  `gh`-CLI auth flow when that is built.
- **Explicitly ruled out: per-user own-GitHub credentials.** Not deferred —
  *rejected*. It is the option that would need a secret store loomux does not
  have and does not want, and it is written down as closed so it does not get
  re-proposed as the obvious answer every time attribution comes up.

The consequence to carry forward: **a service account collapses GitHub-side
attribution.** Once agents push, comment and label as one bot, GitHub can no
longer tell you which person's work a change came from — every PR looks like the
bot's. The loomux audit log is then the only place that mapping exists, which is
why §6.5's actor identity is not optional once the service account lands: the two
decisions are a pair, and shipping the service account without per-user identity
would mean *nothing anywhere* records who asked for what. Not a v1 problem (one
account, one person); a hard prerequisite for the service account.

---

## 7. Threat model — the hardening roadmap

Eight threats. Each names what it is, where it is answered, and — because a
mitigation nobody can fail is a mitigation nobody has — how it is *tested*.

**This section survives H1's deferral intact, and is the reason the deferral is
safe to make.** A prototype that shipped without a threat model would have to
grow one under pressure later, from a codebase already shaped by its absence.
This one exists first, so the hardening track is a checklist rather than an
investigation.

### 7.0 What v1 actually closes, and what it knowingly leaves open

Read this table before building anything. The "v1" column is the honest status of
the prototype; the rest of §7 is the full specification each threat is eventually
held to.

| id | v1 status | why |
|---|---|---|
| **T1** RCE via unauthenticated peer | **OPEN — accepted** | §1.3. Mitigated only by reachability (§1.2: loopback bind + `Origin` refusal), never by identity. This is *the* accepted risk |
| **T2** path & identifier injection | **HALF-CLOSED in v1** | #904 landed; #925 closed the identifier half; #1042 owes the root half. Cheap, independent of identity, and expensive to retrofit — so it stays in (§1.3) |
| **T3** authority laundering (agent ⇒ human) | **structurally held** | Not by role tiers, which do not exist yet, but by the listeners staying separate (§6.3): MCP keeps its own token namespace, and the display socket adds no grant-writing path that the MCP roster lacks. The *tiering* half is deferred |
| **T4** cross-user exposure | **N/A in v1** | One user, one account (H9). Nothing to expose across. Becomes live the moment a second person connects — which is the trigger for the hardening track, not a later nice-to-have |
| **T5** transport attacks | **partially closed** | `Origin` refusal is **in v1** and non-negotiable (§1.2 — SSH does not close it). TLS defers to the reverse proxy; credentials-in-URLs and revocation are moot with no credentials |
| **T6** resource exhaustion | **partially closed** | Bounded per-client send buffers with drop-and-resync are in v1 (C4) because they are a correctness property of streaming, not a security feature. Rate limiting defers with auth |
| **T7** audit integrity | **DEGRADED — known** | The audit still records what happened; it cannot say *who*, because there is no who. Under H9's single account that is no loss today, and it is exactly what the service account plus per-user identity restores. Recorded so the gap is not discovered later as a surprise |
| **T8** lateral movement | **deferred (E2)** | Containers are fast-follow (H5) and were always defence in depth over T2, never a replacement |

The trigger to start the hardening track is not a date. It is any one of: **a
second user**, **a non-workstation host**, **a routable bind**, or **a browser
client**. Each of those individually invalidates §1.3's basis.

### 7.1 The threats in full

| id | threat | primary answer | lands in | test |
|---|---|---|---|---|
| **T1** | unauthenticated peer ⇒ remote code execution | authn before dispatch; default-deny roster; role tiers | C1, C2 | missing / invalid / expired / revoked credential each rejected **before registry state is touched** |
| **T2** | path & identifier injection | validated newtypes + server-declared roots | #904 (done), #925 (identifiers, done), **#1042 (roots, blocker)** | traversal / separator / absolute / empty cases per identifier family |
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

**In v1, exactly one of those three is present** — the roster. There is no
authentication and there are no tiers, so what stands between a peer and
`spawn_pty` is reachability alone (§1.2) and the human's acceptance of that
(§1.3). Stated here, and not only in §1, because T1 is the paragraph someone
will read when they are deciding whether it is safe to expose this daemon —
and the answer, until the hardening track lands, is **no**.

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

**T2 has since split in two, and only one half is closed.** Working #925
established that those three are not one problem. The **identifier** half —
`session_id`, the agent id, and the `rel` arguments of `git_diff`/`git_discard`
— is segment validation, and it landed as `pathseg::PathSegment`, the one
validating constructor `GroupId` and every other path-component family now share
(`groupid-and-path-roots.md`). The **root** half is not a segment problem at all:
`ft_*`/`fm_*`/git take `(root, rel)`, the `rel` side was already contained by
`fileedit::safe_resolve`, and what is unguarded is the caller-supplied absolute
`root` itself. No string predicate can close that — for a root, absolute is
*required* — so it needs H3's registry, and it is tracked on **#1042**.

For a reviewer, the operative sentence is therefore: **#1042, not #925, is the
listener's remaining T2 blocker.** A green #925 segment slice does not lift the
gate, and must not be cited as though it had.

**T3 — authority laundering.** Covered in §6.3. The one-line version: the day
there is one API for humans and agents, the merge gate is decorative.

**T4 — cross-user exposure.** New with multi-user, and the mistake here is
implementing visibility in the client. See §6.4. The error-code choice in §4.5 is
part of the mitigation, not a detail.

**T5 — transport attacks.** TLS (H2). `Origin` checks from day one, because the
browser client is a stated future and cross-site WebSocket hijacking is exactly
what `Origin` exists for. Credentials in headers, never URLs. Revocation lands
with the auth layer rather than after it, and takes effect mid-connection.

**v1:** the `Origin` half is in and is load-bearing (§1.2); the rest describes
the authenticated design. Note the asymmetry — "from day one" here means the
prototype's day one, which is the *only* one of these that is true today.

**T6 — resource exhaustion.** Authentication gates every expensive path, so an
unauthenticated peer can consume a handshake and nothing more. Per-client send
buffers are bounded with drop-and-resync (§9), so a slow or hostile viewer cannot
make the server grow memory on its behalf. Connection and login-attempt rate
limits live in the listener — login throttling in particular, because a
password-based store without it is an offline-speed online guessing oracle.

**v1:** the second sentence holds and the first does not. Bounded buffers are in
(they are a streaming correctness property, not a security feature); the
authentication gate is not, so a peer that reaches the socket can spend the
server's resources freely. Bounded by §1.2's reachability, like everything else
in §1.3.

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

## 13. Sequencing — the prototype is unblocked

H1's deferral removes what was the longest pole: C1 was "server skeleton **and
auth**", gated on an auth-mechanism decision, and everything else in track C
queued behind it. The prototype path is now:

```
  #904  GroupId + one path-assembly point         ── landed
  #925  remaining identifier families             ── identifier half landed
  #1042 server-declared root registry             ── still a merge blocker for listener code
  B1    this note                                 ── RESOLVED; gate lifted
  A1    engine workspace scaffold                 ── start now, the serial chain's head
  A2 ► A3 ► A4  engine extraction (#847 Ph. 0-2 +)── the crate boundary the daemon consumes

  C1a   server skeleton, NO auth                  ── crate + config; CARRIES §1.2's bind
                                                     refusal as config validation
  C1b   engine hosting (waits A4)                 ── the daemon owns a registry
  C2 ► C4 ► C5   listener, streams, replay        ── Origin refusal (§1.2) is C2's; the
                                                     bind refusal is NOT C2's to repeat
  C3    headless PaneHost (parallel, waits A4)
  D1 ► D2        remote client (D1 waits B1 fixtures — available now)
  E1 ► E2        docker-ready, then containers (fast-follow, H5)

  A/B/C/D/E + digit = plan-463 build slices.  H<n> = the §1 human decisions.
```

**What shrank:** C1 loses the user store, the login flow, session issuance, role
resolution and revocation. What remains is a daemon that starts, reads a config,
owns a registry and serves nothing yet — days, not weeks.

**What did not shrink, and must not be quietly dropped along with auth:**

- **#1042 remains a merge blocker for listener code.** This is the one to guard
  hardest, because the temptation now is to read "no auth in v1" as "security
  work defers". It does not: this is *path* validation, it does not depend on
  knowing who is calling, and §1.3 keeps it explicitly in v1. A listener PR that
  merges ahead of it is a blocking finding (§7 T2).

  The blocker moved from #925 to #1042 when the two halves separated, and the
  distinction is the thing to hold onto: #925 closed the **identifier** half
  (validated segments — session id, agent id, the `rel` arguments), which is
  real work but is *not* what H3 asks for. H3 asks that an arbitrary
  client-supplied absolute **root** be refused, and no segment validator can
  express that. **A green #925 is not the gate lifting.**
- **The two §1.2 controls are v1 requirements** — loopback-or-unix-socket bind
  with a routable interface refused by default, and `Origin` refusal on upgrade.
  They are what the accepted risk in §1.3 is conditioned on. A prototype that
  ships without them has not deferred security; it has removed the boundary the
  deferral assumed. **The first landed in C1a as config-layer validation and is
  not C2's to repeat** (§1.2); the second is C2's, unchanged.
- **Roster classification lands with the dispatcher** (§5.1), default-deny, with
  its test. Tiers defer (§5.3); classification does not.

The revised blocking-finding rule for reviewers, replacing the pre-H1 one: **a
command reachable over the wire without both roster classification and root
scoping is a blocking finding**, citable from §2 and §5.1. Authentication drops
out of that sentence for the prototype and returns with the hardening track.

One thing that is *not* a blocker but is easy to mistake for one: the engine-crate
extraction (track A) is a build-shape prerequisite — a headless Linux daemon
cannot depend on a lib that links Tauri and wants webkit2gtk — not a security one.

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
- **#1042** as above (#925 closed the identifier half; the root half is what blocks).

---

## 15. Constraint 3, restated for this feature

Agents cannot live-validate this. No agent spawns a real agent CLI to test the
daemon — tests fake the agent side, as `src-tauri/tests/` already does. The TUI
detectors are believed OS-independent (they are tuned to CLI versions, not
operating systems) but that belief is **unvalidated on Linux**, and the human
validates it on a real Linux PTY once C3 exists. The same applies to the
real-network behaviour of §9's resync path: a fake-slow-sink test pins the
policy, and only a real WAN link tells you whether the policy feels right.

---

## 16. Status: the prototype is unblocked

Every decision this note was written to surface has an answer (§1.1). Nothing
here is waiting on the human except H7's hosting facts, which are inputs to E1's
deployment documentation and §8.2's Pi topology — not to any prototype slice.

Cleared to start:

- **A1** — engine workspace scaffold, then the A2 → A3 → A4 extraction chain.
- **C** — the daemon, without auth: skeleton (carrying §1.2's bind refusal as
  config validation), listener (carrying §1.2's `Origin` refusal and §5.1's
  roster), headless `PaneHost`, per-client streams, replay-on-attach.
- **track-D slices D1 → D2** — the remote client; D1 can begin immediately
  against §4.6's contract fixtures, with no server in existence.
- **E1** — docker-ready packaging, with **E2** containers as the fast-follow H5
  chose.

Still blocking, and unchanged by any of the above: **#1042** for listener code
(§13). Still true, and the thing this note exists to keep true: the prototype is
a prototype *because of where it runs*, not because the security model was
skipped. §7.0 is the way out, and its four triggers are what say when the way out
stops being optional.
