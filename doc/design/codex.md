# Codex CLI as a session source

Started by #2515 slice **C2**, which taught orrerix to read Codex's own rollout
store: the by-id lookup behind a resume, the Sessions-tab rows, and the frontend
identity that makes a codex pane adopt a codex session.

**This file is not finished.** C2 owns the sections below. The launch line, the
profile file orrerix writes, containment, the store watcher and the usage meter
are #2515's C1 and C3, and each adds its own section here. What is written now is
written because C2 depends on it; nothing here is a placeholder for a decision
somebody else will take.

## Pins

Every Codex fact in this file is read out of `openai/codex` at tag
**`rust-v0.153.4`** (`042fb41b7c813ac7999105e886b2b7aa715b5081`), blob by blob
through the GitHub blob API, and is quoted rather than paraphrased wherever the
quote is short enough to be checkable. Paths below are relative to `codex-rs/`.

**No `codex` process was ever run as an agent** (CLAUDE.md constraint 3). Four
commands were run, and only these: `codex --version`, `codex --help`,
`codex resume --help`, `codex migrate-rollouts --help`. Their output is quoted
where it is used.

One vendor behaviour is pinned here rather than only cited in place, because
nothing on this side can detect its loss: **compression preserves the rollout's
mtime** (`rollout/src/compression.rs:105`, `:746`), and every row's sort
position depends on it. See §Compression preserves the mtime below.

The machine this was written on has `codex-cli 0.153.4` at
`%LOCALAPPDATA%\Programs\OpenAI\Codex\bin\codex.exe` — the desktop app's bundled
binary, already on `PATH` — and a `~/.codex` with **no `sessions/` directory at
all**: Codex is installed and logged in there but has never run a session. So
every fact below is pinned from the vendor's source, and none of it is pinned
from that store. A live check against a real rollout tree is still owed.

## The store

    $CODEX_HOME/sessions/<YYYY>/<MM>/<DD>/rollout-<ts>-<thread id>.jsonl

`SESSIONS_SUBDIR = "sessions"` (`rollout/src/lib.rs`), and the tree is built by
`precompute_new_rollout_path` (`rollout/src/recorder.rs`):

```rust
// Resolve ~/.codex/sessions/YYYY/MM/DD path.
let timestamp = OffsetDateTime::now_local()
    .map_err(|e| IoError::other(format!("failed to get local time: {e}")))?;
let mut dir = config.codex_home().to_path_buf();
dir.push(SESSIONS_SUBDIR);
dir.push(timestamp.year().to_string());
dir.push(format!("{:02}", u8::from(timestamp.month())));
dir.push(format!("{:02}", timestamp.day()));
```

### `CODEX_HOME` names the HOME, not the sessions directory

`find_codex_home` (`utils/home-dir/src/lib.rs`):

```rust
let codex_home_env = std::env::var("CODEX_HOME")
    .ok()
    .filter(|val| !val.is_empty());
```

falling back to `~/.codex`. So orrerix appends `sessions` in **both** branches —
the difference from pi, whose `PI_CODING_AGENT_SESSION_DIR` names the sessions
directory itself.

Two consequences orrerix mirrors rather than improves on:

- **Empty is unset; whitespace is not.** The vendor filters on `is_empty()`,
  never `trim()`, so a value of one space is a real (and hopeless) path to Codex
  itself. `codex_sessions_root_from` does the same. Answering "unset" for a
  whitespace value would be orrerix disagreeing with the tool it is reading
  after, and it costs nothing to agree: a whitespace path is not a directory, and
  a root that does not exist is already "no store".
- **A `CODEX_HOME` that is not a directory yields no store, never a fallback.**
  The vendor hard-errors (`"CODEX_HOME points to {val:?}, but that path is not a
  directory"`). `find_session_cwd` cannot error — it reserves `Err` for a store
  that exists and cannot be listed — so it answers "nothing found". What it must
  never do is silently read `~/.codex` instead: that is a *different* store from
  the one Codex would refuse to run against, and rows out of it would be about
  sessions the configured store does not contain.

### The dates are LOCAL

`OffsetDateTime::now_local()`, above. So "today" has two answers either side of
local midnight, and a UTC-derived guess is wrong for part of every day.

