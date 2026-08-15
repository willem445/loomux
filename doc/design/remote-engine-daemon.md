# The remote-engine daemon: the process, and its configuration contract

Issue #888, plan-463 track C — slice **C1a**, the first build slice of the
daemon itself. [`remote-engine-protocol.md`](remote-engine-protocol.md) is the
wire contract and the security model this crate will eventually implement; this
note is about the **process** that will implement it: what `loomux-server` is,
what it deliberately is not yet, and the one contract it establishes now
because establishing it later is more expensive.

Prior art it builds on rather than restates:
[`engine-extraction.md`](engine-extraction.md) (why `loomux-engine` exists and
what is still moving into it), `remote-engine-protocol.md` §1.2/§1.3 (the v1
trust boundary and the accepted risk — read those two sections before running
this daemon anywhere) and §13 (the slice ordering this note sits inside).

---

## 1. What C1a is, and the two things it is not

`crates/loomux-server` is the third workspace member and the first that is a
**binary**. Today it:

- parses a command line,
- loads a YAML config file (or runs with defaults),
- decides whether a listener would be **allowed** to bind the address it was
  given — §3, the one real contract in this slice —
- prints what it resolved, and exits.

It is **not** a listener (that is C2: the socket, the `Origin` refusal on
upgrade, the hello frame, roster-restricted RPC dispatch) and it does **not**
own an engine yet (that is A4: `OrchRegistry` cannot move to this side of the
Tauri boundary until the extraction reaches it). Those absences are the slice
boundary, not an oversight, and the binary says so out loud rather than
pretending — see §5 on the exit code.

**Why a slice this small is worth its own PR.** The protocol note's §13
describes what remained of C1 once the auth decision (H1) removed the user
store, login and session issuance from it: "a daemon that starts, reads a
config, owns a registry and serves nothing yet". Half of that — owning a
registry — is blocked on A4, and the other half is genuinely independent of it.
Shipping the independent half now means C2 opens against a crate that exists,
with the bind decision already made and tested, instead of carrying a listener,
a config format and a security control in one unreviewable diff.

## 2. Where the daemon deploys, and where it builds

The deployment target is **Linux only** — a standing #888 decision, and the
reason the engine had to stop linking Tauri in the first place.

The crate nonetheless **builds and tests on all three CI platforms**, and that
is deliberate rather than incidental. Everything in C1a is portable `std`: the
config layer resolves no addresses, opens no sockets and touches no
platform-specific API, so there is nothing to gate. A `#[cfg(target_os =
"linux")]` around it would buy nothing and cost the thing that matters most
here — a security rule (§3) that only compiles on the deployment host is a rule
no reviewer on any other machine can run, and this repo's CI matrix is two
thirds not-Linux.

The point where that stops being free is C2's socket: a unix-domain listener is
a Linux (and macOS) API. The config layer already anticipates it — a
`unix:<path>` target **classifies** on every platform, because classification
is string work — and whether the host can actually bind one is the listener's
error to report. Keeping the split at "classify anywhere, bind where you can"
is what keeps the tests platform-free.

## 3. The bind decision, and why it is here rather than in C2

`remote-engine-protocol.md` §1.2 states two v1 requirements, and is emphatic
that they are requirements rather than recommendations, because the prototype
has **no authentication at all** and they are what make "reach it over SSH" a
boundary instead of a hope:

1. the listener binds loopback (or a unix socket) and **refuses a routable
   interface** unless an explicit, loudly-named flag says otherwise;
2. `Origin` is checked on the WebSocket upgrade.

The second is C2's — there is no upgrade to check. The first lands **here**,
one slice ahead of the socket, and the argument is worth stating because it
moves a control out of the slice the protocol note assigned it to:

- **The config schema has to name the listen address anyway.** Config is the
  entirety of C1a's content; the address is its most important field.
- **A field that accepts `0.0.0.0` with the refusal deferred is a half
  contract.** The unsafe value would be spellable, documented and silently
  honoured in the interval, and "the listener will refuse it later" is not a
  property of anything that exists.
- **One place produces a `ListenTarget`.** C2 receives a value that has already
  passed the check rather than a string it must remember to check, so the
  control cannot be forgotten by the slice that has the most else going on.

What C2 still owes: the `Origin` refusal, and actually binding what it is
handed. It must not re-derive the address from the config text.

### The rule, precisely

A `listen:` value classifies into exactly one of three targets:

| value | target | allowed by default |
| --- | --- | --- |
| `127.0.0.1:8788`, `[::1]:8788` | loopback | yes |
| `unix:/run/loomux/engine.sock` | unix socket | yes |
| `0.0.0.0:8788`, `[::]:8788`, `192.168.1.5:8788` | **routable** | **no** — needs `allow_routable_bind: true` |

Three deliberate details:

