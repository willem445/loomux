# pi as a loomux agent CLI

How loomux spawns, configures and contains `pi` panes, and why each seam is
the one it is. Companion to `doc/design/orchestration.md` (the capability
model this plugs into), `doc/design/workflows.md` (how a block's persona
reaches a CLI at all) and `doc/design/opencode.md` (the precedent this note
follows in shape and in evidence discipline).

**This note is written one slice at a time** (#2126: P1 spawn and bridge, P2
solo pane and sessions browser, P3 usage, P4 model catalog). Each slice adds
its own section and does not rewrite an earlier one, so every claim here stays
attributable to the round that measured it.

## Version pins and evidence labels

pi's published docs do not cover several surfaces loomux depends on — most
importantly `--session-id`, which is in the CLI's argument parser and its own
`--help` output but not in `docs/sessions.md` — so the load-bearing facts
below are read from the vendors' source at a pinned commit.

| Subject | Pin | Label |
|---|---|---|
| pi (`@earendil-works/pi-coding-agent` 0.84.4) | `earendil-works/pi@b79e4cc834970cca69daebffab7df1da7d1e52c4`, tagged `v0.84.4` | `DOCS` = `packages/coding-agent/docs/*.md` at that tag; `SOURCE` = `packages/coding-agent/src/…` at that tag |
| pi-mcp-adapter (the community extension that gives pi MCP at all) | `nicobailon/pi-mcp-adapter@6ba7d360fcc67a77ccbbb4921586614798020a7a` | `ADAPTER` = `config.ts` / `index.ts` / `utils.ts` at that commit |

Constraint 3 holds throughout: **no `pi` process was run by an agent** to
establish any of it, and none may be — every fact here is a read of a file.
What that leaves for a human to check live is the last section.

A source-read fact is a **labeled observation against a pinned version**, not
a contract. The refresh procedure is the same as every other CLI snapshot in
this repo: re-read the named files at the current tag, and update this note
and the pin together.

## Shape: claude-shaped sessions, an argv MCP seam nobody's CLI provides, and a lower containment ceiling

- **Session identity is claude-shaped.** `--session-id <id>` is *"Use exact
  project session ID, creating it if missing"* (`SOURCE` `src/cli/args.ts`
  line 288 help text; parser arm at :125). So loomux pre-mints a UUID exactly
  as it does for claude, on a fresh spawn and on a rejoin alike, and pi needs
  **no store watcher, no session baseline, and no contest refusal**. The
  "resume at a node, not a session" question pi's session TREE raises
  dissolves: `--session-id` opens the file and pi continues from its current
  leaf.
- **The MCP seam is on argv, but it is not pi's.** pi ships no MCP: *"It
  intentionally does not include built-in MCP, sub-agents, permission popups,
  plan mode, to-dos, or background bash"* (`DOCS` `usage.md` §Design
  Principles). The seam is the community `pi-mcp-adapter` extension, which
  registers a real `--mcp-config <path>` flag, and pi's own parser files an
  unknown `--flag value` into `unknownFlags` rather than erroring (`SOURCE`
  `args.ts:227-237`). `CliCaps::mcp_argv_seam` is therefore `true` — pi is the
  first CLI since copilot whose *solo* pane can carry its channel identity.
- **Containment tops out one rung lower than every other spawnable CLI.**
  `NoEdits`, not `ReadOnly`. See below.

## The launch line

```
pi [--session-id <uuid>] --session-dir <group>/pi/sessions
   --mcp-config <group>/configs/<agent>.json
   [--append-system-prompt <group>/configs/<handle>.pi.md]
   (--approve | --no-approve) [--exclude-tools edit,write]
   [--model <provider/id>] [--thinking <level>]
```

Everything loomux configures on pi rides argv — its MCP config, its session
identity, its session store, its contract and its containment — which is what
makes this the longest of the non-claude arms and what makes its assertions
land on the command line rather than on a generated document.

**A resume is the same line as a fresh start.** `--session-id` opens the
session it names *or creates it*, so there is one flag for both directions and
`resume` is deliberately unread in pi's arm. `pi_launch_flags_per_posture`
pins the two lines EQUAL, so an edit that splits them has to argue for it.
`--session <path|id>` is a different pi flag and loomux never emits it: it
continues an existing session and would not create a missing one, which is
exactly the case a resume of a never-prompted pane hits (pi defers creating
the session FILE to the first assistant response).

**Attended and unattended are byte-identical, and that is a product fact worth
stating loudly.** pi has no permission prompts to bypass — no `--auto`, no
`--yolo`, no `--approval-mode` — so `PI_UNATTENDED_FLAGS` is empty, the
group's `auto_ops` toggle is a genuine no-op on pi, and **an attended pi
worker runs every tool without asking**. `docs/orchestration.md` says so where
a human choosing a CLI will read it.

## The MCP bridge, and why it is not exclusive

