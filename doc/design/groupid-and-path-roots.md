# `GroupId` and server-declared path roots

Issue #904. This is layer 2 of the three-layer authorization model in #888's
architecture proposal (§0), landed on the desktop side while it is still a pure
internal refactor with no behavior change for valid input.

## The argument

CLAUDE.md hard constraint 6 **used to say** orchestration commands trust
`group_id` as a path segment; this change is why it no longer does. Read
carefully, that sentence never claimed the id was safe — it claimed the *caller*
was. `OrchRegistry::group_dir` **was** `self.root.join(group)`:
no validation, no canonicalization, and the single membership guard on that path
lived in `save_attachment` alone, added as "cheap hardening on top of the
pre-existing trusted-webview model".

That trust was derived from **process locality**. The only thing that can invoke
a `#[tauri::command]` today is still our own webview, in this process. That is a fact
about the transport, not a credential — nobody issued it, nothing checks it, and
it cannot be revoked. Reproduce the same command surface over a socket, which is
exactly what #888 proposes, and the fact evaporates: every peer that can open a
connection becomes "the webview".

So the fix is not "add a check to the network layer later". It is to stop
deriving the property from the transport at all, and to make it a property of
the **value** instead — one that travels with the id, is established once, and
holds regardless of who called.

That is why this landed early, and separately from #888: as an internal
refactor it is cheap, reviewable in isolation, and independently valuable. Bolted
on next to a new listener it would be a security change reviewed under schedule
pressure, in the same PR as the thing that made it urgent.

## What a group id is

Group ids are loomux-minted tokens. They are never prose, never user input, and
never reach us from anywhere that gets to choose their shape freely:

| source | shape |
| --- | --- |
| `group_id_for_repo` | `{slug}-{8hex}` — slug is ≤24 chars of `[A-Za-z0-9_-]` off the repo directory name, lowercased; hash is FNV-1a folded to 32 bits |
| `create_group_ex` / `promote_orchestrator_cli` | the above, plus `-{n}` for a second concurrent group on one repo |
| `SOLO_GROUP` | the fixed constant `__solo__` |
| the orchestration root's directory listing | whatever is on disk — which is one of the above, or something that should not be there |
| a transcript scrape (`sessions::detect_orch_signature`) | agent-writable; see below |

Because the real alphabet is that narrow, a strict check costs nothing and buys
everything. The rule is:

- non-empty, at most 64 bytes;
- `[A-Za-z0-9_-]` only;
- no leading `-`;
- not a Windows reserved device name.

