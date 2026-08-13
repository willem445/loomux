# SSH panes (#887): a remote solo pane, and the boundary it stops at

An **SSH pane** is a pane whose child process is a local `ssh.exe` connected to a
remote host, optionally launching an agent CLI there. Shipped across five slices:
S1 the profile store (#907), S2 the pure argv builder (#906), S3 the launch flow
(#921), S4 persistence and reconnect (#926), S5 the docs and this note.

The user-facing page is `docs/features/ssh-panes.md`. This note is the *why*: the
decisions that are not obvious from the code, and the arguments that have to
survive the next person who wants to make one more thing work remotely.

## The shape: no backend, no dependency, no protocol

The transport is **the system `ssh.exe`, spawned as an ordinary ConPTY pane
child**, and it needed **zero backend spawn code**. `pty.rs`'s direct-native-exe
path (`try_direct_command`) already spawns a resolved native executable from an
argv with no shell in between; `ssh.exe` is one. The whole feature is therefore a
frontend that composes an argv, plus one command to find the client
(`discover_ssh`) and two to persist connections.

An SSH library (russh, libssh2 bindings) was rejected on three counts, in order of
weight:

1. **Constraint 2.** The pure-Rust options pull `rand`/`getrandom`, which imports
   `bcryptprimitives.dll!ProcessPrng` — absent on this project's Windows 10
   baseline, where the binary then fails to load with `0xc0000139`.
2. **It would move key handling into loomux**, which is precisely the posture
   `gh.rs` exists to avoid (loomux stores no GitHub token; it shells out to the
   user's authenticated `gh`).
3. **It is a permanent dependency for something the OS ships** — and `ssh.exe`
   inherits the user's entire `ssh_config`, agent, `ProxyJump` and known-hosts for
   free, which no library would.

`discover_ssh` resolves **PATH first**, then the inbox OpenSSH directory under
`%SystemRoot%` — both `System32\OpenSSH` and `Sysnative\OpenSSH`, the same
directory under the name a 32-bit process must use (for which `System32` is
redirected to `SysWOW64`, which has no OpenSSH). One extra `is_file` on a path
that simply doesn't exist for a 64-bit process, and the difference between "found"
and "not installed" for one that isn't. PATH winning is not an ordering accident: a user who installed a newer
OpenSSH or put a wrapper ahead of the inbox client has already said which `ssh`
they mean, and every other program on the machine honours that. The candidate list
exists for the stripped-PATH case, and it is derived from `SystemRoot` rather than
hardcoding `C:\Windows` (constraint 8 — no assumptions about this machine).
Resolution happens **once**, in the launcher, and the absolute path is what is
spawned: a bare `"ssh"` would be re-resolved at spawn time against a different
PATH snapshot and could silently be a different binary.

## No credentials, and what makes that structural

`sshprofiles.json` (a sibling of `tabs.json`/`settings.json` under the loomux data
root, written through `uistate.rs`'s existing atomic-write + corrupt-quarantine
helpers, opaque to the backend) stores hostnames, ports, an identity-file **path**,
a remote directory, a CLI name and extra ssh flags. loomux never writes key
material into it.

Stating that would be worth little; two guards make it hold — and the second has a
residual, stated with it rather than after it:

- **Both directions are allowlists.** `normalizeSshProfile` reads only declared
  fields and `profileToWire` writes only declared fields, so a `password` key
  hand-added to the file — or hung on a profile object by some future caller —
  cannot survive one load/save cycle.
- **`identityFile` is checked to be a path**, because it is the one field through
  which key material could enter by the front door. A value carrying a line break
  (which every real PEM/OpenSSH key body has) or a `-----BEGIN` header is refused,
  and the profile survives without `-i`. What the guard is *not* is a content
  classifier: a single-line base64 key body with no armour is indistinguishable
  from a path by shape and passes — and is then handed to `ssh -i` as a filename
  that does not exist, which fails loudly rather than storing anything.

Encode routes through the same normalizer decode uses, so there is exactly one
implementation of every field guard rather than a write-side copy that drifts. And
a save carries the file's **own** `schemaVersion` through rather than re-stamping
it: an older build editing a newer file must not silently re-label it as v1.

### The schema, as a public contract

A file on the user's disk that older and newer builds both read, and that the user
is invited to hand-edit — so the shape is a contract, not an implementation
detail. `{schemaVersion: 1, profiles: [...]}`, each profile:

| Key | Required | Meaning when absent |
| --- | --- | --- |
| `id` | yes | — (entry dropped; it is what a persisted pane records) |
| `name` | yes | — (entry dropped) |
| `destination` | yes | — (entry dropped; `user@host`, `host`, or an `ssh_config` alias) |
| `remoteShell` | always written | defaults to `"posix"` on read |
| `port` | no | loomux passes no `-p` |
| `identityFile` | no | loomux passes no `-i` |
| `remoteCwd` | no | the remote login directory |
| `defaultCli` | no | a plain remote login shell |
| `keepaliveSeconds` | no | loomux emits no `ServerAliveInterval` at all |
| `extraArgs` | no | no extra ssh flags |

Two conventions carry the no-credentials posture into the file's *shape*: an unset
optional field is **omitted**, never written as `null`, because "loomux passes
nothing and your ssh_config decides" should look like absence in a file a human
reads; and `keepaliveSeconds` refuses `0` rather than accepting it as a second
spelling of "disabled" (absence already means that, and one meaning spelled two
ways is how a user ends up believing they enabled what they disabled).

Only `id`, `name` and `destination` can fail an entry — without any one of them
there is nothing to show, nothing to point a pane at, or nothing to connect to.
Every other field degrades to its unset value on its own, so one bad key costs a
setting rather than a connection, and one bad entry costs an entry rather than the
list.

### NB4 — `remoteCwd`, `defaultCli` and `extraArgs` are unsanitised, and why that is accepted

Carried forward from #907's review, because the honest statement of the trust
boundary belongs beside the guards rather than implicit two paragraphs away.

S1 guards `destination` (leading dash on the whole word *and* on the host half
after the last `@` — the `%h`-expansion shape of CVE-2023-51385) and
`identityFile` (above). **Everything else is trimmed-or-nulled and passes through
verbatim.** Precisely, as S2's builder consumes them:

| Field | How it reaches `ssh` | Containment |
| --- | --- | --- |
| `remoteCwd` | Inside the single remote-command **string**, as the argument of `cd` | Quoted for the declared `remoteShell` — `posixQuote` (single quotes, provably safe) or `cmdQuoteCwd` (double-quote doubling; refuses a newline, which would truncate a `/C` line). May contain `;`, backticks, `$(…)` — the quoting is the containment, not a filter. |
| `defaultCli` | `remoteCommand[0]`, quoted the same way | One quoted argv token in the remote command, never a command line of its own. Not validated against loomux's catalog — an unknown name **warns and runs** (a profile naming a CLI this build doesn't know is a profile to warn about, not one to silently delete). |
| `extraArgs` | **Raw argv words handed to the local `ssh`**, before the `--` separator | *None.* This is real `ssh` option surface: `-oProxyCommand=…` runs a command on the **local** machine. Deliberately unfiltered. |

The trust boundary that makes that acceptable, stated as a claim that can be
checked rather than a reassurance:

1. **These values are the user's own, on the user's own machine.** They are typed
   into the launcher by the human at the keyboard, or hand-edited into a file in
   their own `%APPDATA%`. Anyone who can write that file can already do worse to
   that user than run a command as them.
2. **They are exactly what `~/.ssh/config` already grants.** `ProxyCommand`,
   `LocalCommand`, `PermitLocalCommand` are all one line in a file the same user
   owns. A filter over `extraArgs` would be theatre against an attacker who has
   the easier route open, while breaking legitimate flags.
3. **No agent can reach them.** Agents get MCP tools and a pane to type into;
   neither exposes SSH profiles, the group spawn surfaces never offer one, and
   `sshOrchestrationRefusal` (below) refuses the combination outright. The values
   are never attacker-controlled *through loomux* — which is the property that
   would change the answer, and the one to re-check before wiring any new caller
   into this store.
4. **They never traverse a local shell.** The pane spawns from an argv through the
   direct-native-exe path. Even the fallback is safe by omission: if the direct
   spawn is disabled or fails, `spawn_pane_child` drops to a plain interactive
   shell — an SSH pane passes no `command` string, so its argv is never
   re-interpreted as a command line. There is no path on which these values become
   local shell input.
5. **The webview is the only writer.** Constraint 5's seam means the frontend
   reaches the store through two typed commands and the backend never parses the
   schema, so this is local-user surface end to end — the same "trusted because
   the caller is our own in-process webview, not a network client" reading
   constraint 6 makes explicit for `group_id`.

What would invalidate this argument, so it is not re-derived from scratch: any
change that lets a **non-human** author a profile, or that hands one to a process
loomux did not spawn for a human at the keyboard. Then `extraArgs` becomes remote
code execution on the local box, and the answer is a capability boundary, not a
filter.

## Who owns `RemoteShell`

**`sshprofile.ts` (S1) owns the value set.** It declares the canonical triple —
the `RemoteShell` union, the `REMOTE_SHELLS` list the launcher's picker is built
from, and `DEFAULT_REMOTE_SHELL` — validates it on the way in and out of disk, and
is what the UI imports. `sshcommand.ts` (S2) declares a **structurally identical
union of its own**, deliberately: S2 takes flat primitives and never imports S1, so
the two slices could land from parallel worktrees. Nothing in TypeScript notices
that they are two declarations, because two identical string unions type-check
happily against each other — which is exactly why ownership has to be *written
down* rather than inferred.

The drift is not left to a human to catch, though — and it is worth being exact
about *which* mechanism catches it, because there are two and they fire at
different times.

**The build-time catch is `tsc`, at the S1→S2 seam.** The two unions meet in
exactly one place: `sshLaunchParams` (`panesetup.ts`) assigns
`remoteShell: profile.remoteShell` — S1's type — into an `SshCommandParams` —
S2's type. Grow S1's set by a third member that S2 doesn't declare and that
assignment stops type-checking, so `npm run build`'s `tsc --noEmit` fails before
anything runs. That is the real guarantee behind the paragraph above, and it is
why the seam being a *single* assignment matters.

**`buildRemoteCommand`'s `default:` arm is a runtime backstop, not that catch.**
It throws on any value outside `"posix" | "cmd"` — deliberately as a runtime test
(it casts, `remoteShell as string`, rather than using the `never`-exhaustiveness
idiom that would be a compile-time check), because it is guarding against a
*caller* that reaches the builder without S1's normalizer. **No shipped path can
reach it.** Both the launch path (`planPaneSetup` → `normalizeSshProfile`) and the
reconnect path (`decodeSshProfiles` → `normalizeSshProfile`) coerce an
unrecognized `remoteShell` to `DEFAULT_REMOTE_SHELL` first. So hand-editing
`"remoteShell": "powershell"` into `sshprofiles.json` does **not** produce a loud
refusal: it silently reads as `"posix"`, which is the store's intended
unrecognized-value degradation, not a hole. The arm exists for the next caller,
not for the file.

That third member is a real prospect, and the naming reflects it: the value is
`"cmd"`, meaning **cmd.exe specifically** — not "a Windows host". A spelling of
`"windows"` would have been a promise the schema cannot keep, because a
PowerShell-`DefaultShell` remote expands `$(…)` inside double quotes and is a
strictly worse surface than the one cmd.exe quoting was written for. PowerShell
remotes are unsupported in v1 and reachable as a plain login shell; naming
cmd.exe's own case is what lets a later slice add PowerShell without redefining a
value users already have on disk.

## The launch seam, and the two rules it taught

S3's review (#921) produced two findings whose lesson generalizes past this
feature. Both are recorded here rather than as repo lore, because both are
arguments about *this* code that a future edit could quietly undo.

### Symmetric input read: a guardrail must read every input by one rule

`sshOrchestrationRefusal(opts, pane)` is the #887/#888 boundary in code. Its first
shipped form read **ssh-ness from both** the spawn options and the pane's existing
state, but the **orchestration identity from `opts` only**. That asymmetry is
exactly the width of a bypass: `respawnFresh({ ssh: {...} })` on a pane that is
*already* an orchestration member carries its group on the pane and nothing in the
options — so an opts-only read of the identity waves it through and produces the
combination the guard exists to refuse, with the merge gate unenforced for its
children.

It was not reachable at the time, and that is the point worth keeping: a guard
whose stated job is to survive future edits cannot be justified by today's call
sites, and S4 was about to add a reconnect path that calls `respawnFresh` with
`opts.ssh`.

What shipped: **two same-shaped inputs, unioned field by field**.

```ts
const ssh = !!opts.ssh || !!pane.ssh;
if (!ssh) return null;
const identity =
  opts.orchGroup || pane.orchGroup ||
  opts.orchRole  || pane.orchRole  ||
  opts.orchAgent || pane.orchAgent;
```

Three properties, each deliberate:

- **Neither side is authoritative alone**, so neither is trusted alone. It refuses
  on *any* ssh signal crossed with *any* orchestration marker, from either side —
  fail-closed, including a spawn carrying only half an identity on only one side.
- **The union happens in the pure module**, not at the two DOM call sites in
  `pane.ts`. A rule spelled at two call sites is a rule that drifts at one of them;
  here it lives in the unit-tested function, and the call sites just hand it what
  they know.
- **It cannot over-refuse.** With no ssh signal it returns null before reading the
  identity at all, so no existing orchestration flow changes behaviour.

Coverage sits in two tests, not one: *the guardrail reads BOTH sides by the same
rule* enumerates all four crossings of {which side says ssh} × {which side says
orchestrated} and adds `orchRole`/`orchAgent` on the pane side, while *…and the
guardrail refuses ONLY that combination* holds the negative controls (an ordinary
orchestration pane; an ordinary ssh pane; orchestration on both sides with no ssh
→ null) so "refuse everything" would not pass.

The same rule is what makes the restore path safe by construction rather than by
vigilance: the `dormant-ssh` action has **no field** that could carry `role`,
`groupId` or an agent id, even though the persisted leaf it is built from has room
for all three. An `ssh` leaf hand-edited into `tabs.json` claiming `role: "worker"`
restores as an ordinary dormant SSH card, because there is nothing to carry the
claim through.

### No silent data loss: refuse it or honour it, using the mechanism that already exists

The launch form accepted values the launch would then drop. Type `99999` into
Port: `normalizeSshProfile`'s bounds drop it, the pane connects on port 22, and
nothing says so. Worse for `identityFile`, where a rejected value means connecting
with no `-i` at all and the failure surfaces as an unexplained auth problem.

Normalization dropping those values is **right** — they are the store's own guards.
The defect is dropping them *quietly*.

What shipped, per field:

| Field | Answer | Where |
| --- | --- | --- |
| `port`, `keepaliveSeconds`, `identityFile` | **Refuse at the launch seam**, naming the field and the range | `sshDiscardedFieldError`, called from `planPaneSetup` |
| `remoteCwd` with no remote CLI | **Keep and warn** — the value stays on the saved connection and the form says it won't apply | `sshRemoteCwdWarning` |
| `defaultCli` loomux doesn't know | **Keep and warn** — it runs on the far host exactly as written, with no session id and no autopilot flags | `sshRemoteCliWarning` |

The refusal is one mechanism, not a second one: it **asks the store's normalizer
what it kept** and refuses the difference, rather than re-spelling the bounds at
the seam. `MIN_SSH_PORT`/`MAX_SSH_PORT`/`MIN_KEEPALIVE_SECONDS`/
`MAX_KEEPALIVE_SECONDS` moved into `sshprofile.ts` so the input attributes, the
refusal text and `boundedInt` all name one range. A bound spelled three times is a
bound that ends up meaning three things.

Two consequences worth stating because they are what makes it safe:

- **A saved connection can never be bounced by this.** Values off disk were
  normalized on the way in, so raw and kept agree and nothing is refused. The
  regression control for that is its own test.
- **The launched profile *is* the saved profile.** The seam runs the store's
  normalizer over the form's raw object, so the connection a pane launches and the
  connection written to `sshprofiles.json` are the same object — an out-of-range
  port or a pasted key is dropped from both or from neither.

`remoteCwd` is the case where "honour it" was the wrong answer, and the reason
generalizes: honouring it with no remote CLI would mean synthesizing
`cd … && exec $SHELL -l`, which is a **guess** about the remote's login shell —
the exact class of guess `remoteShell` exists to refuse. Refusing to *save* it
would throw away a setting that becomes correct the moment a CLI is picked. So it
is kept, and the human is told. Not silent, not lost, not guessed at.

## Restore: the leaf records a connection, not a command line

The full argument lives in `doc/design/session-restore.md`'s #887 S4 section; the
two decisions worth repeating here are the policy and the forward-compat story.

**Dormant-with-Reconnect, never auto-connect.** Two independent reasons, neither
of which applies to a local shell: the far end is an agent on someone else's
machine, so an auto-reconnect spends **remote** credits with no human present (the
orch-pane credit argument, one host removed); and a host that is down, asleep or
behind a VPN puts a TCP connect — which may not fail for a minute — on the boot
path. Autossh-style automatic reconnection was rejected for a third reason that
applies mid-session too: a surprise reconnect re-enters a remote TUI in a state
nobody has looked at.

**The record's meaningful content is `{paneKind: "ssh", name, sshProfileId,
sessionId}`** — no `cwd` (there is no meaningful local one) and deliberately **no
argv**. That is the non-null *subset*, not the literal JSON: `Pane.capture()`
returns a full `PersistedPane` and `encodeTabs` stringifies it wholesale, so the
bytes on disk also carry `"cwd":null,"command":null,"argv":null,"shellKind":null,
"role":null,"groupId":null,"file":null`. Worth the clause because the file is
hand-editable and this note is presenting a shape: those keys are present and
empty, not absent. "No `cwd`, no argv" is a statement about the **value**. Reconnect
re-derives everything through the same builders a fresh launch uses, which is what
makes an edit between boots apply, and what avoids re-parsing a quoting scheme
`sshcommand.ts` exists to be the sole implementation of. A deleted profile
therefore reconnects with *nothing* — the click refuses with `SSH_PROFILE_GONE` —
rather than replaying a stale command line into a host the human removed on
purpose.

That refusal is **click-time**, and the distinction follows from what the card can
know without doing I/O at boot. Its `initial` error state is gated on the *record*
having no `profileId` at all, which is a different state from a profile that has
since been deleted: the record still names its connection, so the card cannot know
at mount whether that connection exists without reading the store, and it reads
the store on the click anyway — as it must, since the whole point is that the
profile is re-read at reconnect time rather than captured. A record that never had
a connection needs no read to know it has nothing, so it says so up front.

### Schema forward-compat: `SCHEMA_VERSION` stays at 2

The `ssh` leaf adds one field (`sshProfileId`) and one kind value, both additive,
and `tabs.json` decode is shape-driven — so **`SCHEMA_VERSION` stays at 2**, for
the same reason the content kinds left it there: a v2 file written before this
build simply never carries an `ssh` leaf and decodes exactly as it always did.

The **downgrade** direction is the one that costs something, and it costs more
here than a per-entry drop, which is why it is recorded rather than softened:

- a **docked** `ssh` pane is the soft case — an older build drops that entry and
  keeps everything else;
- a **tiled** `ssh` pane is the sharp one — an older build's `decodePane` rejects
  the unknown kind, and `decodeLayout`'s whole-tree fail-safe then collapses
  **that tab's entire layout** to `null`, so the tab comes back as **one empty
  pane on the welcome surface** (`main.ts`'s empty-tab fill; nothing spawns until
  the human picks a kind) — so that tab's *other* panes are lost with it, which is
  what makes this the sharp case rather than merely an untidy one.

Accepted, because the alternative is worse: persisting an SSH pane under a kind an
old build *does* recognize means an old build spawning the wrong process under the
right title. Losing a layout is recoverable and visible; a terminal pretending to
be a remote agent session is neither.

`sshprofiles.json` carries its own independent `schemaVersion` (v1) and has no
such problem: an unknown-version file still decodes field by field, and an entry
an older build cannot make sense of is dropped alone.

## The #888 boundary, and how to tell whether a follow-up crosses it

**SSH panes are display-only in v1: a remote solo pane the human drives.** They
are not orchestration members, and the refusal is enforced rather than documented
(above).

The concrete failures behind that line, none of which degrade gracefully:

- **worktrees** are local directories made by local `git` against a repo that is
  on the other machine;
- **the MCP server** is loopback-only and its per-agent config reaches only
  children loomux spawns itself, so a remote agent cannot `report` at all;
- **the `gh` shim** — the thing that *enforces* the merge gate — likewise reaches
  only locally-spawned children, so a remote `gh` would run with **no gate**. That
  is a security regression, and it is why this is a refusal and not a best-effort
  degradation;
- **`gh` auth** on the far host is unknown to loomux;
- **session identification, transcripts and usage** are local-store scans;
- **briefs** written by a local orchestrator name local paths.

There is also a timing argument that would bite even if all of the above were
solved. Prompt delivery is byte-stream machinery — bracketed paste, the submit
sequence, stranded/stuck detection — and it *mechanically* survives ssh, because
the channel is transparent. What breaks is that its constants assume local echo: on
a 200–400 ms-RTT link the submit-confirm window misses submits that actually
landed, the retry then presses Enter into a pane whose first Enter already went
through, and **a prompt gets submitted twice**. Kickoff delivery-id dedup catches a
duplicated kickoff; an arbitrary prompt has no such guard. v1's answer is
structural — that machinery never targets an SSH pane, because an SSH pane is never
a group member — and a human typing into the pane is their own confirmation loop.

**The test for a follow-up:** does it need a loomux process, or loomux-owned state,
on the remote host? Remote sessions in the browser, remote usage readback, remote
transcripts, remote group members, a tunnelled MCP server, RTT-scaled delivery
timing for a remote worker — every one of them does. They are **#888** (the remote
engine), not this issue. The docs page states the boundary in one sentence, the
guardrail and its test hold the line in code, and this paragraph is the reason both
exist: "make *X* work remotely" is the shape of scope creep this feature attracts.

## Test strategy: the ssh side is faked, and the live half is the human's

Everything SSH-specific is **pure and unit-tested**: the profile schema and its
guards, the argv builder and its two quoting schemes (with adversarial cases — a
hostile quote, a trailing backslash, the `cmd /C` leading-quote strip), the
fresh-vs-resume rewrite, the launch seam's refusals and warnings, the restore
policy, and the orchestration guardrail. `buildSshArgv`'s `program` parameter is
the **fake-ssh seam**: a test (or a hand validation) substitutes a local stub for
`ssh.exe`, the same way `src-tauri/tests/` fakes agent CLIs. No sshd, no network,
no credits.

A real loopback sshd rig was considered and rejected: Windows CI has no OpenSSH
Server enabled, enabling it is privileged machine setup (constraint 8), and every
property it would add — auth, crypto, the wire — is OpenSSH's to test, not ours.

So **live validation is the human's**, per repo convention and constraint 3: a real
host, a real prompt, a real drop. The checklist is in S5's PR body — connect and
authenticate, the connection saved with no credential, a remote Claude Code landing
in the remote folder, the degradation showing up as degradation, the no-client
refusal, and a mid-session drop reconnecting into the same remote session.