Orrerix therefore **never computes a date**. `walk_codex_session_files`
enumerates whatever year/month/day directories exist, three levels deep and no
deeper, and asks nothing about their names. That is not laziness about parsing —
it is the only way to be right about a boundary the vendor draws in a timezone
orrerix does not know.

### The file name carries TWO ids

`RolloutFileName::render` (`rollout/src/rollout_file_name.rs`):

```rust
Ok(if self.thread_id == self.rollout_id {
    format!("rollout-{timestamp}-{}.jsonl", self.thread_id)
} else {
    format!(
        "rollout-{timestamp}-{}_{}.jsonl",
        self.thread_id, self.rollout_id
    )
})
```

and the struct's own doc: *"Ordinary rollout filenames encode one ID, which is
both the thread ID and rollout ID. Filenames for reverted threads append an
underscore and a distinct rollout ID after the stable thread ID."*

**The trailing half is a second UUID, not a sequence number.** #2515's slice plan
described the second form as `-{uuid}_{n}`; it is not, and the correction is
recorded on the issue. It matters because it rules out a fixed-width read: the
thread id is everything from offset 20 of the name's core up to the **first**
`_`, or to the end when there is none — which is what `RolloutFileName::parse`
does:

```rust
let core = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
let timestamp = core.get(..19)?;
if core.get(19..20)? != "-" {
    return None;
}
let ids = core.get(20..)?;
let (thread_id, rollout_id) = ids.split_once('_').unwrap_or((ids, ids));
```

`codex resume <id>` names the **thread** — *"Session id (UUID) or session name.
UUIDs take precedence if it parses"* — so the leading half is the one a lookup
compares against, and a reverted thread keeps answering for its own id.

Orrerix is deliberately **looser than the vendor about the timestamp**: it
requires the `-` at offset 19 but does not require `core[..19]` to parse as a
date. Requiring that would drop a file whose name is otherwise well-formed
because its timestamp is odd, which fails toward *hiding* a real session. For a
browser, and for a lookup settled by equality anyway, failing toward listing is
the right direction.

### A rollout older than a week is compressed in place

This is the fact that changes behaviour, and the one a `*.jsonl` walk gets
silently wrong. `rollout/src/compression.rs`:

```rust
/// Starts a best-effort background job that compresses cold local rollout files.
///
/// The worker is fire-and-forget: failures are logged, startup is not blocked,
/// and a run marker under `codex_home` prevents overlapping or too-frequent
/// compression runs from the same local store.
pub fn spawn_rollout_compression_worker(codex_home: PathBuf) {
```

```rust
const MIN_ROLLOUT_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
```

It rewrites `<name>.jsonl` to `<name>.jsonl.zst`, and the vendor's own name
parser strips that suffix before deciding anything:

```rust
const COMPRESSED_SUFFIX: &str = ".zst";

pub(super) fn parse_rollout_file_name(name: &str) -> Option<&str> {
    let name = name.strip_suffix(COMPRESSED_SUFFIX).unwrap_or(name);
    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
        Some(name)
    } else {
```

A `*.jsonl`-only walk therefore lists only the last seven days of sessions, and
`find_session_cwd("codex", id)` answers `Ok(None)` — *"no such session"* — for
every older one. That is exactly the failure class this dispatch exists to fix
(probe the wrong shape; the miss reads as absent), reproduced one file extension
over, and it arrives on a schedule rather than by chance: seven days after a
human's first Codex session.

**What orrerix does, with no new dependency.** The walk accepts both
representations. The id is matched off the canonical plain name, `.zst` stripped
first, so a compressed session is **found**. When both files exist for one
session — compression publishes by writing the `.zst` and then removing the
`.jsonl`, so a window exists where they overlap — the **plain sibling wins**,
mirroring the vendor's own `should_skip_compressed_sibling`; without that the
browser shows one session twice.

**Nothing is decompressed.** Reading a zstd header line would mean a new
`src-tauri` dependency and its getrandom audit (constraint 2) for one line of
metadata. So a compressed rollout lands on the answers this module already has
for a file that exists and does not say: `Ok(Some(""))` from the by-id lookup,
and a browser row whose title is `(no prompt)` and whose workspace is blank.