Every other argv-seam CLI gets exclusivity alongside its config: claude pairs
`--mcp-config` with `--strict-mcp-config`; copilot's config rides
`--additional-mcp-config`; opencode gets `OPENCODE_DISABLE_PROJECT_CONFIG` for
a contained pane. **pi gets none, and it is not an omission.**

The adapter has an exclusive mode — `PI_MCP_CONFIG_MODE=exclusive` — and at
the pin it **discards the `--mcp-config` override**:

```ts
// ADAPTER config.ts:420-421
function getConfigSources(overridePath?: string, cwd = process.cwd()) {
  const userPath = getEffectivePiGlobalConfigPath(overridePath);
  …
// ADAPTER config.ts:506-508
function getEffectivePiGlobalConfigPath(overridePath?: string): string {
  return getPiGlobalConfigPath(isExclusiveConfigMode() ? undefined : overridePath);
}
```

In exclusive mode the single source's `readPath` is `userPath`, and `userPath`
resolved with `undefined` is `getAgentPath("mcp.json")` — one fixed per-user
file. So **a per-agent config and exclusivity are mutually exclusive at this
pin**: setting the variable would not harden a loomux pane, it would point
that pane at somebody else's file.

**The plan for #2126 states the opposite** ([§B2](https://github.com/willem445/orrerix/issues/2126#issuecomment-5533670546)),
and it is recorded here rather than quietly corrected because the plan comment
is permanent and a future reader will find it. Its reading of
`getConfigSources`' exclusive branch is right as far as it goes — one source —
and it does not follow the `:421 → :507` hop that decides *which file that
source is*. loomux takes the per-agent config; the residual below is the
price, and it is stated rather than claimed closed.