The alphabet is doing almost all the work, and it does it by *exclusion* rather
than by enumerating attacks. `.` is not in it, so `..` and `.` are unspellable.
`/` and `\` are not in it, so no id names more than one path component. `:` is
not in it, so no drive letter and no NTFS alternate data stream. Control bytes
and whitespace are not in it, so no NUL truncation and no log-line injection.
Non-ASCII is not in it, so Unicode normalization and homoglyph confusion never
arise. There is no blocklist to keep current.

The two rules that are *not* consequences of the alphabet are worth their own
lines. A leading `-` is perfectly legal in a path but is an option to any command
line the id reaches — the same hazard `mergeq::valid_id_component` already
refuses for merge-queue batch ids, which this type's rules are deliberately
modelled on. And a Windows device name (`CON`, `NUL`, `COM3`, …) is a path that
opens a device rather than naming a file; no minted id is ever a bare device
name, so refusing them costs nothing.

### Refused, never rewritten

`GroupId::parse` does not trim, lowercase, strip or sanitize. An id is either
usable exactly as written or it is refused.

This matters more than it looks. Normalizing would let two distinct strings name
one directory — and the moment that is true, a membership check and a path join
can disagree about which group they are talking about, which is the bug class
the check exists to prevent. `mergeq::valid_id_component` makes the same choice
and says so in the same words.

### The minter and the validator must not be able to disagree

A validator that refuses a real group id is not hardening, it is an outage. So
the acceptance half of the test suite is as load-bearing as the refusal half,
and the strongest form of it is a property rather than a list:
`group_id_for_repo` is fed hostile *repo paths* and its output must satisfy
`GroupId::parse` every time.

One shape used to break that property: a repo directory whose name begins with
`-` produced a slug — and therefore an id — that led with a dash. The minter now
strips leading dashes, so it cannot mint something its own validator would
reject. The behavioral delta is confined to repo directories named `-…`, which
get a different state directory than they would have before.

## The declared roots

The second half of #904's title. A group path is not something each call site
derives; it is something the backend *declares*, in one place, from a value that
is already proof.

There were four raw `root.join(group)` sites, plus a fifth interpolation that
does not touch loomux's root at all. None of them survive — every one now routes
through `group_dir_at`, and the source guard keeps it that way:

1. `OrchRegistry::group_dir` — the main one, 78 call sites.
2. `append_audit` — the audit log.
3. `promptsubmit_marker_path` — the hook marker.
4. `sessions.rs`'s group-existence probe.
5. `generated_agent_handle` — `loomux-<group>-<block>`, which becomes a **file
   name** under `~/.claude/agents` and `~/.copilot/agents`.

The fifth is the most dangerous and the least obvious, because it is the only one
whose blast radius is outside loomux's own state directory entirely: a separator
in the group id would not traverse within `%APPDATA%\loomux\orchestration`, it
would write into the user's CLI configuration — and `end_group`'s reclaim then
deletes by the same shape.

**The rest of this section is history, kept because the reasoning is still worth
reading — but it describes the FIRST slice, not the shipped state.** In slice 1
sites 2–5 validated at their own join and failed closed, each in a way suited to
its call site, because a refusal has to go somewhere:

- `append_audit` dropped the record and left a breadcrumb. Auditing is
  best-effort by contract — it must never take the orchestration down — and the
  function returns `()`, so there is no caller with anywhere to put an error.
  The breadcrumb names the `GroupIdError`, never the offending string: an id that
  reached here was chosen by the caller, and echoing it verbatim into a log file
  is how log injection starts.
- `promptsubmit_marker_path` and `generated_agent_handle` returned `None`. Their
  callers already return `Option`, so the refusal was a `?`.
  (`promptsubmit_marker_path` takes a `GroupId` and is infallible again now;
  `generated_agent_handle` still returns `Option` for its `?` ergonomics.)
- `sessions.rs` read a refused id as "no group", which is the same answer it
  already gives for a session with no orchestration signature.

Site 1, `group_dir`, could not be validated locally: its return type is
`PathBuf`, which has no refusal channel, and its 78 callers are spread across the
registry. It is converted by threading the type instead — and that is what the
second slice did. `group_dir` now takes a `&GroupId`; the ~50 `orch_*` commands
parse their `String` once, at the boundary, in `command_group`; and the id
travels as a type from there.

Two consequences worth naming, because both were discovered by the compiler
rather than designed:

- **`AttentionItem.group` went back to `String`.** Its own doc says the field is
  *empty* for a plain, non-orchestration pane — and a `GroupId` cannot be empty.
  That is the type earning its keep: a slot whose empty value is meaningful is a
  display slot, not a group id. It never reaches a path join.
- **`QueuedDelivery.group` became `Option<GroupId>`** — the one field where the
  newtype could not simply replace the `String`, and the one place this design
  had to give ground rather than the code.

  A pre-#468 snapshot has no `group` key, which is what its `#[serde(default)]`
  was for. `GroupId` has no `Default`, so a bare `GroupId` here would make such
  an entry fail to deserialize. The first attempt did exactly that, and argued
  it was an improvement: the entry "was never replayable anyway", so letting
  `read_snapshot`'s `Err(_) => skipped += 1` count it seemed strictly safer.

  That argument was wrong, and the existing suite said so.
  `an_entry_from_an_older_build_parses_but_has_no_durable_identity` pins that a
  legacy entry still **parses**, and its doc comment gives the reason: *"so its
  payload is surfaced as an orphan"*. Failing to parse would have turned a
  recoverable, human-visible payload into an anonymous number — an observability
  regression wearing a safety argument's clothes.

  `Option<GroupId>` says exactly what the empty string used to mean, "no recorded
  identity", but says it in the type: it cannot be confused with a group or
  joined onto a path. Consumers filter with `as_ref() == Some(group)`, so such an
  entry matches nothing and is never replayed into a pane it was not for — which
  is the other half of that test's contract, unchanged.

### Not `AsRef<Path>`, and a test that says so

`GroupId` deliberately does not implement `AsRef<Path>`, so it cannot be passed
to `Path::join` at all. Holding a validated id is not the same as being allowed
to build a path from it wherever you like; a group path comes from
`group_dir_at`, and only from there.