### Compression preserves the mtime, and the row order rests on that

A pin rather than a note, because its loss would be **silent** (review round 1,
W1). Every codex row's sort position and its survival past `LIST_LIMIT` is the
rollout file's mtime: `candidate_meta` reads it, then `scan_sessions` sorts
newest-first and truncates to 300. Compression **rewrites the file**, so that
mtime survives only because codex deliberately restores it —
`rollout/src/compression.rs`:

```rust
output.set_times(std::fs::FileTimes::new().set_modified(metadata.modified()?))?;   // :105
file.set_times(FileTimes::new().set_modified(modified))?;                          // :746
```

Verified holding at `rust-v0.153.4`. If a future codex stopped restoring it,
every session older than a week would acquire a fresh mtime, sort to the TOP of
the Sessions tab, and push genuinely recent rows out of the 300 — with no error
and no red test anywhere, because nothing on this side reads the vendor's
intent. That is why it is written down: the next reader needs to know the order
rests on someone else's code.

**Residual, stated plainly:** a compressed rollout lists with no title and no
folder.
folder. That is strictly better than the alternative — not listing a week-old
session at all — and it is pinned by a test
(`a_compressed_rollout_lists_with_an_unknown_workspace_rather_than_vanishing`)
rather than only asserted here. If it ever stops being acceptable, the fix is a
zstd dependency and the audit that comes with it, not a wider walk.

### The header line

`SessionMeta` written through `RolloutItemWire` (`history/src/rollout_payload.rs`,
`#[serde(tag = "type", rename_all = "snake_case")]`), so on disk the first line
is:

```json
{"timestamp":"…","type":"session_meta","payload":{"session_id":"…","id":"…","cwd":"…","originator":"…","cli_version":"…"}}
```

`payload.id` is the recorder's `conversation_id` — the thread id, the same value
the file name carries and the same one a resume is matched against. `payload.cwd`
is where the session ran.

**The file name proposes and the header disposes.** The name is matched first
because it is free; a name that matches is then confirmed against `payload.id`
whenever the header can be read, and a header naming a different thread
disqualifies the file outright rather than letting the name stand. That is what
stops a hand-copied or renamed rollout answering for a session it does not
contain.

**Residual, and it is the price of the cheap half:** a file whose *name* does not
match is never opened, so a rollout renamed away from its own thread id is not
found even though its header would say so. Closing it would mean one bounded read
per session ever recorded, on every miss, for a case only hand-editing the
vendor's store produces. pi's half makes the same trade for the same reason.

### A conversation turn

Codex wraps everything as `{"type":…,"payload":…}`. A user turn is a
`response_item` whose payload is `ResponseItem::Message`:

```json
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"…"}]}}
```

