# `GroupId` and server-declared path roots

Issue #904. This is layer 2 of the three-layer authorization model in #888's
architecture proposal (§0), landed on the desktop side while it is still a pure
internal refactor with no behavior change for valid input.

## The argument

CLAUDE.md hard constraint 6 says orchestration commands **trust `group_id` as a
path segment**. Read carefully, that sentence never claimed the id was safe — it
claimed the *caller* was. `OrchRegistry::group_dir` is `self.root.join(group)`:
no validation, no canonicalization, and the single membership guard on that path
lives in `save_attachment` alone, added as "cheap hardening on top of the
pre-existing trusted-webview model".

The trust is derived from **process locality**. The only thing that can invoke a
`#[tauri::command]` today is our own webview, in this process. That is a fact
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
does not touch loomux's root at all:

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

Sites 2–5 are validated at the join and fail closed. What "closed" means is
chosen per site rather than uniformly, because a refusal has to go somewhere:

- `append_audit` drops the record and leaves a breadcrumb. Auditing is
  best-effort by contract — it must never take the orchestration down — and the
  function returns `()`, so there is no caller with anywhere to put an error.
  The breadcrumb names the `GroupIdError`, never the offending string: an id that
  reached here was chosen by the caller, and echoing it verbatim into a log file
  is how log injection starts.
- `promptsubmit_marker_path` and `generated_agent_handle` return `None`. Their
  callers already return `Option`, so the refusal is a `?`.
- `sessions.rs` reads a refused id as "no group", which is the same answer it
  already gives for a session with no orchestration signature.

Site 1, `group_dir`, is the one that cannot be validated locally: its return type
is `PathBuf`, which has no refusal channel, and its 78 callers are spread across
the registry. It is converted by threading the type instead — `group_dir` takes
`&GroupId`, and the id is parsed once at the command boundary — which is the
second half of #904 and lands separately. **Until that lands, constraint 6 still
holds for `group_dir`**: the majority of group-scoped paths are still assembled
from an id the process trusts because of who called it.

### Not `AsRef<Path>`

`GroupId` deliberately does not implement `AsRef<Path>`, so it cannot be passed
to `Path::join` directly. Holding a validated id is not the same as being allowed
to build a path from it wherever you like; a group path comes from the declared
root helper, and only from there. The type makes the shortcut inexpressible
rather than merely discouraged.

`Deref<Target = str>`, `AsRef<str>`, `Borrow<str>` and the `PartialEq` bridges
are implemented, and for a plain reason: they let a `&GroupId` stand in wherever
a `&str` group id stands today, which is what keeps the second half a signature
change rather than a rewrite of every body.

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
  roots". Those are a larger surface with their own trust story.
- It does not close constraint 6. `group_dir` is still `root.join(<trusted
  string>)` until #904's second half threads the type through the command
  surface.
