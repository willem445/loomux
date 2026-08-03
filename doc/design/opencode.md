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
([1](https://github.com/willem445/loomux/issues/722#issuecomment-5161943081),
[2](https://github.com/willem445/loomux/issues/722#issuecomment-5161943414),
[3](https://github.com/willem445/loomux/issues/722#issuecomment-5161943777));
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
                       "enabled": true, "headers": { "X-Loomux-Agent": "<token>" },
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

The reader itself is not in this slice; this is the seam it lands on.

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
whatever the CLI's own `--help` reports.

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

## Deliberately not done

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
