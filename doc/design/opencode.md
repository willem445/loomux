# OpenCode as a loomux agent CLI

How loomux spawns, configures and contains `opencode` panes, and why each
seam is the one it is. Companion to `doc/design/orchestration.md` (the
capability model this plugs into) and `doc/design/workflows.md` (how a block's
persona reaches a CLI at all).

## Version pins and evidence labels

OpenCode's published docs do not cover several surfaces loomux depends on, so
the load-bearing facts below are read from the CLI's own source. Everything
marked `SOURCE` is read at **`anomalyco/opencode@f67e80c2756ac0d9d05a31da59483b0a7a6cd0c3`**,
the commit tagged **v1.18.11**; everything marked `DOCS` is from
https://opencode.ai/docs/ (indexed in the `agent-cli-reference` skill).
Constraint 3 holds throughout: **no `opencode` process was run by an agent**
to establish any of it. The full verification pass, with per-file line
citations and the local-observation half, is #722's slice-V memo
([1](https://github.com/willem445/orrerix/issues/722#issuecomment-5161943081),
[2](https://github.com/willem445/orrerix/issues/722#issuecomment-5161943414),
[3](https://github.com/willem445/orrerix/issues/722#issuecomment-5161943777));
this note carries the conclusions loomux's code rests on, and its citations,
rather than reproducing the memo — a design note is an argument, and the memo
is the evidence behind it.

A source-read fact is a **labeled observation against a pinned version**, not a
contract. The refresh procedure is the same as every other CLI snapshot in this
repo: re-read the named files at the current tag and update this note and the
pin.

## Shape: gemini-shaped delivery, copilot-shaped sessions, claude-grade denial

- **Config delivery** is gemini-shaped: one generated document, delivered by an
  environment variable on a pane loomux spawns, named on no command line. That
  is why `CliCaps::mcp_argv_seam` is `false` for opencode and a *solo* opencode
  pane stays delivery-only (a solo launch appends flags to a command line the
  human owns; it cannot set environment).
- **Session identity** is copilot-shaped: `--session <id>` continues an
  existing session and nothing pre-assigns one (`DOCS`, CLI page), so loomux
  cannot mint an id up front the way it does for claude.
- **Containment** is claude-grade, reached a different way — see below.

## The launch line

```
opencode [--session <id>] [--model <provider/model>] [--agent <handle>] [--auto]
```

That is the whole of it, and the shortness is the design rather than an
omission: the MCP server, the permission posture and the persona *definition*
are all keys in a document loomux delivers by environment. A contained pane's
denials being absent from this string is likewise the design — they are
asserted on the generated document and on the environment instead.

**The TUI, not `opencode run`, and that is load-bearing.** `run` handles a
permission it cannot ask a human about by **rejecting** it outright and
continuing (`SOURCE`, `cli/cmd/run.ts`: without `--auto` it prints
`permission requested: … ; auto-rejecting` and replies `reject`). So an
attended posture built on `edit: "ask"` would silently refuse every edit
instead of prompting. Under the TUI the same ask renders as an interactive
footer. Anything that later "optimises" an opencode pane onto `run` inherits
the silent rejection.

**`--auto`, not `--yolo` / `--dangerously-skip-permissions`.** All three exist
and mean the same thing (`SOURCE`, `cli/cmd/tui.ts`), but the latter two are
marked hidden in the CLI's own option table; loomux emits the documented
spelling. `--auto` is not a policy setting — it replies "allow once" to a
permission that was already going to be *asked*, and a `deny` never raises an
ask at all, which is exactly why it cannot reach a contained pane's denials.

**`--model` is omitted when empty**, and `default_model("opencode", …)` is
empty on purpose. Unlike claude's strong/mid pair or gemini's single `pro`,
opencode has no vendor-neutral alias: ids are `provider_id/model_id` against a
catalog of dozens of providers, so any default loomux picked would be a
hardcoded model table (#329) *and* would silently override a human who had
already chosen. Blocks and the launcher pin explicit models; an unpinned pane
inherits the human's own configuration.

**Model ids carry a `/`, and `sanitize_model` had to be widened to admit it.**
Before #722 the sanitizer dropped every `/`, so `opencode/deepseek-v4-flash-free`
became `opencodedeepseek-v4-flash-free` — a model that does not exist, handed
over with no error. `/` is inert in both emitted forms (not a glob
metacharacter to a POSIX shell, not an operator in PowerShell), so the widening
costs nothing the "can't smuggle an argument" property depends on. The same
deliberate-widening decision as #709's, which went the other way on `[`/`]`
because those *are* glob syntax.

## The generated document

`write_mcp_config`'s opencode branch produces one JSON document
(`opencode_config_json`) and delivers it two ways: written to
`<group>/configs/<agent>.json` as the **audit copy**, and set on the pane as
`OPENCODE_CONFIG_CONTENT` as the **authoritative bytes**. One generator, two
deliveries, so the two cannot disagree.

```jsonc
{
  "mcp": { "loomux": { "type": "remote", "url": "http://127.0.0.1:<port>/mcp",
                       "enabled": true, "headers": { "X-Orrerix-Agent": "<token>" },
                       "oauth": false, "timeout": 30000 } },
  "share": "disabled",
  "permission": { /* the global posture — see below */ },
  "agent": { "loomux-<group>-<block>": { "mode": "primary",
                                         "prompt": "{file:<abs path>}",
                                         "permission": { /* denials */ } } }
}
```

- **`oauth: false`** — OAuth auto-detection is on by default (`SOURCE`,
  `v1/config/mcp.ts`) and the loomux server authenticates by header, so a 401
  during discovery must not start a flow this server does not speak.
- **`timeout: 30000`**, above the documented 5000ms default: loomux's tools do
  real work behind a call (`report` writes state and audits, `notify_when`
  registers a watch), and a timeout reads to an agent as the tool being broken.
- **`share: "disabled"`** — a group agent's session is never published. The
  vocabulary is `manual` (default) / `auto` / `disabled` (`DOCS`).
- **Autoupdate is suppressed by environment, not by the config key** — see
  below.

### Why `OPENCODE_CONFIG_CONTENT` and not `OPENCODE_CONFIG`

`SOURCE`, `config/config.ts` (`loadInstanceState`) — sources are merged in load
order with `mergeDeep`, later winning per leaf key:

| # | source |
|---|---|
| 1 | well-known remote configs |
| 2 | global `~/.config/opencode/opencode.json` |
| 3 | **`OPENCODE_CONFIG`** (custom file path) |
| 4 | project `opencode.json`, walked cwd→worktree |
| 5 | each `.opencode` dir: `opencode.json`, then `agents/*.md`, then `modes/*.md` |
| 6 | **`OPENCODE_CONFIG_CONTENT`** |
| 7 | account/org console config (signed in with an active org) |
| 8 | managed config dir |
| 9 | macOS MDM managed preferences |
| 10 | `config.mode.*` folded into `config.agent.*` |
| 11 | **`OPENCODE_PERMISSION`** → `mergeDeep(result.permission, …)` |

The custom-file variable loads at rank 3 — **before** the project's own config
— so a repo could re-allow what loomux denies. The inline variable loads at
rank 6, after every repo-owned source. This is also why loomux does not
substitute `OPENCODE_CONFIG_DIR`: full isolation, but it silently discards the
user's own global config and TUI settings, where the merge posture is the
claude-like one loomux already prefers elsewhere.

One consequence worth writing down because it is counter-intuitive: among
`.opencode` **directories** the *outermost* wins, and `~/.opencode` wins over
every project one — `ConfigPaths.directories` does not reverse its list the way
`ConfigPaths.files` does (`SOURCE`, `config/paths.ts`). Immaterial to loomux
(rank 6 beats them all); recorded so nobody re-derives it wrong.

### Namespaced agent handles

The generated agent's handle is `generated_agent_handle` —
`loomux-<group>-<block>` — the same shape claude's and copilot's generated
files use, and here it is load-bearing rather than merely tidy. loomux's entry
does win a same-name collision with a repo's own `.opencode/agents/<name>.md`
(rank 6 beats rank 5), but the merge is a **deep** merge: a colliding file
would keep every key loomux left unset. A handle a repo cannot guess makes the
collision impossible instead of survivable — and every field loomux cares about
is emitted explicitly regardless.

### The contract travels by file

The entry's `prompt` is `{file:<abs path>}`, pointing at a file under the
group's own `configs/` directory carrying `block_contract_text`'s output. Four
mechanics govern it (`SOURCE`, `config/variable.ts`):

1. **Substitution is textual, on the raw string, before the document is
   parsed.** A JSON-escaped Windows path would therefore reach the filesystem
   with its separators doubled, so loomux emits **forward slashes**
   (`opencode_path`).
2. The inserted content is JSON-escaped by opencode, so a persona containing
   quotes or newlines cannot break the document around it.
3. The content is `.trim()`ed — nothing may depend on the trailing newline.
4. **A missing file is fatal to config load.** The file is therefore written by
   `persona_inject`, which runs before the pane is spawned at both spawn sites
   — which is why `write_mcp_config` is called *after* `persona_inject` in
   `register_orchestrator_pane` too, where it used to sit above.

It lives under the group dir rather than a CLI-owned per-user directory for two
reasons: opencode has no user-level agents directory loomux could write a
*definition* into without owning its lifecycle (the definition is in the
document), and a file under the group dir is reclaimed with the group, so it
needs none of the orphan sweeping `~/.claude/agents` and `~/.copilot/agents`
do (#464/#502). The repo's own `.opencode/agents/` is never written, for the
reason copilot's `.github/agents/` is not: loomux does not dirty a user's git
tree with files they did not write.

## Containment

### One key, not a list of tool names

opencode's permission engine is keyed on the permission a tool **requests**,
not on the tool's name — and `edit`, `write` and `apply_patch` all request
`"edit"` (`SOURCE`: each tool's own `permission: "edit"`, and
`permission/index.ts`'s `disabled()`, whose `edits = ["edit", "write",
"apply_patch"]` maps all three to that one key). The CLI's own read-only agent
is built from exactly that rule: `plan`, described "Plan mode. Disallows all
edit tools", denies with `edit: { "*": "deny" }` (`SOURCE`, `agent/agent.ts`).

So `OPENCODE_EDIT_DENY_PERMISSION` is a single key where the claude, copilot and
gemini constants are lists — and that makes this tier *stronger* than a name
list, not weaker. A file-modifying tool opencode ships tomorrow asks the same
key and is denied with no loomux change: the fail-open direction #465 could
only close for claude (with `dontAsk`, a mode a reviewer cannot have) is closed
here by construction, for reviewers included.

The scalar `"deny"` form is deliberate. It becomes a rule with the `*` pattern,
and `disabled()` drops a tool from the model's toolset entirely when the last
matching rule for its permission is a `*`-pattern deny — the same "never sees
it as a choice at all" property claude's bare `--disallowedTools` has.

### Rule order is the containment

`evaluate` takes the **last** matching rule, matching by wildcard on the
permission KEY as well as the pattern, and `fromConfig` emits rules in the
document's key order (`SOURCE`, `permission/index.ts`). Two traps follow, both
pinned by tests:

- **Never emit a `"*"` permission key.** It matches every permission, so one
  sitting after the specific denials silently re-allows them.
- **Allows before denies within a key.** `git *: allow` after
  `git commit*: deny` re-opens the commit.

The no-match fallback is `ask`, not allow — the docs' "opencode allows all
operations by default" is true only because the built-in defaults ruleset opens
with `"*": "allow"`.

`git commit*` carries no space before the `*`: the matcher is zero-or-more
against the whole command, so `git commit*` covers a bare `git commit` where
`git commit *` would not.

### Two layers, each covering the other's failure mode

Same "delivered twice, on purpose" shape as gemini's deny rules, and for the
same kind of reason — not belt-and-braces for its own sake:

- **Agent-level rules** (in the document's `agent.<handle>.permission`) are
  concatenated **after** the global ruleset (`SOURCE`, `agent/agent.ts`:
  `item.permission = Permission.merge(item.permission, fromConfig(value.permission))`,
  where `item.permission` already ends with the global `user` ruleset). Being a
  strictly-later, separate ruleset is what makes them independent of key order
  *between* the two objects.
- **`OPENCODE_PERMISSION`** (rank 11) applies after **every** config rank,
  including the org/managed/MDM ones loomux does not control.

The second layer exists for a specific silent failure: an unresolvable or
subagent-mode `--agent` does **not** error. It prints a warning and falls back
to `build`, the most permissive agent there is (`SOURCE`, `cli/cmd/run.ts`,
`cli/cmd/tui.ts`). A reviewer degrading into a full-write agent over a dropped
environment variable or a typo, with only a scrollback line to show for it, is
exactly the failure #462's guarantee cannot survive — so containment never
rests on `--agent`, and `mode` is always emitted explicitly. When the contract
file cannot be written, loomux emits **no** `--agent` at all rather than one
that would resolve to `build`.

### Project config is skipped for contained panes

`OPENCODE_DISABLE_PROJECT_CONFIG` (`SOURCE`, `flag/flag.ts`; truthy = `"1"` or
`"true"`) skips rank 4 and drops project `.opencode` dirs from the directory
list. Set for `Containment::denies_edits()` panes only. For a worker the repo's
config is legitimate material (its commands, its own MCP servers) and loomux's
document already wins the merge; for a reviewer or planner the calculus flips —
"loomux's rules win the merge" is a claim about ordering, "the repo's rules
never load" is a claim about nothing at all — and a contained pane is the one
place worth paying a repo's custom commands for the simpler story.

### The size of the guarantee

Unchanged from every other CLI (see `Containment`'s own doc): these are
structural denials of the tools named, not a sandbox. `bash` stays open for a
reviewer because a reviewer runs the tests, and `gh` stays open for a planner
because its deliverable is an issue comment.

## Postures

| | attended worker | unattended worker | reviewer (`NoEdits`) | planner (`ReadOnly`) |
|---|---|---|---|---|
| `edit` | `ask` | `allow` | `deny` | `deny` |
| `bash` `*` | `ask` | `allow` | `allow` | `allow` |
| `bash` `git *`, `gh *` | `allow` | — | — | `gh *` allow |
| `bash` `git commit*`, `git push*` | — | — | — | `deny` |
| `external_directory` | `allow` | `allow` | `allow` | `allow` |
| `question` | `allow` | `deny` | attended: `allow` | `deny` |
| argv | — | `--auto` | follows `auto_ops` | `--auto` (always) |

The attended row runs the opposite direction from every other CLI loomux
spawns: opencode's own default is `"*": "allow"`, *more* permissive than any
loomux attended pane, so the generated posture **narrows** it rather than
widening a restrictive default.

`external_directory` is `allow` on every posture because the pane's own
role-instruction file lives under the group's state dir, outside the worktree,
and that key defaults to `ask` — without it an unattended pane would stall on
its own contract.

It is the **blanket** form rather than the narrow `{"*": "ask", "<group
dir>/*": "allow"}` that the stated requirement would suggest, and the narrow
one would be a regression, not a tightening. Rules are concatenated
defaults-then-user with last-match-wins, so a user `"*": "ask"` lands *after*
opencode's own default whitelist for this key — its temp dir, its skill dirs,
its reference dirs — and demotes every one of them to `ask`, turning the CLI's
own internal machinery into prompts. Re-listing those paths here would mean
hardcoding another vendor's internals, which ages exactly as badly as a model
table (#329). What the breadth costs is one prompt on an *attended* pane before
the agent reads outside its worktree; `read` is not a tier loomux contains at
on any CLI (see `Containment`: these are denials of named tools, never a
filesystem sandbox), so no guarantee rests on it.

`question` is keyed on **attended**, not on containment, and it exists because
of a consequence of running every pane as a config-declared agent: the built-in
defaults ruleset denies `question`, and only the default `build` agent
re-allows it (`SOURCE`, `agent/agent.ts`). Without restoring it an attended
worker's "should I do X?" would come back as a permission error rather than a
question. The unattended half is equally deliberate — a pane with nobody to
answer that stalls on a question is the deadlock `forces_unattended` exists to
prevent.

The agent-level block carries **denials only**. The shell a reviewer or planner
still needs is already allowed by the global posture its ruleset is
concatenated after, so restating it there could only risk stating it
differently — and widening is not what an agent-level block is for.

## Environment

| variable | value | when |
|---|---|---|
| `OPENCODE_CONFIG_CONTENT` | the generated document | always |
| `OPENCODE_PERMISSION` | the global posture, again | always |
| `OPENCODE_DISABLE_AUTOUPDATE` | `1` | always |
| `OPENCODE_DB` | `<group dir>/opencode/opencode.db` | always |
| `OPENCODE_DISABLE_PROJECT_CONFIG` | `1` | contained classes only |

**Autoupdate by environment, not the config key.** A mid-boot self-update
restarts the CLI and flushes anything typed into the first instance — exactly
the hazard copilot's `--no-auto-update` closes. An environment variable cannot
be overridden by a config rank loomux does not control, and the key can.

**Per-group database.** The session/message store is a single SQLite database
under the user's data root (`SOURCE`, `database/database.ts`), not the
per-project JSON tree the troubleshooting page still describes — that layout
survives only as migration code. `OPENCODE_DB` honors an absolute path as-is,
so each group gets its own file. Two structural reasons: the human's own
sessions stay out of a group's store and vice versa, and "the newest session in
this database" becomes an unambiguous question — which is what session
identification needs on a CLI that cannot be handed a session id up front. The
project id is derived from the git **origin remote**
(`sha1("git-remote:" + host/path)`, `SOURCE`, `project.ts`), so every loomux
worktree of one repo resolves to the same project — a per-project scan could
never tell two agents apart, and this is the seam that replaces it. SQLite
creates the database file but not its parent directory, so loomux creates it
and treats a failure as a spawn error rather than a best-effort mkdir.

The reader lands on this seam — see *Usage and cost readback*, below.

## Knobs (#687)

Both rows ship empty, with notes that say why:

- **effort.** There *is* a session-scoped reasoning-effort flag — `--variant`,
  "model variant (provider-specific reasoning effort, e.g., high, max,
  minimal)" — but only on `opencode run`; the TUI loomux spawns has no
  `variant` option. The seam that does exist on the TUI path is per-agent
  `agent.<name>.variant` in the document loomux already generates. It stays
  unwired because the per-model vocabulary is provider-specific (the flag's own
  help says "e.g."), and a knob loomux cannot reliably deliver renders disabled
  with a reason rather than silently doing nothing.
- **context.** Model-determined; no session-scoped variant switch is documented
  or present in the TUI's options.

## The launcher and the workflow pane (#722 slice D)

The frontend adds no vendor fact of its own. It asks
(`agent_cli_knobs` → `CLI_CAPS`) and renders the answer: both opencode knobs
come back with an empty value set and a note, which the existing generic path
already renders as *disabled, carrying opencode's own reason*. No opencode
special case exists in `selectorknobs.ts`, and adding one would be how the
launcher comes to advertise a `--variant` loomux does not write.

**The model picker offers "no model at all" as a real row, and lands on it.**
Every other CLI has a vendor-neutral alias to default a role to — claude's
strong/mid pair, copilot's `auto`, gemini's `pro`. opencode has none, which is
why `default_model("opencode", _)` is empty. If the launcher defaulted a role
to a curated id anyway, the two would disagree: the backend would be inheriting
the human's own `opencode.json` model while the form quietly pinned one over
it. So `orchclis.ts`' opencode row pins nothing on any role, and its curated
list starts with the empty id — which the picker labels rather than rendering as
a blank line, because a menu row a human cannot read is not an option.

The curated ids are a shortcut, not a catalog:
`opencode/deepseek-v4-flash-free` (the free Zen model #722 exists for), its paid
sibling `opencode/deepseek-v4-flash`, and `opencode/gpt-5.1-codex` (the models
reference's own example). The Zen free tier is broader and deliberately not
enumerated — each row is another line of hardcoded model table to go stale
(#329), against a `custom…` entry that already accepts any id and a merge with
whatever the probe reports (`opencode models`, below).

**The `/` survives every hop the frontend owns.** A curated id is the option's
`value` verbatim; only the *label* is prettified, and the prettifier now names a
`provider_id/model_id` id by its model half (`opencode/deepseek-v4-flash-free —
DeepSeek V4 Flash Free`) while the id in front keeps every character. It splits
narrowly — one `/`, a lower-case identifier in front, a model half it can
actually improve — so a Bedrock ARN goes on passing through untouched, and an id
whose model half only re-cases (`opencode/auto`) gets no name at all rather than
a name with the provider stripped off it. The roster box prettifies nothing: it
states what will be spawned, so an opencode block reads
`reviewer · opencode · opencode/deepseek-v4-flash-free`, and an unpinned one
reads `default model`.

**PATH detection is unchanged, and deliberately not presence-gating.** opencode
is listed like every other CLI; the launcher probes the program, warns inline as
you pick, and refuses the whole launch on submit if it is missing. Hiding a CLI
until a probe resolved would read as loomux having forgotten it — the same rule
the disabled-with-a-reason knobs follow. The orchestrator-mode CLI id IS the
program name probed, which `test/orchclis.test.ts` pins against the launchable
agent catalog.

## Model enumeration: `opencode models` (#935)

opencode is the one CLI in the roster that can be *asked* what models it has:
`opencode models` "[l]ist[s] all available models from configured providers" and
"displays all models available across your configured providers in the form of
`provider/model`" (`DOCS`, https://opencode.ai/docs/cli/). `cliprobe.rs` runs it
as a second probe command and takes its output over the `--help` parse.

**Why it wins over the help parse, categorically.** The help text is what the
vendor wrote; the list command is what *this machine* is configured for. For
opencode the gap is not a nicety: a valid id is `provider_id/model_id`, and the
help parser's token filter admits no `/` at all — so the `--help` route can
never produce an id an opencode pane could actually launch on: the most it can
yield is a provider-less fragment, which is not a model opencode accepts. So
until now the curated three carried opencode's picker on their own.

**It lives in `ENUMERATORS`, a table, not in an `if`.** Same reason
`CLI_CAPS` is a table (constraint 8): the next CLI that grows a list command is
a row — program, argument, parser — and the probe keeps one code path. The
argument string is pinned by test to exactly `models`: **never `--refresh`**,
which "[r]efresh[es] the models cache from models.dev" (`DOCS`, same page) —
a probe that fires while a human is filling in a launcher form must not re-pull
a remote catalog on their behalf, and the cached list is the one their own
opencode would use anyway.

**Availability is decided by `--help` alone, and the list command can only
add.** The launcher refuses an entire group launch on `available: false`, so an
enumerator that failed, timed out, or printed a layout the parser doesn't
recognise must leave that verdict untouched — and it does, structurally: the
`models` run happens after availability is settled, and only a *non-empty*
parse replaces the help-parsed list. The failure path is therefore exactly the
behaviour that shipped before, not a degraded one.

**The parser drops what it doesn't recognise, and never manufactures.** The
docs state the id format but not the surrounding layout, so
`parse_models_from_list` models no layout: each line is reduced to printable
text, its first token taken, and that token kept only if it *is* an id (every
`/`-segment non-empty and made of id characters — which is also what rejects a
bare URL), deduping while preserving the CLI's own order.

The guarantee that makes this safe to ship against an unobserved layout is
narrower than "it recognises nothing unfamiliar", and stating it that loosely
was wrong (#939 review): **every id it emits is a verbatim whitespace-delimited
token of the CLI's own output.** It cannot splice two fields together, invent
characters, or repair a broken token. That property is what removals have to
respect, and the review found it broken: the probe's escape-stripper *deleted*
control bytes, and a tab is both a control byte and a column separator — so a
tab-columned row collapsed `id<TAB>Claude Sonnet 4.5` into `…-4-5Claude`,
id-SHAPED but not an id, non-empty, and therefore promoted over the help-parsed
fallback and to the head of the picker. `plain_line` substitutes a SPACE for
every escape sequence and control byte instead of deleting it, so a removal
always separates: the id extracts from its column, or the line yields nothing.
(It is deliberately *not* named `strip_ansi` — `orchestration::strip_ansi` is a
different function with many callers, and deleting is right for its job of
reading a pane's ring, where cursor addressing legitimately re-writes one row.)

What it is NOT is a promise that everything accepted is a model. A column
header literally reading `provider/model` would be taken as one, because at
that point it is indistinguishable from an id. That is the deliberate direction
of error: under-recognise, never manufacture — a stray literal the CLI itself
printed sits in a picker beside real ids with the `custom…` escape intact,
whereas a spliced token is a launch that fails on an id nothing ever offered.

**Cost.** An opencode probe is two subprocesses instead of one, each under the
same 8s timeout, on the blocking pool. Only a COMPLETE probe is cached for the
app run: an installed opencode whose `models` run failed keeps its help-parsed
fallback for that call but is not remembered, so a provider configured — or an
`opencode auth login` finished — a minute later shows up without restarting
loomux, the same argument that already keeps an unavailable probe out of the
cache. The list may also be long: it is a flat dropdown by design, and neither
the backend nor the picker caps it — a silently truncated model list is worse
than a long one.

**Residual, for live validation.** The exact stdout layout of `opencode models`
is not documented and was not observed (constraint 3 — no agent ran the CLI).
Two layouts are covered by test — bare `provider/model` lines and tab-columned
rows — and any layout at all is covered by the guarantee above: the ids offered
are strings opencode itself printed, and a layout that yields none degrades to
today's curated list. A human running it once on a real install settles what it
actually prints.

## Usage and cost readback (#722 slice B)

Every other CLI loomux reads usage from writes a transcript file. OpenCode
writes a database, and the `session` row for a pane already carries the dollar
cost it computed itself plus **five** token counters (`SOURCE` + `LOCAL-OBSERVED`,
the DDL is quoted in `src-tauri/src/opencodedb.rs`). So the reader is one row,
not a fold over message records, and OpenCode gets **no `price_for` entry** —
loomux would be second-guessing a number the vendor already computed against
its own provider table.

That is why an opencode agent's dollars come back `estimated: false` and land
in `group_usage`'s **reported** basis, next to Claude's *estimated* one. A
group running both reads *mixed*, which is the honest label: blending a
price-table guess and a vendor's own invoice under one word would make neither
checkable.

**Two mappings are lossy, and both are decisions.** loomux has four token
buckets; OpenCode has five.

- **Reasoning folds into `output`.** It is a separate counter for OpenCode, and
  a real session on the maintainer's machine spent 1193 reasoning tokens
  against 1115 output ones — dropping it would have halved that session. It
  goes to `output` rather than anywhere else because that is where the CLI
  loomux compares against already puts it: Claude counts thinking inside
  `output_tokens`, so the fold makes one bucket mean one thing.
- **`cache_write` is `cache_creation`.** The same quantity under two vendors'
  names.

**A pane's usage is its session plus its subagent sessions.** OpenCode's
subagents are `session` rows of their own with `parent_id` set, and their spend
is spend the pane caused; charging only the root row would under-report exactly
the agents that fan out most. The rollup is a recursive CTE over `parent_id`,
so depth is not assumed — and it uses `UNION`, not `UNION ALL`, so a cycle in
those edges could never spin.

**Read-only, not `immutable`.** The connection is `SQLITE_OPEN_READ_ONLY`:
SQLite refuses every write on one, so loomux cannot corrupt or lock out a live
opencode whatever this code does. `immutable=1` was the tempting alternative —
it skips locking entirely — and it is wrong as the primary: an immutable
connection ignores the WAL and reads the main file alone, which for a live
agent means silently missing most of the session. A meter that under-reports
without saying so is worse than one that reports nothing. It survives only as a
*fallback* for a store whose writer died without checkpointing, where the
`-shm` rebuild a plain read-only open would need is itself a write.

**Every failure is a degrade.** Absent store, unopenable file, drifted schema,
lock contention past a 250ms bound — each yields a zero-usage agent and a fall
through to the statusline, never an error the group has to be rescued from.
The schema is a vendor's internal detail with no compatibility promise, so
drift is a *when*, not an *if*.

**Degraded is not the same as undiagnosable.** The snapshot cannot act on
*which* degrade it hit, but the record must still say: a drifted schema needs a
human and a never-booted pane needs nobody, and both otherwise surface only as
"not from the store". `note_opencode_db_degrade` writes one
`opencode-usage-degraded` line per **episode** per group, carrying the kind and
the underlying message. It has to be latched, because this runs on the polled
`group_usage` path — an unlatched line would be an audit entry every UI tick
for as long as the condition lasted, flooding the log exactly when something is
wrong with it. Latched by *kind*, not by message, so a varying error string
cannot defeat the latch; cleared on the first successful read, so a recurrence
after a real recovery is a new incident rather than one silenced for the life
of the process. `Absent` is never audited at all — a group whose opencode panes
have not booted has no store yet, which is the ordinary state. Same one-shot
shape as `HoldEpisode`'s `announced`/`notice_reported`.

**What is not read here.** Which session a pane owns — that is the next
section's, on the same connection: `opencodedb::session_usage_on` takes an open
connection precisely so identification pays for one open, not two. Until a
pane's session is named, `compute_usage_snapshot`'s opencode arm has no id to
key on and simply does not fire.

## Session identification (#722 slice C)

OpenCode has no `--session-id`. `--session` *continues* an existing session and
nothing pre-assigns one, so loomux cannot hand a pane its identity the way it
does claude's — it has to learn it afterwards. That makes the shape copilot's:
snapshot the store before the spawn, poll for what appeared, bind it.

**The project id cannot answer this, and that is not a detail.** OpenCode
derives it as `sha1("git-remote:" + host/path)` (`SOURCE`, `project.ts`), so
every worktree of one repo — which is every agent in a loomux group — resolves
to a single project row. What separates panes is the `session.directory`
column.

**A candidate is four things at once:** `parent_id IS NULL` (subagents are
`session` rows too, and binding a pane to its own subagent would make usage,
resume and digest all answer about the wrong conversation while looking
healthy); in this pane's directory; absent from the pre-spawn baseline; and not
already bound to another pane in the group.

That last one is the difference from copilot, and it exists because the stores
differ. Copilot's is the machine's, and two panes racing in one directory is
the exotic case. A group's opencode store is written by **every pane in that
group**, and the orchestrator, the reviewer and any worker without its own
worktree all run in the repo root — so "the newest new session in this
directory" naming *another pane's* session is the ordinary case here, not the
corner.

**Two candidates refuse rather than pick.** `doc/design/session-id-learning.md`
settled this exact class of contest already, and the asymmetry it settled it on
holds here unchanged: a refused match costs a pane that stays unidentified —
precisely the status quo of an opencode pane before this slice — while a wrong
one reports one agent's spend as another's and resumes a human into a
conversation that is not theirs, undetectably. Refusal is usually self-healing:
the watcher keeps polling, and when the other pane's watcher claims its
session, the count falls to one.

The residual is real and named rather than assumed away: two panes in the *same
directory*, spawned close enough together that neither's session existed when
the other's baseline was taken, both refuse and neither identifies. Panes are
spawned one at a time, seconds apart, so the ordinary case is that the earlier
pane's session is already in the later one's baseline — but when it does
happen, the timeout audits `contested` with a count, so the state is
diagnosable rather than a silent nothing.

**A baseline that cannot be read is not an empty baseline.** An empty one says
"the store held nothing", which makes everything in it a candidate; if the
truth was "the file could not be opened this instant", that hands this pane a
session that belongs to someone else. So a real degrade refuses to watch at
all and says so once in the audit log. A *missing* store is the opposite and
genuinely is empty — that is the first opencode pane in a group, whose file
opencode has not created yet.

**One watcher, not two.** `spawn_session_watcher` serves copilot and opencode;
`SessionBaseline` carries where to look, and the poll interval and deadline
come with it. OpenCode's deadline is ten minutes against copilot's ninety
seconds, and that is an honest gap rather than caution: loomux cannot verify
whether opencode writes the `session` row at TUI boot or at the first turn —
checking means running the real CLI, which constraint 3 forbids — and a
deadline chosen for "at boot" would leave every pane whose first turn was late
permanently unidentified, looking exactly like a CLI that was never installed.

### Two things an opencode pane could not do at all

Both are identity, both were found building this slice.

- **`sanitize_session` rejected every opencode id.** It admitted hex digits and
  `-`: exactly a claude UUID. OpenCode mints `ses_` + 12 hex + 14 base62, so
  `spawn_agent(resume_session = <an opencode id>)` failed as "invalid resume
  session id" with nothing wrong with the id. Widened to ASCII alphanumerics
  plus `-` and `_` — which still admits no separator, no `.`, no whitespace, no
  quote and no shell metacharacter, so the `Path::join` and command-line
  interpolation downstream keep every property they had.
- **An opencode group could not be reopened.** `sessions::find_session_cwd`
  answers for claude and copilot and sends everything else down its *claude*
  arm, so an opencode orchestrator resume searched `~/.claude/projects`, found
  nothing, and hard-failed with "not found in the opencode session history on
  this machine". `session_cwd_in_store` routes opencode to the group's own
  store — which is where its panes write, `OPENCODE_DB` pointing every one of
  them at `opencode_db_path(group)`.

### Resuming an orchestrator, and what a killed store does to it (#1563)

The backend resume is CLI-aware, and `opencode_orchestration_restores_from_recorded_session`
(`src-tauri/tests/orchestration.rs`) is the pin that says so.
`resume_recorded_session`'s orchestrator branch resolves the CLI from the
group's own block, asks `session_cwd_in_store` — which routes opencode to
`opencode_db_path(group)` — and relaunches through
`create_orchestration_group`, which emits `opencode --session <id>`. Its
copilot twin has existed since #412; nothing drove an opencode orchestrator
through that path until #1563. The three things the pin holds down are the
three that are opencode-specific: the store consulted is the **group's**, the
flag is `--session` (opencode's *continue* flag — not claude's `--session-id`,
not copilot's `--resume`), and the MCP wiring rides the pane environment rather
than argv, so a resumed orchestrator that came back without
`OPENCODE_CONFIG_CONTENT` would have its conversation and no loomux server at
all.

What that pin is **not** is a claim that an opencode orchestrator is resumable
from the UI. The reason it was not sits above this layer — the learned id never
reached `tabs.json`, and the sessions browser reads the human's *global* store
by design (see *The Sessions browser* below). Those are #1563 A and B.

**A store a killed process left behind.** The crash the report started from
raises a specific question about this path: `opencodedb::open_readonly` is a
plain read-only open with an `immutable=1` **fallback**, and an immutable
connection reads the main database file alone — it never consults the WAL. A
hard kill is exactly the state where that distinction bites, because the main
file holds what was last checkpointed and the `-wal` holds everything committed
since, which usually includes the orchestrator's own session row.

Measured on the artifacts a kill actually leaves, rather than assumed
(`an_opencode_store_left_by_a_killed_process_still_resolves_the_session`):

- With the whole set on disk — main file, dirty `-wal`, `-shm` — the session
  resolves. A read-only connection consults the WAL, which is the entire reason
  the primary open is not `immutable=1`.
- With the `-shm` swept (a reboot, a cleaner, a hand-copy that took the two
  files a human thinks of), it still resolves: SQLite rebuilds the wal-index
  given write access to the directory, and `SQLITE_OPEN_READ_ONLY` constrains
  the *database*, not its sidecars.
- The fallback's blindness is real all the same, and the test proves it on that
  same store instead of describing it: an `immutable=1` connection returns
  `Ok(None)` for the row living in the `-wal` while reading the checkpointed
  row beside it perfectly.

So the fallback loses crash-committed sessions **if** it engages, and nothing
on the resume path engages it on either artifact set. That is recorded rather
than fixed. The fix would be a `-wal`/`-shm` presence check before falling
back, and it belongs to whoever finds a reachable case: widening
`open_readonly` on a hypothetical trades a working degrade — a number for a
store that cannot be opened any other way — for a refusal, on no evidence.

One limit the test states about itself: the `-shm` it copies out from under a
live connection is coherent, where a real kill can leave a torn one. A torn
index forces SQLite's recovery path, which needs directory write access — the
case where the read-only open really could fail, and the one no test can
produce deterministically.

## Transcript and digest readback (#722 slice B2)

`session_digest` reduces a finished worker's transcript to friction windows, and
it does that behind **one normalizer per CLI**, all of them producing the same
`digest::TranscriptEvent`. OpenCode is the third. Before it, an opencode agent's
digest was the flat refusal `session_digest does not support agent CLI
"opencode"` — and, less visibly, every opencode session contributed nothing to
the #324 recurrence scan, which drops any session whose transcript will not read.

The conversation lives in two more tables of the store slice B already reads, so
the reader is one more query through the same `open_readonly`, never a second
connection route: `message JOIN part`, scoped to one session, ordered by the
vendor's own indexes — `message(session_id, time_created, id)` and
`part(message_id, id)` (`SOURCE`, `session/sql.ts`). Ordering by id string is
sound *here* and not at the session level: message and part ids are minted
`ascending`, while session ids may be minted with an inverted timestamp
(`SOURCE`, `id.ts` — see *Session identification*).

**Three shape facts do all the work**, each `SOURCE`-verified at the pin in
`packages/schema/src/v1/session.ts`:

- **A message carries no text.** Both a human's prompt and an agent's reply are
  `part` rows. So the message document is read for exactly one field — `role` —
  and the digest's `initial_prompt` is the first user *text part*.
- **One `tool` part carries the call AND its outcome**, as a `state` of
  `pending | running | completed | error`, where `completed` holds `output` and
  `error` holds `error`. Claude writes those as two blocks a message apart. So a
  finished tool part expands into **two** events — the `ToolCall` every friction
  signature pairs against, then the `ToolResult` — and an unfinished one emits
  the call and no result. That asymmetry is deliberate: a call with no outcome
  is exactly what a session that died mid-tool looks like, and manufacturing a
  clean result would delete the wall from the digest rather than record it.
- **Text parts can be `synthetic` or `ignored`** — scaffolding OpenCode injects
  on the model's behalf, and text it has already decided not to send. Neither is
  anything a human or an agent said, and `initial_prompt` is the first user text
  in the stream, so admitting one would put a machine-written string where the
  task brief goes.

**One shared predicate widened rather than a parallel one added.**
`is_edit_tool` matched `Edit`/`Write`/`MultiEdit` — Claude's spellings — and
OpenCode names the same two `edit`/`write`, so the reverted-edit signature was
structurally dead for it. The predicate is now case-insensitive. The alternative
was rewriting OpenCode's tool names to Claude's inside the normalizer, which
would put a name in the event stream that the transcript does not contain, for
every consumer downstream, to satisfy one predicate. Nothing else moves: no
Claude tool name differs from those three only by case (`NotebookEdit` is the
near-miss, and still misses), and Copilot's normalizer emits no tool calls at
all, so the predicate is unreachable there.

**The edit tool's path key is genuinely ambiguous at the pin**, and the
normalizer reads both spellings rather than choosing. Two tools are named
`edit`: the v1 one whose runner writes these rows takes `filePath`
(`SOURCE`, `packages/opencode/src/tool/edit.ts`), the newer core one takes
`path` (`SOURCE`, `packages/core/src/tool/edit.ts`). Backing one would switch
the reverted-edit signature off silently the day the other wins — the single
failure a digest cannot report about itself, since a digest with no windows and
a session with no friction look identical.

**Degrading and erroring are different questions here.** `Unavailable` stays a
degrade on the polled `group_usage` path, where the right answer to a missing
store is "no number this tick". A digest is one deliberate call whose entire
product is the transcript, so there it surfaces as an error naming the session
and the reason — matching the claude arm's "no Claude transcript found". An
empty digest would read as *this worker hit no friction*, which is a claim about
the session made from evidence nobody read. Recurrence is unaffected either way:
`corroborating_session_keys` already drops a session whose transcript will not
read, so a missing store shrinks `sessions_scanned` rather than failing another
agent's digest.

## The Sessions browser (#722 slice C2)

The sidebar lists resumable sessions from every CLI's own store, and opencode's
is a database rather than a directory of transcripts. Three decisions carry the
weight.

**It reads the human's GLOBAL store, not any group's.** `OPENCODE_DB` is set
per group, so a group's sessions live in `opencode_db_path(group)` and a *solo*
pane — the only kind this sidebar can reopen at all — writes where an unadorned
`opencode` writes. Which file that is, is the vendor's own resolution ported
verbatim (`sessions::opencode_store_from`): `OPENCODE_DB` first (absolute as-is,
a bare name under the data directory), otherwise `<xdgData>/opencode/opencode.db`
with `xdgData` = `XDG_DATA_HOME` or `<home>/.local/share` — that fallback on
Windows too, which is why the observed path there is
`%USERPROFILE%\.local\share\opencode`. Only the default channel's file is read:
a listed row is worth showing only if the resume command beside it works, and
that command is a bare `opencode`, which reads exactly that file. Group sessions
are deliberately absent — they are reopened *through the group*, with its
roster, board and MCP identity, never as a bare `--session` pane. The
affordance that actually reopens one is the session browser's
**Orchestrations** section (`orch_list_recorded`, #1563): it reads loomux's own
record of each group rather than any CLI's store, which is the only way a
group-store session can be surfaced at all.

**No index entry, and that is not an oversight.** `session-index.json` (#493)
exists to avoid re-reading the head of a transcript that has not changed. There
is no such cost here — one indexed `SELECT` returns every column, for every
session, at once — and reusing the index would be actively wrong-shaped: it is
keyed by file path and validated by `(mtime, len)`, so all N sessions would
collide on one key and invalidate together whenever any one of them was
written. The `LIST_LIMIT` cut is pushed into the SQL instead, and the merge is
a re-sort of the whole list rather than an append, so the limit keeps meaning
"the newest N sessions on this machine" rather than N per source.

**The frontend widened in the same change, because a row is not typed on the
wire.** `SessionInfo.source` is a plain string over IPC: the backend's set and
the frontend's `"claude" | "copilot"` union were never checked against each
other, so a third scanner shipping alone would have been silently mis-handled,
not rejected — an opencode session named `copilot · …` in the pane title, and
resumed with `claude --resume ses_…`. What moved with it: the reconciler's
`Cli`, `Pane.agentCli` (now `panerestore::sessionCliFromCommand`, so the set is
spelled once), and `agentResumeCommand`/`agentFreshCommand`, which have to know
that opencode names a session with `--session` and has no flag that
pre-assigns one — so its "start fresh with the same identity" arm keeps no
identity, because there is none to keep.

`panerestore`'s `SoloCli` deliberately did **not** move. It is bound to
`CliCaps::mcp_argv_seam`, not to what loomux can scan: opencode has no argv MCP
seam, a solo opencode pane is delivery-only from birth, and its recorded
command carries no identity flags for `stripSoloMcpFlags` to excise. That type
widens when the seam arrives, not when the sidebar learns to list the CLI.

## Deliberately not done

- **Reasoning parts in the digest.** OpenCode stores the model's reasoning as
  its own part type. It is skipped for the same reason `push_claude_block`
  skips Claude's `thinking` blocks: the digest reduces what an agent *did*, and
  a scratchpad is the noisiest thing in a transcript per byte of signal.
- **Solo-pane full channel membership** (#288). Not a gap: a solo launch
  appends flags to a command line the human owns and cannot set environment,
  and opencode has no MCP flag. Recorded as a decision in
  `doc/design/cross-workspace-channel.md`'s matrix.
- **`persona.extra_allow` translation.** Those are claude/copilot tool-pattern
  strings; opencode's permission keys and matcher are a different namespace, so
  translating them would be inventing semantics. Same decision, same reason, as
  gemini's arm. An opencode block widens nothing; it only ever gets its class's
  baseline.
- **Driving `opencode serve` over HTTP** instead of a PTY+TUI pane. A different
  integration architecture from every other CLI; loomux is a terminal
  multiplexer and panes are the product.
- **A compact-nudge banner.** `auto_compact_banner_substrings` and its copilot
  counterpart are *observations of a live pane*, and constraint 3 forbids an
  agent spawning one to collect them. opencode gets an empty slice honestly,
  like gemini, rather than a guessed literal that would silently match nothing.

## Still for the human (live only)

1. **Zen entitlement.** The model id is settled — `opencode/deepseek-v4-flash-free`
   (`DOCS` Zen page format + the catalog opencode itself consumes) — but
   whether the account can call it needs a real run.
2. **One manual native-Windows group spawn**: boot dialogs, kickoff
   paste/submit, how the permission footer renders against the question-guard
   heuristics, `--auto` in-pane, loomux MCP tools visible, resume round-trip.
   The vendor recommends WSL over native Windows, so this is also where that
   risk is settled.
3. Whether the TUI statusline shows tokens/cost — display-only; usage does not
   depend on it, since the store carries per-session cost and token counters.