That is half the guarantee. The missing impl stops a `GroupId` reaching a `join`
— it says nothing about the *string* inside one, which would compile fine. So
the other half is `the_orchestration_root_is_joined_with_a_group_in_exactly_one_place`,
a source-scanning test that parses every `.join(` argument under **both**
production source roots — `src-tauri/src` and `crates/loomux-engine/src` — and
flags any naming a group: structural, not a list of spellings, because the
first version *was* a list of spellings and missed one. It enumerates its own
limits rather than claiming completeness: the qualified and bare spellings of
both `AsRef` and `Path` are matched, but an aliased `Path` import, a
macro-generated impl, an impl header split across lines, and `PathBuf::push`
(indistinguishable from `Vec::push`) are all outside a textual scan. None of
them appears in the tree today, and none would be caught if it were the first
one added. That tail is unbounded, and more pattern-matching buys less than it
costs — what holds the property is the compiler: with no `AsRef<Path>` anywhere,
a `GroupId` cannot reach a `join` as a value at all, and the scan is defence in
depth over the *string* inside one.

**Why two roots, and the rule it generalizes to.** `GroupId` itself now lives in
`loomux-engine` (#888 slice A2). Rust's orphan rule means an
`impl AsRef<Path> for GroupId` — a foreign trait on a foreign type — can be
written in exactly one crate, the one that owns the type, so after that move the
only directory the violation can be *spelled* in is `crates/loomux-engine/src`.
A scan left pointing at `src-tauri/src` alone would have passed every run
forever while watching a directory the defect could no longer reach: an inert
tripwire, which is worse than no test, because CLAUDE.md constraint 6 cites this
assertion as the enforcement. The rule for any later move: ask **where the
violation can be spelled now**, not where it used to live. The test guards
against its own staleness too — it asserts per root that files were found, and
that the file *defining* `GroupId` was actually in scope.

A sibling, `every_group_taking_command_parses_its_id_at_the_boundary`, watches
the other end. That one is the more valuable of the two: the sink is a single
function, the sources are fifty, and it was a missing *source* — one command
that never parsed — that survived a whole slice of review.

It exists for a specific reason. **This section's claim was false for the whole
of the first slice** and nothing caught it but a reviewer: `append_audit` and
`promptsubmit_marker_path` are free functions taking a bare `root`, reached from
`deliver_now`, which has no registry — so neither went through
`OrchRegistry::group_dir`, and "and only from there" was prose, not fact. The
fix was to make it true rather than to soften the sentence: `group_dir_at` is now
a free function all four seams delegate to, and the test is what keeps a fifth
from growing back.

`Deref<Target = str>`, `AsRef<str>`, `Borrow<str>` and the `PartialEq` bridges
are implemented, and for a plain reason: they let a `&GroupId` stand in wherever
a `&str` group id stands today, which is what keeps the second half a signature
change rather than a rewrite of every body.

## The root registry

#904 closed the *segment* half of this note's title. This is the *root* half —
#1042, and the mechanism §1.1 H3 of the remote-engine protocol names as the
listener's merge blocker.

The families #904 left alone (`ft_*`, `fm_*`, the 22 `git_*`, the 10 `gh_*`,
`git_watch`, `dir_info`/`change_dir`, and the orchestration boundaries taking a
`repo`) do not take an identifier that becomes a path component. They take an
**absolute root** and check it with `is_dir()`. No segment validator can help
there, and neither can any other predicate over the string: nothing about the
shape of a path separates a repo from `~/.ssh`. The difference is not in the
path — it is in whether anybody ever declared it.

So the answer is server-held state rather than a stronger check. A root is
usable iff it is in a registry, and only sources the engine itself trusts can
put one there.

### It is wire enforcement, not a local sandbox

This is the crux, and it is what keeps the desktop from regressing.

The registry does not exist to constrain the local webview. That webview already
owns the disk — it can open a file picker anywhere — so a rule stopping it from
declaring a root buys nothing and costs real behaviour (a folder chip that goes
dead after an agent `cd`s, a restore that will not restore). Locally, it may
admit anything.

The teeth are in what *cannot* admit. `admit_root` is classified off-roster, so
when the listener's default-deny dispatcher lands, a remote peer can **use**
declared roots and can never **mint** one. Desktop UX therefore survives by
construction, and the enforcement is entirely on the wire — the same shape as
the listener's `ListenTarget`: classify once at a boundary, and make the
unchecked form unrepresentable downstream.

