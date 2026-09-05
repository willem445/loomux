# Codex CLI as a session source

Started by #2515 slice **C2**, which taught orrerix to read Codex's own rollout
store: the by-id lookup behind a resume, the Sessions-tab rows, and the frontend
identity that makes a codex pane adopt a codex session.

**Still one section short.** C2 wrote the store sections; C1 added the harness —
the launch line, the profile file orrerix writes, containment, and the session
watcher. The **usage meter** is #2515's C3 and adds its own section here.
Nothing below is a placeholder for a decision somebody else will take.

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

## The harness

Everything from here down is #2515 slice **C1**: what loomux writes for a codex
pane, the line it launches, how the pane's session is identified, and what was
deliberately not done.

The pins above cover it too, with one addition. C1 read four things the slice
plan had wrong or had not reached, and each is recorded on the issue with its
blob citation rather than only here:

- **`max` IS deliverable.** `ReasoningEffort`'s hand-written `FromStr` maps
  `"max" => Ok(Self::Max)` beside `none`, `minimal`, `ultra`, `persistent` and a
  `Custom(String)` catch-all, so codex's effort vocabulary is a strict superset
  of loomux's five rather than a subset of it.
- **`-p` names a whole config document.** The flag is
  `config_profile_v2: Option<ProfileV2Name>`, documented *"Layer
  `$CODEX_HOME/<name>.config.toml` on top of the base user config"* — **not**
  the legacy `[profiles.<name>]` table, whose `ConfigProfile` struct has no
  `projects`, no `mcp_servers` and no `developer_instructions` at all. Every key
  below depends on that distinction.
- **A rollout FILE does not exist at boot.** `RolloutRecorder::new`: *"For newly
  created sessions, this precomputes path/metadata and defers file creation/open
  until an explicit `persist()` call."*
- **The trust key is honoured from a profile layer.** The loader folds every
  layer into `merged_so_far` before it computes the project trust context, and
  the profile is pushed as a second *user* layer.

## The launch line

    codex -C "<cwd>" -p <profile> [-m <model>] [resume <thread id>]

Four things, and three of them are one thing.

**`-C/--cd <DIR>`, on every line.** *"Tell the agent to use the specified
directory as its working root."* Without it codex's working root is the
process's cwd, which for a loomux pane is not the pane's. On a **resume** it is
doing real work rather than being symmetric for its own sake: resuming a thread
whose recorded cwd differs from the launch directory makes the TUI *prompt*
("resume here or there?") when `tui.resume_cwd` is unset, and a prompt on a pane
loomux is about to type a kickoff into is a lost kickoff.

**`-p <profile>`** carries everything else — see the next section.

**`-m <model>` only when a block pins one.** `default_model("codex", _)` is
empty, so an unpinned block inherits the `model` key from the human's own
`~/.codex/config.toml`, and the generated profile deliberately does not name a
model.

**`resume <thread id>` only on a resume, and LAST.** This is the only session
identity among loomux's five adapters that is a *subcommand* rather than a flag.
codex's usage is `codex [OPTIONS] <COMMAND> [ARGS]` and
`SharedCliOptions::inherit_exec_root_options` gives the subcommand the root
options written before it — so this order is not a preference, it is the only
one that works.

A **fresh** spawn names no session at all, because there is nothing to name:
codex has no opens-or-creates flag, and the id is learned from the store
afterwards (see §Sessions).