The block type is **`input_text`**, and this is the one place Codex's format
diverges from both of the formats orrerix's shared `content_text` serves. Codex's
`ContentItem` (`protocol/src/models.rs`) has no `text` variant at all — a user
turn's blocks are `input_text`, an assistant's `output_text`:

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText { text: String },
    InputImage { … },
    InputAudio { … },
    OutputText { text: String },
}
```

So `scan_codex_jsonl` has its own six-line reader rather than widening the shared
one: widening it would change what claude and pi rows title themselves with in
order to fix a Codex problem. A scanner that reused it would title every Codex
row `(no prompt)` — a green suite and an empty-looking browser, which is why
there is a test that feeds a `text` block to a Codex line and asserts it is *not*
read.

## The human's own store

Which store the Sessions tab reads is the whole question, exactly as it is for
opencode and pi — but Codex answers it differently from both, and the difference
is worth stating because it is visible to the human.

opencode and pi are **group-local**: orrerix points a group's panes at a
per-group store (`OPENCODE_DB`, `--session-dir`), so a group's sessions are
somewhere the Sessions tab does not look, and the tab lists only what a solo pane
or the human's own terminal created.

**Codex is not**, and cannot be made so cheaply. Its only relocation knob is
`CODEX_HOME`, which moves the *whole* Codex home — `auth.json` and `config.toml`
included — so a per-group `CODEX_HOME` would boot every pane logged out. (That is
pi's `PI_CODING_AGENT_DIR` argument, reaching the same answer.) So orrerix writes
no `CODEX_HOME` at all, `group_local_session_store("codex")` is false, and every
Codex session — a group's and the human's alike — lives in `~/.codex/sessions`
and appears in the Sessions tab.

That is a deliberate consequence rather than a leak, and the docs say it out
loud: resuming a group's Codex session from the Sessions tab gives you the
**conversation and nothing else** — no MCP tools, no board, no roster — because
the row's resume command is a bare `codex resume <id>`. A group is restored from
**Orchestrations**, which rebuilds the whole spawn. The row is a way back into the
transcript, not a way back into the group.

### `archived_sessions/` is not read

`ARCHIVED_SESSIONS_SUBDIR = "archived_sessions"` (`rollout/src/lib.rs`) is a
sibling subtree of `sessions/`, of the same shape, holding what `codex archive`
moved out of the way.

Orrerix walks only `sessions/`. A human who archived a session asked for it to
stop appearing; listing it would undo that, and a by-id lookup finding one would
offer to resume a session they retired. They see no archived rows, and they see
no *wrong* rows, which is the failure mode that matters. `codex unarchive <id>`
brings a session back into the live tree and into the list.

This is a residual, so it is pinned rather than only disclosed:
`archived_sessions_is_not_listed_and_the_residual_is_pinned` writes an identical
session into each subtree and asserts that exactly the live one lists.

## Resume

The row's command is `codex resume <id>` — a **subcommand**, not a flag, and the
only session identity among orrerix's five sources that is not a flag. Two
consequences run through the frontend:

- **The excision is positional.** `panerestore.ts` cannot strip a Codex session
  id by flag name, so `stripCodexResumeFromCommand` matches the `resume` token
  and the argument it consumes. `--last` is the awkward one: it is an argument to
  `resume` that *starts with* `--`, so the general "consume the following value
  unless it looks like a flag" rule would drop `resume` and strand `--last`,
  leaving `codex --last` — an unknown root flag.
- **The token is appended LAST.** Codex's usage is
  `codex [OPTIONS] <COMMAND> [ARGS]` and its root options are inherited by the
  subcommand, not the other way round, so the resume goes at the end of the line
  and every recorded flag before it survives.

**Residual, peculiar to a bare word.** Every other identity token orrerix excises
begins with `--`, which prose does not; `resume` does not, so a Codex line
carrying an *unquoted* prompt whose words happen to run "… resume something"
would lose those two words. Orrerix never records such a line — it appends no
prompt to any agent command — so the exposure is a hand-typed custom command
only, and a quoted prompt is immune by the same tokenizer property that protects
a quoted `--resume`.

### A fresh respawn keeps no id

Codex has **no public pre-mint flag**. Its TUI `Cli` does carry a
`resume_session_id`, but it is `#[clap(skip)]` — *"Internal … Set by the
top-level `codex resume {SESSION_ID}` wrapper; not exposed as a public flag"* —
so there is nothing for `agentFreshCommand` to append. A Codex pane respawned
fresh starts a genuinely new thread, and its id is learned from the store
afterwards.

This is opencode's answer, reached on Codex's own facts rather than borrowed:
opencode loses its id because no opencode flag can pre-assign one, and neither
can any Codex flag. pi keeps its id because `--session-id` is exactly such a
flag. The rule is the same in all three cases; only the vendor's vocabulary
differs.

## Solo identity: `-p <profile>`

Codex has no MCP-config flag — its servers are `[mcp_servers.*]` tables in a TOML
file — but it has a flag that selects *which* config file is layered on:
`-p/--profile <name>`, which loads `CODEX_HOME/<name>.config.toml`. So a channel
identity still arrives on the command line, one indirection further out, and
Codex is a `SoloCli`.

`stripSoloMcpFlags` excises that flag **only when the profile name carries an
orrerix brand prefix**. `-p` is an ordinary Codex flag a human uses for their own
profiles, so an unconditional match would delete their flag on every restore.
That makes this arm strictly stronger than pi's neighbour, where a human's own
`--mcp-config` *is* stripped and re-minted — there the flag carries no
orrerix-owned namespace to recognise, and here it does.

What *writes* that profile file, and everything in it, is #2515 C1.