**What loomux writes** (`pi_mcp_config_json`, to
`<group>/configs/<agent>.json` — audit copy and authoritative bytes are one
artifact, as claude's are):

| Key | Value | Why |
|---|---|---|
| `mcpServers.<per-agent name>.url` | `http://127.0.0.1:<port>/mcp` | the loomux MCP endpoint |
| `.headers` | the agent-token header | a reconnect re-`initialize`s with the same token, so it returns as the same `Caller` and the same pane identity |
| `.lifecycle` | `keep-alive` | connect at startup instead of lazily, so the kickoff's first `report` pays no connect latency and the direct-tool registry is reconciled from a live `tools/list` before the first status snapshot |
| `.directTools` | `true` | register every tool individually rather than behind the adapter's proxy tool; loomux's largest role surface is well under the adapter's 75-tool advisory |
| `.toolPrefix` | `none` | the role templates spell bare names (`report(...)`), never a prefixed form. `brand::MCP_TOOL_PREFIX` is a claude ALLOWLIST fact, not a template fact |
| `.requestTimeoutMs` | 30000 | loomux's tools do real work behind a call, and one timing out reads to an agent as the tool being broken |
| `settings.disableProxyTool` / `scriptMode` | `true` / `false` | drop the adapter's own two tools once direct tools are up |

No `type` key: that is a claude-shaped field the adapter reads only through
its compatibility importer, and stating it would be a claim about a schema
this document is not written in. Every key above was checked against the
adapter's own `validateConfig` (`ADAPTER` `config.ts:704-716`), which accepts
`{ mcpServers, imports?, settings? }` with each server entry any JSON object —
so the per-entry keys are read by the runtime, not by the validator.

The pane environment carries **no part of this agent's identity**: one
variable, `PI_SKIP_VERSION_CHECK=1` (`DOCS` `environment-variables.md`),
suppressing the boot-time `pi.dev` request that would otherwise sit between
the pane appearing and the kickoff landing. The narrow variable, not
`PI_OFFLINE`, which would cut off more than loomux wants gone.

### The per-agent server name

pi's server is named `<loomux>-<agent id>`, not the bare `orrerix` every other
CLI's config uses. The adapter merges its sources and resolves a collision by
server NAME, **later source winning** (`ADAPTER` `config.ts:318-334`
`loadMcpConfig` folding `mergeConfigs`), and the repo's own `./.mcp.json` and
`./.pi/mcp.json` are pushed AFTER loomux's file in the source list
(`config.ts:438-495`). So a repo declaring a server called `orrerix` would
replace loomux's entry outright, and the pane would boot with no orrerix tools
— or with something else's.

It is free here and would not be elsewhere, which is why `one_server_map`'s
"one name, so the file and the argv cannot drift" argument is untouched:
claude, copilot and gemini all SPELL the server name on argv
(`--allowedTools mcp__<server>`, `--allow-tool <server>`,
`--allowed-mcp-server-names <server>`), and pi spells it nowhere. With
`toolPrefix: "none"` the name does not reach a tool name either. A side
benefit: the adapter's direct-tool metadata cache is keyed by server name, so
a per-agent name also stops a pane inheriting the tool list a *different*
role's pane cached under one shared name.

### The residual: a repo can still shadow a tool NAME

**Open, and not closed by anything above.** Because the bridge cannot be
exclusive, a pi pane's tool surface includes whatever the repo's own
`./.mcp.json` and `./.pi/mcp.json` declare. A per-agent server name removes
the *name*-collision route; it does not remove this one:

> A repo may declare its own MCP server whose direct tools are named `report`,
> `review_verdict`, `message_orchestrator` — the names the role templates
> tell an agent to call — and those tools sit in the same flat namespace as
> loomux's.

That is repo-authored input in a threat model where the repo is the thing
under review. It is the same class of exposure as an opencode WORKER's merged
project config (`doc/design/opencode.md` §Environment: loomux leaves the
repo's opencode config loaded for an uncontained class deliberately), and it
is stated here rather than papered over.

**What loomux does about it is MEASURE, not refuse** (constraint 8 — a repo
declaring MCP servers is legitimate and common, and loomux is not in a
position to adjudicate it). At every pi spawn, `pi_repo_mcp_exposure` reads
the two repo files and audits one `pi-repo-mcp-merged` row naming: each file,
whether it parsed, the server names it declares, whether any of them shadows
this agent's own server, and any direct-tool name that is also a loomux tool
name. A repo that declares nothing produces no row at all.

**What that measurement cannot see, stated because the gap is the point.** A
tool-name collision is visible only where an entry pins `directTools` to an
explicit LIST of names. An entry with `directTools: true` advertises its names
only at connect time, and loomux never connects to a repo's server to find
out. So an empty `shadows_loomux_tool_names` is *"nothing statically
visible"*, never *"nothing there"* — and `pi_repo_mcp_exposure`'s own tests
pin both directions so the distinction cannot quietly collapse.

**Upstream.** The clean fix is the adapter honouring `--mcp-config` in
exclusive mode, which would give loomux the same posture claude has — filed as
[nicobailon/pi-mcp-adapter#496](https://github.com/nicobailon/pi-mcp-adapter/issues/496).
If it lands, this section becomes `PI_MCP_CONFIG_MODE=exclusive` in
`cli_extra_env`, the per-agent server name stops being load-bearing (it stays,
for the direct-tool cache), and the residual above goes away. Until then the
residual is real, and `pi_repo_mcp_exposure` is what makes it visible rather
than merely admitted.

## Containment: the ceiling is `NoEdits`, and a planner cannot run on pi

`--exclude-tools edit,write` denies pi's two editing built-ins by NAME, and it
is applied AFTER every allowlist — *"`--tools` replaces this behavior with a
strict allowlist for all tools … `--exclude-tools` filters the resulting
list"* (`DOCS` `settings.md` §Tools). That is deny-beats-allow, the same
property claude's `--disallowedTools` and opencode's `edit: deny` have,
reached a third way. It is exactly a reviewer's containment.

It is **not** a planner's. `Containment::ReadOnly` additionally denies the git
subcommands that commit and push, and pi has no command-pattern deny at all —
its tool controls are name lists (`--tools`, `--exclude-tools`,
`--no-builtin-tools`, `--no-tools`) and it has no permission engine to express
a `bash` pattern in. So `max_containment` is `Containment::NoEdits`, and
`cli_can_host("pi", Role::Planner)` refuses with the reason quoted into the
message. The parser refuses the same pairing at load time, so a repo learns
when it writes the file rather than when a pane opens.

**The fail-open direction, named rather than left implicit.** A name list is
weaker than a permission KEY: opencode's `edit` key is what *every*
file-modifying tool asks under, so a tool opencode ships tomorrow is denied
with no loomux change. pi's list is two names, so a file-modifying built-in
added under a third name would NOT be denied and nothing would go red to say
so. That is the #448 hazard, and it is a property of pi's ceiling rather than
of loomux's use of it.

**The one route to a pi planner**, recorded because it exists and is
deliberately not taken: a loomux-shipped guard extension loaded with `-e
<path>`, blocking `bash` in a `tool_call` handler (`DOCS` `extensions.md`:
return values from `tool_call` control blocking via `{ block: true, … }`). The
human ruled out shipping a pi extension of our own, so it stays a named
follow-up rather than scope.

## Trust: pi's one boot dialog, and why a group pane never sees it

Interactive startup asks *"Trust project folder?"* when the cwd carries any of
`.pi/{settings.json, extensions, skills, prompts, themes, SYSTEM.md,
APPEND_SYSTEM.md}`, or a `.agents/skills` directory in the cwd or any parent,
AND no decision is saved in `~/.pi/agent/trust.json` (`SOURCE`
`core/trust-manager.ts`). Note `.mcp.json` and `.pi/mcp.json` are NOT on that
list — and loomux writes neither into a repo anyway.

**Every group spawn carries exactly one of `--approve` / `--no-approve`
(`SOURCE` `args.ts:219-221`; `DOCS` `usage.md`), so the dialog can never
appear on a pane loomux is about to type a kickoff into.** Which one is the
containment question:

- **Uncontained classes take `--approve`.** A repo's own extensions, skills
  and prompts are legitimate worker material — the same posture opencode's
  worker takes toward the repo's config.
- **Contained classes take `--no-approve`.** A repo's `.pi/extensions` could
  register a file-writing tool under a name `--exclude-tools edit,write` does
  not mention, and "the repo's resources never load" is a much simpler claim
  than "loomux's denials outrank them". Same argument as
  `OPENCODE_DISABLE_PROJECT_CONFIG_ENV`'s.

User- and global-level extensions — the MCP adapter included — load either
way; this is a PROJECT-local switch only. A SOLO pi pane in such a repo *does*
show the dialog, which is correct: it is the human's own pane.

## Sessions: the group's own store

`--session-dir <dir>` points every pane in a group at
`<group state dir>/pi/sessions` (`pi_sessions_in`, one derivation for the
launch line and the resume lookup alike). The point is
[`OPENCODE_DB_ENV`](opencode.md)'s: a group's sessions stay out of the human's
own `pi --resume` list, and theirs stay out of the group's.

**The layout is pi's, not loomux's.** With `--session-dir` pi writes the
session file DIRECTLY under that directory — `SessionManager.create` uses the
given directory with no per-cwd subdirectory, unlike the default store, whose
`--<cwd with every separator and colon replaced by ->--` segment is what makes
the default layout cwd-keyed. So one flat directory per group holds
`<timestamp>_<uuid>.jsonl` for every pane in it.

A lookup by id is therefore an **exact `_<id>.jsonl` filename-suffix match**
(`pi_session_file_in_dir`, the one declared assembly point for a pi session
file — the resume lookup and the usage meter both go through it, and it takes a
`PathSegment` so an unvalidated id cannot reach the filename it builds), never a
prefix or a `contains`: loomux's ids are
UUIDs, so a loose match is harmless today and wrong the first time one id ends
with another, and the leading `_` is what disambiguates a real id from a
timestamp that happens to end in the same digits. The cwd comes from the first
line — pi's session header, `{"type":"session","version":3,"id":…,"cwd":…}`.

**That header read is BOUNDED, and the bound is about the caller rather than
the file.** `orch_list_recorded`'s `resumable` check asks this once per pi
group on every session-browser refresh — the boot prefetch, each sidebar open,
each resume settle — while a pi transcript is append-only and grows a line per
turn. So the read is a `read_line` over a capped `BufReader`
(`PI_SESSION_HEADER_MAX_BYTES`, 64 KB) rather than a whole-file
`read_to_string`: a listing's cost must not scale with how much work a group
has done, to answer a question the first few hundred bytes answer. The cap
itself only bites on a file with no newline at all — a corrupt or mid-write
one — where "the first line" would otherwise mean "everything"; a truncated
read fails to parse and the session lists with an unknown workspace, which is
what a header-less file already gets.

**A pane that was never prompted has no file at all**, because pi defers
creating it to the first assistant response (`SOURCE` `session-manager.ts`
:1480, whose own comment says so). That makes an absent directory "not found"
rather than a store failure, and it makes a resume of such a pane a *fresh*
session under the id it was always going to have rather than an error — which
is only true because `--session-id` creates what it cannot find.

loomux creates the directory at spawn even so, and the reason is smaller than
it looks: pi WOULD create it itself (`session-manager.ts:880` `mkdirSync`s it
when persisting), so this is not load-bearing for the pane's boot. It is an
error rather than a best-effort mkdir because an unwritable group dir is a
real fault worth failing the spawn on, beside the config write that would fail
for the same cause — not because anything downstream needs it.

`group_local_session_store` is the predicate that keeps this store off
`StoreIndex` (#1592): that index amortises ONE enumeration of a big per-user
store across many groups, and a group-local store is already O(1) per group
with nothing to share.

## The human's own store: solo panes and the sessions browser (#2126 P2)

The section above is the GROUP's store. This one is the other half: the store an
unadorned `pi` writes, which is what the Sessions tab lists and what a solo pane
resumes from. A group's sessions are deliberately not listed there — they are
reopened through the group, with its roster, board and MCP identity, never as a
bare `--session` pane.

**Where it is.** `pi_sessions_root_from(session_dir, agent_dir, home)` ports the
vendor's own order: `PI_CODING_AGENT_SESSION_DIR` names the sessions directory
itself and wins; otherwise `<PI_CODING_AGENT_DIR or ~/.pi/agent>/sessions`. A
blank variable is not a value — the shell exports one routinely, and treating it
as a path would send every lookup to the app's own working directory.

**The settings-file `sessionDir` key is a disclosed residual.** pi resolves
`--session-dir` > `PI_CODING_AGENT_SESSION_DIR` > settings `sessionDir`, and
reading the third would pin loomux to the schema of a second vendor file. A human
who has moved their store that way sees no pi rows; they never see *wrong* ones,
which is the failure mode that matters.

**Both layouts are read, and that is not a hedge.** The flat-under-`--session-dir`
fact the Sessions section above records applies to the environment variable too,
and the first version of this scanner walked two levels unconditionally — so an
override store yielded zero rows, and `find_session_cwd("pi", id)` answered
`Ok(None)`, *"no such session"*, for a session that exists. `walk_pi_session_files`
now walks both shapes and is shared by the browser's collector **and** the by-id
lookup, so the two consumers cannot drift apart about where a store keeps its
files. Accepting both unconditionally rather than selecting on which root won: the
resolver would have to thread a second value through the test seam; a store used
both ways still lists everything; and neither arm can find anything where the
other owns the layout.

**No cwd filtering.** pi's own `list` filters an override store against the header
`cwd` because it answers "what can I resume from HERE". Both consumers here answer
something else — the browser lists every session and shows each one's own cwd, the
lookup is keyed on the id — so a filter would only hide real rows.

**The filename is not the id.** A session is `<timestamp>_<uuid>.jsonl`, matched
on the exact `_<id>.jsonl` SUFFIX. A prefix or bare `contains` would let one
session answer for another, and the leading `_` is what stops an id that merely
ends with another's from matching. The **header's** `id` then beats the one in the
file name, falling back to it only when the header is truncated — so a renamed or
hand-copied file still resumes the conversation it actually contains.

**Which resume flag, and why the two paths differ from every other CLI's.** The
Sessions tab emits `pi --session <id>`: it CONTINUES, and fails honestly on a file
the human deleted, where `--session-id` would mint an empty pane wearing the right
id. The pane-restore path emits `--session-id` on BOTH resume and fresh, because
that flag opens-or-creates and a pi session file does not exist until the first
assistant response — a pane killed before it was ever prompted has a real
pre-minted id and no file. `PI_SESSION_FLAG_NAMES` recognises both spellings for
excision, since pi refuses them together.

**Solo MCP excision, and the one flag that is not pi's.** A solo pi pane's channel
identity is one flag and its value, `--mcp-config <path>` — no allow-list token,
because pi has no permission surface and exclusivity rides on
`PI_MCP_CONFIG_MODE`. That makes the pattern weaker than claude's and copilot's,
so the pi branch of `stripSoloMcpFlags` is gated on the PROGRAM: ungated, it would
classify a hand-typed `claude --mcp-config ./my.json` as pi's and delete the
human's own flag on every restore. Residual: a pi line carrying the human's OWN
`--mcp-config` is stripped and re-minted rather than preserved.

**That flag comes from the adapter, not from pi.** pi core does not know
`--mcp-config` and rejects unregistered flags; `pi-mcp-adapter` registers it. So a
channel-tools solo launch on a machine without the extension boots into an
unknown-flag error. Nothing on the excision side can prevent that — it only ever
removes the flag — so it is named here as the requirement it is: an
adapter-presence preflight is still unbuilt (see §Deliberately not done).

**The argv-seam set is total over its union.** `SOLO_MCP_CLIS` is derived from a
`Record<SoloCli, true>` rather than written as `readonly SoloCli[]`, because a
union-typed array accepts any SUBSET — so widening `SoloCli` and forgetting the
list used to compile cleanly. Both directions are now a compile error. The
launcher's toggle gate, its mint gate and the excision all read that one list.

**Colour and badge.** `cerulean` `#00d2f0`, badge letter `P`. The measurements,
and the alternatives rejected against them, are in
[`ui-redesign.md`](ui-redesign.md) §The per-CLI hues.

## Usage and cost: a transcript fold with pi's own dollars (#2126 P3)

pi's session file is the usage record, so a pi pane is read the way a claude
one is — the totals are folded off the JSONL — and **not** the way an opencode
one is. What differs from claude is where the dollars come from:
`usage::PiFold` sums the `cost.total` pi wrote on each entry, so the figure is
**reported**, not estimated, and no `price_for` lookup happens. The mechanics
and the cursor contract live in
[group-cost-tracking.md](group-cost-tracking.md); what is pi-specific is here.

**Four entry shapes carry spend**, per pi's session-entry union
(`SOURCE` `core/session-manager.d.ts`, `pi-ai` `types.d.ts`):

| record | where the `usage` sits | why it counts |
| --- | --- | --- |
| `message` / `role: assistant` | `message.usage` | the ordinary turn |
| `message` / `role: toolResult` | `message.usage` (optional) | a tool that called a model of its own — pi's own comment says it is *"Not part of main LLM context accounting"*, but it is money spent under this session |
| `compaction` | entry-level `usage` (optional) | *"Usage from the LLM call(s) that generated this summary"* |
| `branch_summary` | entry-level `usage` (optional) | the same, for a branch |

Everything else — the header, `model_change`, `thinking_level_change`,
`custom`, `label`, user messages — contributes nothing.

**The fold is over the FILE, not the active path.** pi's session file is a
tree, and `/tree` navigation can leave whole branches off the current leaf. The
tokens on an abandoned branch were still bought, so they are still counted;
`a_branch_the_leaf_has_navigated_away_from_still_counts_because_the_tokens_were_bought`
pins that against a fixture where the two answers differ.

**`reasoning` is NOT folded into `output_tokens` — the opposite of what the
opencode mapping does with the same-named field.** pi's `Usage` documents it as
*"a subset of `output`: `output` already includes these tokens"*, and pi agrees
with itself elsewhere: `totalTokens` is `input + output + cacheRead +
cacheWrite`, and `calculateCost` prices exactly those four. Folding it in would
double-count. OpenCode's fifth bucket is genuinely disjoint from its `output`,
which is why that mapping folds and this one must not.

One observation is recorded rather than smoothed over, because it contradicts
that invariant: on the maintainer's own install, real `openrouter` transcripts
carry turns where `reasoning` EXCEEDS `output` (for example `output: 104`,
`reasoning: 110` on `moonshotai/kimi-k2.6`), which a subset cannot do. Whichever
way that provider's numbers are wrong, taking pi's documented contract and pi's
own cost basis is what keeps loomux's tokens and loomux's dollars describing the
same spend. If pi ever moves `reasoning` out of `output`, this is the paragraph
to re-derive.

**No message-id dedupe**, unlike claude's fold. claude's exists because
`--resume` re-emits assistant messages that are already on disk; pi opens the
same file by id and appends, and re-folding the same bytes is prevented by the
cursor's guards rather than by a set in the fold.

**pi ships a whole-file rewriter and claude does not, so name BOTH halves of
what catches it.** `_rewriteFile` — what a `/tree` navigation or a label edit
runs — opens the session file with mode `"w"`, a truncate, and writes every
entry back. Two shapes come out of that, and only one is caught by the arm it
is tempting to name alone:

- **A rewrite that DROPS entries shortens the file**, so `stat_verdict`'s
  `len < self.len` arm resets the cursor before any byte is read
  (`a_rewritten_pi_file_refolds_from_scratch_rather_than_folding_onto_a_stale_position`).
- **A rewrite that keeps the length or GROWS reaches `Extend` instead**, and
  what catches it is the 64-byte anchor: the consumed region's tail is re-read
  through the same handle before anything is folded onto it, and a mismatch
  discards the cursor
  (`a_non_shrinking_pi_rewrite_is_caught_by_the_anchor_not_by_the_length_arm`).

That second half matters more here than it does for claude, and the reason is
the residual on `ANCHOR_BYTES` (#1361 B1): an in-place edit to a consumed byte
EARLIER than `offset - ANCHOR_BYTES`, on a file that goes on being appended to,
is seen by no stat arm and no anchor, and is bounded only by
`CURSOR_REVALIDATE_AFTER` throwing the cursor away on a timer. For claude that
residual is close to theoretical — nothing rewrites a claude transcript. For pi
it is reachable by an ordinary `/tree` navigation on a long session, so the
honest statement is "wrong for at most one revalidation interval", not "cannot
happen".

**`source: "pi-transcript"`, `estimated: false`.** The snapshot arm keys on
`pi_sessions_dir(group)` plus the pane's session id, which pi premints
(`CliCaps::premints_session_id`), so unlike opencode the arm never sits idle
waiting for the pane to announce a session. Two things follow that are worth
saying next to [#2167](https://github.com/willem445/orrerix/issues/2167):

- the CLI this arm is reached with comes from `Guardrails::cli_for_block`
  (live poll) and `cli_for_agent` (`mark_dead`), so a pi pane in a roster's
  SECOND pi block is read as pi rather than as its class's default — the fix
  is shared, not re-implemented here, and
  `a_pi_pane_in_a_second_block_of_its_class_is_read_as_pi_not_as_the_class_default`
  pins both callers;
- the file's LOCATION involves no cwd slug at all. The store is the group's
  own directory and the id is loomux's, so the slug-shaped half of #2167's
  original suspicion has no surface on pi.

**Two residuals, stated rather than left to be found.**

*A SOLO pi pane has no usage source.* `compute_usage_snapshot` keys the arm on
`pi_sessions_dir(entry.group)`, and a solo pane's group is `__solo__` — a
directory a solo pane never writes to, because loomux passes `--session-dir`
only on a spawned pane's launch line. A solo pi pane's session lives in the
human's own `~/.pi` store, which loomux deliberately does not read. The arm
therefore finds nothing and the pane falls through to the statusline, which is
the same shape the opencode arm already has for the same reason (`__solo__` has
no `OPENCODE_DB` either). Reading a human's own store to price a solo pane is a
change of policy, not a missing line, so it is a follow-up rather than a gap
this slice left silently open.

*A store-level read error reads as "no usage", with no audit line.* pi's
locator returns `Ok(None)` for an absent directory and `Err` only for a real IO
fault on the group's own state directory; the usage reader collapses both into
its existing `None`. That is deliberately unlike opencode, which writes one
audit line per degrade episode (`note_opencode_db_degrade`) because a drifted
SQLite schema and a never-booted pane are otherwise indistinguishable. pi has no
such ambiguity to resolve — its ordinary "nothing written yet" case is already
`Ok(None)`, and the residual `Err` is an unreadable group directory, which the
spawn itself already fails loudly on (`fs::create_dir_all` is an error there,
not a best-effort mkdir). If that ever stops being true, the opencode shape is
the one to copy.

## Knobs (#687)

`--thinking <level>` over `off, minimal, low, medium, high, xhigh, max`
(`SOURCE` `args.ts:60`, `:147`), a SUPERSET of loomux's five `EFFORT_LEVELS` —
so pi joins claude as a CLI whose effort knob loomux can actually deliver, and
`effort_levels` is `EFFORT_LEVELS` in its row. A model that does not support a
level has it clamped or hidden per that model's own thinking-level map, which
is the same "safe to emit any of them" property claude's fallback rule gives.

`context_variants` is empty: pi's `--list-models` REPORTS a context column,
and no flag, setting or session control selects a variant.

`default_model("pi", _)` is `""`, for opencode's reason and none of its own:
`--model` takes `provider/id` against 15+ providers with no vendor-neutral
alias, so any default loomux picked would be the hardcoded model table #329
says ages badly — and would silently override a human who had already set
their own `defaultModel`.

## Readiness

`ready_marker: None`, and the argument is pi's boot ORDER rather than an
absence of evidence. `InteractiveMode.init()` focuses the editor and then
starts the UI **before** initialising extensions (`SOURCE`
`modes/interactive/interactive-mode.ts`, whose own comment says "Start the UI
before initializing extensions so `session_start` handlers can use interactive
dialogs"). So the input box is live before the adapter has connected anything
— the opposite order from opencode's, which is what made #1591 necessary
there. The generic painted-and-quiet gate is expected to hold.

Candidates exist if it ever does not: the adapter's footer status
`1 server enabled (1 connected)` or its startup notice
`MCP: 1/1 servers connected (N tools)`. Both were deliberately NOT adopted —
`ReadyMarker::CountThen`'s word rule refuses `1 server enabled`, so adopting
one means a new variant, and the `(1 connected)` segment renders only on
SUCCESS, so a failed connect would pay the ceiling on every kickoff. A row
gets a marker when a pane on it is caught painted-but-not-listening, never
speculatively.

Boot-time terminal I/O is the #179 class and needs nothing new: pi-tui writes
`ESC[?2004h` and a Kitty keyboard query `ESC[>7u ESC[?u ESC[c`, whose trailing
DA1 xterm.js auto-replies to. `classify_human_input` already skips CSI-shaped
replies and `firstInputAt` is keyed on `onKey`/paste rather than `onData`.

## Deliberately not done in P1

- **A generated `.pi/mcp.json` in the worktree** (the issue's own candidate):
  loses to the argv seam on four counts — a file in the tree a worker's
  blanket `git add` would commit, with a token in it; a clobber question for
  the human's own `.pi/mcp.json` in a repo-root pane; a `.git/info/exclude`
  write loomux does not do today; and no exclusivity gained anyway, since the
  repo's `.mcp.json` would still merge.
- **`PI_CODING_AGENT_DIR` per agent** (full isolation): relocates `auth.json`,
  `settings.json`, `trust.json` and the installed adapter package, so every
  pane would boot logged-out with no MCP. Same reason `OPENCODE_CONFIG_DIR`
  was rejected for opencode.
- **A store watcher / `SessionBaseline::Pi`**: unnecessary given
  `--session-id`, and it would add a contest-refusal path for a CLI that has
  no ambiguity to contest.
- **`toolPrefix: "mcp"`** (claude's spelling): the role templates never spell
  the prefix — they write `report(...)` — and bare names cost fewer tokens per
  call.
- **Remote pi blocks.** pi accepts a pre-minted id, so the *reason*
  `parse_workflow` used to give for `remote:` requiring `cli: claude` ("claude
  is the only CLI that accepts one") became false the day pi landed. The gate
  is unchanged and the reason is reworded to what is true — claude is the only
  CLI loomux drives remotely — with a negative assertion pinning that the old
  claim cannot come back.
- **pi's RPC mode as a structured driver.** `--mode rpc` (JSON-per-line over
  stdin/stdout) is exactly the shape #84's native-protocol track wants, and it
  belongs to that track: a PTY pi pane is what was asked for and what every
  other harness has. Nothing here builds against RPC and nothing here blocks
  it.
- **A compact nudge.** pi is not on the short list of CLIs loomux pastes
  `/compact` into. It has `/compact` and auto-compacts by default, but loomux
  has no context-pressure reader for it yet; a follow-up, not a gap this slice
  left open silently.

## Still for the human (live only)

Constraint 3 means no agent can run any of these. Each names what a failure
looks like.

1. **`pi list` → the installed `pi-mcp-adapter` version.** The
   `--mcp-config` flag registration is present at the pinned commit; an older
   installed version may not have it, in which case pi files the flag away
   silently and the pane boots with **no orrerix tools at all**. There is no
   red anywhere — check `/mcp` in the pane. (A launcher preflight reading
   `~/.pi/agent/settings.json`'s package list is a follow-up.)
2. **One native-Windows group with a `cli: pi` worker and a `cli: pi`
   reviewer.** The kickoff is pasted once and submits on its own (no
   `delivery … unconfirmed`), multi-line brief intact; `/mcp` shows the
   per-agent server connected with BARE tool names; `report(...)`,
   `message_orchestrator(...)` and `review_verdict(...)` all succeed; the
   reviewer has no `edit`/`write` tool and still runs `bash`; a killed pane
   resumed from the Orchestrations list continues the same conversation and
   its `/session` shows the pre-minted id.
3. **Boot noise.** No DA/Kitty reply text lands in the editor, and a pane left
   untouched shows no first-input timestamp. If a paste is ever lost while pi
   is still loading extensions, that is the evidence for a ready marker —
   capture the screen at the moment it happens.
4. **The trust dialog.** Open a group in a repo that has `.pi/settings.json`
   or `.agents/skills`: no dialog on any group pane. A SOLO pi pane in that
   same repo DOES show it, which is expected.
5. **Stale direct tools.** The per-agent server name should prevent a pane
   inheriting a previous role's cached tool list; watch the first pane after a
   role change for a tool that should not be there.
6. **`--append-system-prompt` really reads the FILE.** Its help text says
   "text or file contents", and loomux always passes a path. Confirm the
   contract is in effect — e.g. the agent can name its own role-instructions
   path — rather than the literal path having been injected as text.
7. **`pi --list-models` populates the model picker.** Behind the inherit row
   within the probe timeout, with `provider/model` ids matching what `pi --model`
   accepts (an OpenRouter row arrives as a two-slash id). If it times out, the
   picker shows only inherit + custom — expected on a cold catalog.

## The model catalog: `--list-models` is the machine's own answer (#2126 P4)

Everything above configures panes; this section is about the launcher's model
picker, whose two sources (`doc/design/model-catalog.md`) are a curated
suggestion and what the CLI on THIS machine reports. For pi only the machine
can answer: `--list-models [search]` (`SOURCE` `args.ts:196`) prints **only the
models whose auth is configured** (`DOCS` `models.md`: unauthenticated models
"stay unavailable in `/model` and `--list-models`") — the "this machine and
this account" answer no curated list can be.

**The layout, and why pi gets its own parser instead of opencode's.** pi
prints a two-space-padded column table — header
`provider  model  context  max-out  thinking  images`, one row per model
sorted by provider then id, every column `padEnd`'d to its width and joined
with two spaces (`SOURCE` `src/cli/list-models.ts:83-114`). `provider` and
`model` are SEPARATE columns, so the id loomux emits is ASSEMBLED:
`parse_models_from_table` (cliprobe.rs) finds the header by its first two
whitespace tokens (`provider` `model`), reduces every later line to printable
text (`plain_line`), splits on runs of two-or-more spaces, and keeps the row
only if both of its first two columns are id-shaped, as `{provider}/{model}`.
`parse_models_from_list` cannot serve this: it takes each line's first
whitespace token, which here is the bare provider half — a token with no `/`,
rejected by that parser's own id check. Sharing would have meant pi's probe
always yielded nothing; the per-CLI parser lives in its `ENUMERATORS` row, the
same DATA-not-a-branch pattern as `CLI_CAPS`.

**A two-slash id is one id.** An OpenRouter row is
`openrouter  z-ai/glm-5.3-flash  …`, so the assembled id carries a second `/` —
`openrouter/z-ai/glm-5.3-flash`. The id-shape check accepts `/`-separated
segments (`is_id_shaped`, shared with the opencode parser), and
`prettyModelId`'s two-slash guard leaves such an id unlabelled rather than
half-rewritten; `test/modelcatalog.test.ts` pins both. The header line itself
never becomes a model: the line that trips the header check is consumed by it,
not parsed as a row.

**The 8 s probe timeout against pi's 15 s internal budget.** loomux's probe
kill is `HELP_TIMEOUT`, 8 s; pi runs `--list-models` under its own
`AbortSignal.timeout(15_000)` (`SOURCE` `main.ts:864`) — pi's budget for a cold
models.json refresh, not ours. When the refresh loses the race, the probe dies
mid-list, the parse yields nothing, `probe_with` marks the answer incomplete
and `probe_cached` declines to cache it — so the picker keeps rendering only
the curated `[INHERIT_MODEL]` row and the NEXT probe retries, now against a
catalog pi has finished refreshing. Caching the miss would pin the worst moment
of the session for the rest of it, the same reason an opencode `models` failure
is never cached. The cost is bounded: off-thread, once per probe call, and the
launcher memoizes its own probes.

**No measured time is recorded here, and that is deliberate.** The worst case
is structural (two 8 s timeouts, off-thread); the typical case is pi answering
from a warm cache in well under a second, and MEASURING it means running a
real pi, which constraint 3 forbids an agent. The live check is §F8 above.

**The curated row stays `[INHERIT_MODEL]`, probed or not.** pi has no
vendor-neutral alias (§Knobs), so anything loomux curated would be a stale
copy of a live answer; the probe's ids merge in behind the inherit row
(`mergeModelOptions` pins it first — `test/modelcatalog.test.ts`), and a
timeout degrades to inherit + custom, not to an empty dropdown.