**Not on the line, ever:** `--full-auto` (which does not exist at the pin — its
only occurrence in `codex-rs` is a test, and the docs call it "a deprecated
compatibility alias"), the dangerous bypass flag and its `--yolo` alias,
`--approve-for-me`, and `-s`/`-a`. The last two are the interesting refusal:
they exist and would work, and emitting them would give a pane a posture its own
profile disagrees with. Posture belongs to the profile, so the attended and
unattended lines are byte-identical and the two profiles are not — a pair of
assertions that is only honest together.

## The profile file

    $CODEX_HOME/<brand>-<agent id>.config.toml

`resolve_profile_v2_config_path` builds it as
`format!("{profile_name}{CONFIG_PROFILE_V2_SUFFIX}")` against `codex_home`, with
`CONFIG_PROFILE_V2_SUFFIX = ".config.toml"` — directly under the home, no
subdirectory. The name is `<brand>-<agent id>` so the sweep below can recognise
loomux's own files, and `ProfileV2Name`'s alphabet (ASCII alphanumerics, `_`,
`-`) is *wider* than `PathSegment`'s, so every agent id loomux mints produces a
selectable name by construction.

### Key order is load-bearing

In TOML every key after a table header belongs to that table. So every top-level
scalar — `approval_policy`, `sandbox_mode`, `model_reasoning_effort`,
`developer_instructions` — is emitted **before** the first `[…]` line. Get this
wrong and nothing fails loudly: codex's strict-config check reports an unknown
key at best, and at worst the pane boots with none of its posture and no obvious
cause. `a_codex_profiles_top_level_keys_all_precede_the_first_table_header`
pins it on POSITION, so a new key added below the fold reddens without anyone
having to remember the rule.

### Every key, and why

| key | value | why |
| --- | --- | --- |
| `approval_policy` | `never` \| `on-request` | the posture. `never` is `AskForApproval::Never`, "Never ask the user to approve commands"; `on-request` prompts, and the attention scan catches the overlay. Unlike pi's, this toggle is real. |
| `sandbox_mode` | `workspace-write` | the only rung a working agent can use — see §Containment. |
| `model_reasoning_effort` | a block's `effort:` | omitted entirely when unset: `ReasoningEffort` refuses the empty string outright, so a blank key would fail the *whole* profile rather than be ignored. |
| `developer_instructions` | the block's role contract | see §The contract. |
| `[sandbox_workspace_write] network_access` | `true` | off by default under `workspace-write`, and a worker that cannot reach GitHub is not a worker. |
| `[projects."<cwd>"] trust_level` | `trusted` | see §Trust. |
| `[mcp_servers.<brand>]` | `url`, one header map, `default_tools_approval_mode` | orrerix's own server, over streamable HTTP. One server contract, six spellings. **No timeouts** — see below. |

`default_tools_approval_mode = "auto"` is codex's own default, **spelled out
rather than inherited**: a human's `config.toml` can set a different one
globally, and an agent whose `report` needs approving has nobody to approve it.
The profile layer wins over the user layer, so stating it is what makes the
pane's tool surface independent of their setting.

### Neither MCP timeout is written, and that is the decision

The first version of this design set `tool_timeout_sec = 30` and
`startup_timeout_sec = 20`, documented as RAISES over codex's "60s" and "10s"
defaults. Both defaults were wrong. They came from #2515's slice plan (which
took them from the published config reference) and were transcribed here without
being read from the source — the failure CLAUDE.md names as "a routed
instruction's factual premise is a claim to verify, not text to transcribe".
Caught in review round 1.

At the pin, codex resolves both keys in `codex-mcp/src/connection_manager.rs`
with `.unwrap_or(DEFAULT_…)` against `codex-mcp/src/rmcp_client.rs`:

```rust
pub(crate) const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(300);
```