- **The wildcards are routable.** `0.0.0.0` and `::` are the single most likely
  thing an operator types, and they are reachable on every interface the
  machine has. The café failure §1.2 describes is exactly this value working
  perfectly right up until it is catastrophic.
- **Host names are refused, never resolved.** Whether `localhost` is loopback
  is a DNS answer; it can differ between the check and the bind, and a resolver
  that hands back a routable address for a name the operator believed was local
  would launder the control into a lookup. An IP literal is required and the
  refusal says why.
- **An allowed routable target is still marked routable.** The flag removes a
  refusal; it does not turn the address into a safe one. The startup banner
  warns off `ListenTarget::is_routable()`, not off the config flag, so every
  later consumer inherits the honest answer.

## 4. The config file, and one place it deliberately contradicts the wire

The file is YAML, parsed with `serde_norway` — the same format and the same
parser as `.loomux/workflow.yml`. An operator hand-editing loomux configuration
edits one format, and nothing new joins the workspace lock; a config format is
exactly where a daemon otherwise grows its first gratuitous dependency.

```yaml
# loomux-server.yml — every key is optional; this is the full v1 surface
listen: "127.0.0.1:8788"        # or "[::1]:8788", or "unix:/run/loomux/engine.sock"
allow_routable_bind: false      # §3. Setting this exposes an UNAUTHENTICATED daemon
state_root: /var/lib/loomux     # default: the engine's own data root
```

**Unknown keys are a hard error**, and that is the opposite of the wire's rule
(`remote-engine-protocol.md` §4.4: "both sides ignore what they do not know").
The two do not conflict, because they are answers to different questions:

- The **wire** rule exists for two peers that update independently. Tolerating
  an unknown field is what makes an old client survive a new server.
- The **config file** has exactly one author, on this machine, editing by hand.
  There is no version skew to survive — an unrecognised key is a typo, and a
  typo silently ignored is a setting the operator believes is in force.
  `allow_routable_bnd: true` happens to fail closed; a mistyped `listen:` would
  bind the default while the file says something else.

This is the choice `workflow.rs` already makes for `.loomux/workflow.yml`, for
the same reason and with the same `deny_unknown_fields`.

**`state_root` defaults to the engine's `obs::data_root()`** — the
`<user data dir>/loomux` root, `LOOMUX_DATA_DIR`-overridable (#394). Defaulting
to anything else would give the daemon a second opinion about where
orchestration state lives, when the whole point of the extraction is that there
is one; on a workstation it also means a daemon sees the groups that are
already there. Nothing in C1a creates or validates the directory: `--check-config`
that made directories as a side effect would be a surprising thing for a check
to do, and the first writer under the root is the honest place for that.

## 5. Exit codes: a skeleton must not look like a running daemon

| code | meaning |
| --- | --- |
| 0 | did what was asked (`--help`, `--version`, `--check-config` on a valid config) |
| 2 | the command line or the config was wrong — **including a refused routable bind**, which is a configuration error, not a runtime failure |
| 3 | the config is fine and a listener would be allowed, but this build has none |

Code 3 is the one worth arguing. The caller of a daemon is usually a service
manager, and exiting 0 having served nothing is indistinguishable from "started
and shut down cleanly" — which is how a skeleton gets deployed, monitored green
and reported as working. A distinct code plus a stderr line naming the slice
that adds the listener makes the absence a fact the operator's tooling can see.
It disappears the moment C2 lands, and nothing should be written against it.

`--check-config` exists so that "resolve everything and exit" can still be a
**success**: that is a legitimate operation (a deployment sanity check) and it
will remain one after C2.

## 6. What this slice does not touch, and why that is not an omission

- **#925 (path-root validation for the remaining caller-supplied identifiers)
  remains a merge blocker for listener code**, and C1a is not listener code:
  nothing here accepts input from a peer. The config file is operator-authored
  local state, in the same trust position as `.loomux/workflow.yml`. C2 is the
  slice that inherits the blocker, and #925 must land before it.
- **No authentication, no roster, no tiers.** H1 defers the first (see §1.3 of
  the protocol note for the accepted risk, stated plainly); the roster and the
  dispatcher that enforces it are C2's, where the commands actually arrive.
- **No `server_version` on the wire yet.** When C2 sends the hello frame, that
  field is the **loomux release version**, not this crate's permanent `0.0.0` —
  `loomux-server` is `publish = false` and stays outside the seven-field version
  lockstep `scripts/check-versions.js` keeps, exactly as `loomux-engine` does.
  C2 must be passed the version rather than reach for `env!("CARGO_PKG_VERSION")`,
  which is the same trap `obs::install_panic_hook` already had to avoid when it
  moved into the engine.
- **No user documentation.** There is nothing a user can usefully run: the
  daemon serves nothing. Deployment documentation is E1's deliverable, and it
  needs H7's hosting facts, which are still the human's to supply.