The sources, and the ruling for each, come from a survey of where roots actually
originate — which is emphatically *not* the file picker. A pane's OSC-7 report
is emitted by the process running in that pane, so it is agent-controllable; the
launcher's free-text field and its `localStorage` recents are client state; and
the persisted tabs JSON is replayed on every launch. The organizing rule is that
**a source is an admit path only if it is the local trusted webview acting on
its own state or on a human gesture; everything else is resolve-only.** An
agent that `cd`s to `~/.ssh` gets a quiet folder chip, not a rooted file
browser — while a human clicking "open git view here" on that same pane has
declared something, and that gesture admits.

### Never persisted

The registry is in-memory and rebuilt from its trusted sources on every boot,
and neither type carries a `Serialize` or a `Deserialize`. That absence is
load-bearing rather than an omission.

A registry file would be exactly the replay-poisonable artifact the persisted-tabs
analysis warns about: an entry admitted once — or injected into the file — would
survive every reason it was admitted for, and a client replaying its own tabs
file would be re-asserting authority rather than re-requesting it.
Rebuilt-from-trusted-sources makes replay poisoning structurally impossible at
this layer, which is a stronger guarantee than any validation on the way in.

Note the contrast with `GroupId`, whose `Deserialize` is load-bearing in the
opposite direction: an id has to survive a state file, so the constructor must
guard the way back in. A declared root must not survive one at all.

### The descendant rule

`RootRegistry::resolve` accepts a candidate whose canonical form **equals or is
a descendant of** a declared root, and refuses everything else.

The direction is the whole point. A subdirectory grants strictly *less* than the
root that was declared, so a pane that `cd`s around inside an admitted repo goes
on working with nothing new declared — which is what keeps the OSC-7 consumers
alive without giving an agent-controlled byte stream an admit path. An
**ancestor** grants strictly *more* than was declared, so it is refused, and so
is a sibling whose name merely extends a declared root's (`…/repo-evil` under
`…/repo`) — the reason containment is a component-wise `Path::starts_with` and
never a string prefix.

Refused too: a relative or Windows drive-relative candidate (`C:foo` resolves
against whatever the process happens to have as drive C's current directory,
which nobody declared), and a link whose target leaves every declared root.
Containment is compared canonical-against-canonical, which makes it
symlink-sound in both directions: a link *into* a declared root resolves inside
and grants nothing new; a link *out of* one resolves outside and is refused.

### `plain` and `canonical`, and the `..` that would have split them

`std::fs::canonicalize` on Windows returns an extended-length `\\?\C:\…` path.
That is the right **comparison key** — it resolves links and junctions and
normalizes the case Windows does not care about — and the wrong **working
path**: MSYS git does not want one as a subprocess cwd, and nobody wants to read
one. So a `DeclaredRoot` carries both, and its one accessor `as_path()` hands
back `plain`, the caller's own path lexically normalized the way
`fileedit::safe_resolve` already normalizes a root. Commands keep feeding
git/gh/the filesystem the same shape of path they do today, which is what makes
the eventual enforcement behaviourally silent for an admitted root.

Carrying two forms is only safe if they can never name two different
directories, and one shape makes them: a `..` component. `plain` folds `..`
lexically; Unix resolves it after following symlinks. Those answers diverge
whenever a link precedes the `..`, and they diverge in the direction that
matters — a crafted candidate can canonicalize *inside* a declared root, so the
containment check passes, while its lexical fold lands outside every declared
root and is then what `as_path()` hands to `current_dir`. `resolve` therefore
refuses a `..` outright: a root or cwd argument has no legitimate one, so the
refusal is free, and it is refused rather than folded for the same reason
`GroupId::parse` refuses rather than sanitizes.