So the shipped values were **reductions** — the tool timeout by 10× — under a
rationale ("orrerix's tools do real work behind a call, and one timing out reads
to an agent as the tool being broken") that argues for the opposite. A
`spawn_agent` behind a large group can legitimately outrun 30 seconds; stock
codex would have waited 300.

Writing a key whose only effect is to make the pane worse than the vendor's own
default is indefensible, so both keys are gone. Nothing is lost that pi's
`PI_MCP_TIMEOUT_MS` buys: that constant raises the pi adapter's shorter default
*to* 30s, and codex already gives ten times that.

`a_codex_profile_sets_no_mcp_timeout_and_says_why` pins the absence. It
deliberately does not assert 30 and 300: those are someone else's constants,
orrerix cannot keep them honest, and pinning them would go stale silently the day
codex changes them. What orrerix controls — and all it should assert — is that it
writes no number at all.

### Trust: the single most important line

`should_show_trust_screen(config)` is exactly
`config.active_project.trust_level.is_none()`, and it renders "Do you trust the
contents of this directory? … 1. Yes, continue 2. No, quit".

A fresh worktree is always a new project root — `find_project_root` walks
ancestors for a marker, and a worktree's `.git` is a **file**, which the
`marker == ".git" && metadata.is_directory` skip therefore does not apply to, so
the walk stops at the worktree rather than continuing to the main clone. Without
the trust key **every worker pane would boot into that dialog and eat its
kickoff**.

**Residual, because a reader will look for it.** The key is the pane's `cwd`,
and codex looks trust up under the project ROOT. Every pane loomux launches is
*at* a root — a worker's worktree, or the group's repo — so the two coincide
today. A pane launched in a subdirectory of a repo would find no entry and see
the dialog. Writing the ancestor chain instead would mean re-implementing the
vendor's root discovery here; writing the repo root would trust more of the
human's tree than this pane asked for.

### The token rides the channel the pane HAS

This is the one place a group pane's profile and a solo pane's differ, and the
difference is forced rather than chosen. #2515's plan D2 said the token "lives
in the pane ENV, never in the file or on argv". That is right for the pane D2
was describing and impossible for the other.

| pane | how the token reaches codex | why |
| --- | --- | --- |
| **group** | `env_http_headers = { "<header>" = "<VAR>" }`, value exported by `cli_extra_env` | the pane has an environment, so the secret goes there and no token byte is in the file. D2 verbatim. |
| **solo** | `http_headers = { "<header>" = "<token>" }` | the pane has no environment. |

`solo_prepare` only ever appends a flag string to a command line the human owns
— it sets no pane environment at all. A solo profile naming a variable nothing
sets would connect with **no auth header**: every tool call fails, and the pane
is still advertised `delivery_only: false`, which is exactly the
advertised-status-disagrees-with-reality defect `solo_prepare`'s own fallback
arm exists to prevent. Carrying the token in the file is what every other CLI's
generated config already does, and the file is in a directory that is the
human's own.

Both shapes are pinned by asserting the ABSENCE of the other as well as the
presence of their own. Presence alone would pass on a generator that emitted
both maps, which is the one outcome worse than either: the secret in the file
*and* a dependency on the variable.

### The contract

codex has no `--append-system-prompt` and no `--agent`, and its
`model_instructions_file` is the wrong knob — it REPLACES the built-in prompt
rather than adding to it, which would take the agent's own tool discipline away
in order to give it a role. The seam is `developer_instructions`, so the
contract is the only one of loomux's five persona shapes that travels **by
value** rather than as a path. It never reaches argv, so #417's
`CreateProcessW` limit — the reason the other four are files — does not apply.

**It is `ContractCarrier::SystemLayerFull`, and that is measured rather than
assumed.** The key is documented as "inserted as a `developer` role message",
which sounds like conversation history a compaction would eat. It is not: codex
reads it from CONFIG into every `TurnContext` and re-inserts it through
`build_initial_context_with_world_state` on the compaction path itself
(`start_new_context_window` → `replace_compacted_history`). A compacted codex
pane recovers its contract from this file with loomux doing nothing.

**The encoding is a multi-line BASIC string, not the `'''` literal the slice
plan named.** A TOML literal string admits no escapes at all — its content ends
at the first closing delimiter run and there is nothing one can write instead —
so "escaping `'''`" means REWRITING the role contract on its way to the agent.
A contract that reaches the model altered is worse than one that is encoded,
because the alteration is invisible from both ends. The basic form is lossless
and total: every `"` becomes `\"` so a delimiter run cannot appear in the body,
every `\` becomes `\\` so a trailing backslash cannot become a line
continuation, and every other control character becomes `\uXXXX`, which the
literal form forbids outright. Newlines stay literal, which is what keeps a
multi-KB contract readable when a human opens the file.

### Removal, and the sweep

This is the only file loomux writes into a vendor's user directory, so three
things travel with it: the loomux-branded name, removal with the agent that owns
it (`mark_dead`), and a startup sweep for anything the removal missed. A file in
someone else's home that nothing cleans up is #502 by another route.

The sweep applies #464's rule one level down. That sweep already refuses to
delete anything when it cannot enumerate GROUPS; a group whose `agents.json` is
individually unreadable is the same ignorance in a smaller scope, and reading it
as "that group has no agents" would make every one of its panes' profiles look
orphaned. So a single unreadable roster makes the live-agent set *incomplete*
and the sweep deletes nothing. A group directory with **no** `agents.json` is
not that case — that is a group that has never spawned an agent, which is real
and common, and it genuinely contributes no ids.

## Containment: why the ceiling is `None`

codex's only containment axis is `sandbox_mode`
(`read-only | workspace-write | danger-full-access`), and its `tools` section
exposes only `view_image` / `web_search` — there is no way to deny the editing
tool by name. `read-only` is not a reviewer's tier either: in read-only mode
codex "can read files and answer questions, but requires approval to make edits,
**run commands, or access network**", which removes the tests and the `gh` a
reviewer's job is made of.

The **rules engine** is the one route that could have worked — `prefix_rule(…
decision="forbidden")` is a real git-mutation deny — and it does not, because
rules load only from `CODEX_HOME/rules/*.rules` and a project's `.codex/rules/`.
Neither is per-agent, so loomux cannot give one pane a rule set and its
neighbour another.

So the ceiling is `Containment::None`: **worker, orchestrator and solo run on
codex; reviewer, planner and MANAGER are refused by `cli_can_host` at parse
time**, with
`containment_note` quoted into the refusal so a rejected workflow says what is
actually missing.

#267 stage 2 read all of this correctly and then concluded that loomux would
never spawn codex at all. That does not follow — `Containment::None` is the
ceiling every worker and orchestrator block already runs at — and C1 is the
correction of the conclusion, not of the ceiling.

A `NoEdits` reviewer stays a named follow-up, gated on the live check in §Still
for the human.

## Sessions: a store watcher, and a contest it refuses

codex has **no public pre-mint flag**: the TUI's `resume_session_id` is
`#[clap(skip)]` — *"Internal … Set by the top-level `codex resume {SESSION_ID}`
wrapper; not exposed as a public flag"*. So `premints_session_id` is false and
codex takes copilot's shape: snapshot the store's thread ids immediately before
the spawn, then poll for one that was not there.

Three things differ from copilot's watcher, each for a codex reason:

- **A cwd match is REQUIRED, with no newest-wins fallback.** copilot falls back
  to the newest fresh session because it may not have written a
  `workspace.yaml` yet, so "no cwd match" there is routinely "not yet". codex
  writes `cwd` in the rollout's FIRST line, at creation, so a file that exists
  and does not match this directory is a different pane's session — and binding
  it would hand this pane somebody else's conversation. Failing to identify is
  recoverable and visible (`session-untracked` in the audit); a wrong binding is
  neither.
- **Two matches is `Contested`, never resolved.** The store is the human's, not
  the group's, and several panes in one group routinely share a directory. That
  makes "two rollouts appeared here" a recurring answer rather than an edge
  case.
- **The deadline is ten minutes, not ninety seconds.** The rollout file does not
  exist at boot (see the pin above), so it appears when the pane does some work.
  A short deadline would leave permanently unidentified every pane whose first
  turn was late — a kickoff queued behind a busy pane, an attended group the
  human walked away from — and the failure would look exactly like codex not
  being installed.

A torn header is `Waiting`, not a wrong binding: the next poll reads it whole.

**Residual, and it is the deadline's other edge.** A pane whose first turn is
more than ten minutes late stays unidentified: the rollout file does not exist
until the session first `persist()`s, so the watcher polls for something not yet
written, gives up at `CODEX_SESSION_TIMEOUT`, and audits `session-untracked`.
Resume and usage never bind for that pane, and it looks exactly like codex being
absent. Ten minutes is chosen against the alternative — a shorter deadline loses
more panes, a longer one widens the window in which another pane's new session
in the same directory is a candidate — but it is a trade, not a fix. The real fix
is a signal that a session STARTED, which codex does not offer on the TUI path.
Raised as a premortem in review round 1 and recorded here rather than left in a
review comment nobody re-reads.

## Readiness

`ready_marker: None`. A row gets a marker when a pane on it is caught
painted-but-not-listening (#1591), never speculatively — and codex's own boot
hazard is not a late input loop but the trust dialog, which the profile answers
before the pane paints rather than by waiting longer.

## Deliberately not done

- **`-c key=value` on argv.** It would push a TOML inline table and a quoted
  Windows path key through the command builder's shell string, and the token
  through the process list: two quoting hazards and one leak, for nothing the
  profile does not already do.
- **A project `.codex/config.toml`.** It loads only when the project is
  *trusted* — and the trust dialog fires before it is read, so it cannot answer
  the dialog. It also lands in a worktree that a blanket stage-everything
  commits.
- **A per-agent `CODEX_HOME`.** It relocates `auth.json` and `config.toml` too,
  so every pane would boot logged out. This is pi's `PI_CODING_AGENT_DIR`
  argument reaching the same answer, and it is why the store is not group-local.
- **The `notify` hook** as a session-id source. It fires at turn END and needs a
  hook binary loomux does not ship.
- **`codex exec --json` / the app-server.** Those are not a TUI pane; that is
  #84's native-protocol track.
- **`[windows] sandbox`.** Elevated setup is the human's, and getting it wrong
  costs them a private desktop their credentials are not on.
- **`tui.alternate_screen`.** A real key, and settable here — a profile-v2 file
  is strict-validated as a whole `ConfigToml` — but whether to trade codex's
  alternate screen for scrollback is a live judgement call left to the human.

## Still for the human

Constraint 3 means none of the following can be checked by an agent. Each is a
place where the design above rests on a claim about someone else's runtime
rather than someone else's source.

1. **Trust.** Open a group whose worker cwd is a fresh worktree: no "Do you
   trust the contents of this directory?" dialog, and `/status` shows
   workspace-write with the workspace trusted.
2. **Elevated sandbox.** In that pane, a GitHub auth check and a push to a
   scratch branch both succeed with no overlay under `auto_ops`. Commands run as
   a separate sandbox user on a private desktop, and the human's `gh`/git
   credentials may not be visible there.
3. **Writes outside the tree.** A clean `npm install` in the worktree succeeds —
   npm/cargo caches and `%TEMP%` are outside the workspace, and under `never`
   these would be silent task failures. `writable_roots` is the likely first fix.
4. **MCP.** `/mcp` lists the loomux server as connected, `report(...)` lands, and
   tools appear unprefixed.
5. **Resume.** Kill a worker pane and resume it from the Orchestrations list:
   the same conversation, and no cwd prompt.
6. **The reviewer ceiling** (the follow-up's gate). In a scratch repo,
   `codex -s read-only -a never`: ask for the git working-tree status and the
   open PR list. If both run, the ceiling can rise to `NoEdits`; if either needs
   approval, it stays `None`.
7. **Alt screen.** Decide whether `tui.alternate_screen = "never"` belongs in
   the profile.