(Windows is not exposed to that particular divergence — Win32 normalization
folds `..` lexically before the filesystem sees the path — but the refusal is
uniform. This core is what a Linux daemon links, and a rule that holds only
under one platform's path semantics is one the next reader has to re-derive.)

### Not `AsRef<Path>`, on the same grounds as `GroupId`

A `DeclaredRoot` has no public constructor, no `From<String>`, no
`Deserialize` and no `AsRef<Path>`. The only way to mint one is
`RootRegistry::resolve`, so holding one is proof that some trusted source
declared a root containing it; and `as_path()` being a named method rather than
a blanket impl is what keeps every place a declared root becomes a working path
greppable. Holding one is not membership, exactly as holding a `GroupId` is not.

The two mechanisms compose without overlapping, and it is worth stating as one
sentence: **a validated segment answers "is this string safe to join under a
root?", a declared root answers "is this root yours to use?"**. They meet at
`safe_resolve(root, rel)`, one parameter each, and neither is a second copy of
the other. Group ids are unaffected: they stay identifiers joined at
`group_dir_at`, and the registry receives group-derived *values* only, so the
one-join guarantee above is untouched.

### What has landed, and what has not

Slice A — this section's mechanism — is `crates/loomux-engine/src/rootreg.rs`:
`RootRegistry`, `DeclaredRoot`, `RootError` and their unit tests, in the
Tauri-free core so the daemon can link them. It is std-only and mints no ids, so
CLAUDE.md constraint 2 holds trivially.

**Nothing consumes it yet, and no command refuses anything it did not refuse
before.** The `admit_root` command, the engine-derived registration at group
create/load, and the frontend admit-at-source wiring are slice B; boundary
`resolve` on every root-taking family — with the choke functions changing
signature so the compiler holds the property — is slice C, and it is slice C
that introduces the `root-not-declared` error code to a caller. Until then the
code exists on `RootError` and is returned to nobody.

Deferred past all three, and recorded so the absence is deliberate: a
`roots_list` wire command and a daemon config's seeded roots (there is no wire
to serve them over yet), an authenticated remote admit path (an escape hatch to
be *added* later, never one removed later), and eviction — the registry is
unbounded because a session declares a handful of roots at a few hundred bytes
each, and an eviction rule needs an answer to "is this root still in use by an
open pane, a watcher, a queued delivery?" whose wrong answer is a live
regression.

## The one agent-writable source

Every group id above is loomux-minted or read back from loomux's own state, with
one exception: `sessions::detect_orch_signature` scrapes the id out of an agent
CLI's transcript by looking for the kickoff phrase and taking what follows.

A transcript is a file the CLI writes from the agent's own conversation. Any text
an agent emits can contain that phrase and choose what comes after it. The
scraped id then travels: `Parsed.orch_gid` → the persisted session index →
`SessionInfo.orch_group` → the frontend → back in as
`resume_orch_session(group_hint)`, which joins it onto the orchestration root.

The scrape's `take_while` alphabet was the only thing standing between that loop
and a traversal. It happens to exclude separators, which is why this was never
exploitable — but "happens to" is not a specification, and the charset lives in
a function whose job is text-matching, not path safety. The scrape now runs its
result through `GroupId::parse`, which is the one place that decides what a group
id may be, and the persisted value is re-validated on the way back out as well:
an index entry written by an older build is not evidence of anything.

## Deserialization is a construction site

`GroupId`'s `Deserialize` goes through `parse`. A state file written by an older
build — or edited by hand, or corrupted — cannot hand the process an id the
constructor would have refused. `Serialize` is transparent (a bare string), so no
persisted file and no frontend payload changes shape.

## What this does not do

- It does not authenticate anyone. That is #888 §0 layer 1, and a `GroupId` says
  nothing about whether the caller is entitled to *that* group — only that the
  string is a usable path segment. Membership remains a separate check
  (`save_attachment`'s, and whatever layer 3 adds).
- It does not re-scope the `ft_*`/`fm_*`/git `repo` families, which take
  arbitrary absolute paths and are the other half of §0's "server-declared
  roots". Those are a larger surface with their own trust story — the section
  above is that story, and #1042 slice A has landed its *mechanism*. The
  re-scoping itself is slice C and **has not landed**: as of this sentence those
  families still take any absolute path an `is_dir()` accepts.
- It does not make a group id a *capability*. Membership — "is this caller
  entitled to this group?" — is still a separate check, and only
  `save_attachment` performs one today.

## What it does close

Constraint 6 as originally written — "orchestration commands trust `group_id` as
a path segment, safe only because the webview is trusted" — no longer describes
the code. Nothing about a group path now rests on who called: the id is parsed at
the command boundary, travels as a type, and reaches the filesystem through one
helper that will not accept anything else.

What remains for #888 is the part this was always a *prerequisite* for rather
than a substitute for: authenticating the display client (§0 layer 1) and
re-scoping the path-taking `ft_*`/`fm_*`/git families to server-declared roots.
A validated `GroupId` says the string is a usable path segment. It says nothing
about whether the peer holding it should be talking to that group at all.

The second of those two now has a mechanism — `RootRegistry` (#1042 slice A) —
and does not yet have its enforcement, which is slice C. Read the root
registry's own "What has landed, and what has not" for the line between them
before citing this note as evidence that a root-taking command is scoped.
