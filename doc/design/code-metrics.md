# Code metrics: what CI measures about this repo's own code

`scripts/code-metrics.cjs`, the `code-metrics` job in `.github/workflows/ci.yml`,
`.github/clippy/clippy.toml`, and the sticky `<!-- code-metrics -->` comment on every
pull request. Spec and rationale for all of it (#2138, from the #2128 investigation).

This is **feedback about developing orrerix**, not a product capability. CLAUDE.md
constraint 8 puts it entirely in repo config — `scripts/`, `test/`, `.github/`,
`doc/design/` — and nothing here touches `src-tauri/`, `crates/` or `src/`.

## Why it is a report and not a gate

The obvious thing to build is a gate: fail the build when a function is too long. The
reason this slice deliberately does not is that **no threshold could be chosen
honestly yet**. Before this job existed, nobody in this repo knew what the function
length distribution WAS — #2128 part 1 measured file sizes with `wc` and had to
estimate per-function lengths from the gaps between `fn` lines, because no parser-level
instrument ran anywhere. A threshold picked without the distribution is a number
someone liked the look of, and the first thing it does is block correct work.

So the order is: measure, watch, then gate. #2128 slice C proposes three ratchets —
new function over the tree's p95, new import cycle, new `.unwrap()` in product Rust —
each keyed on a **new entity**, so existing debt never blocks, and each shipping with
the false-block count it accumulated in report mode. This slice is what produces that
count. The `workflow_dispatch` `shas` input exists for exactly that: it runs the report
over a list of already-merged commits, so a proposed gate can be tried against known-
good subjects before it is armed (CLAUDE.md: "a guard that REFUSES ships only after it
has run clean over known-good subjects").

Report-only is structural, not a promise:

- every step of both jobs is `continue-on-error`;
- nothing in the merge gate `needs` either job;
- no number in the script is ever compared against a threshold;
- an unreadable clippy message produces a **missing row**, never a number, and the
  output carries `messagesSeen` beside `parsed` so a parser that stopped matching is
  visible rather than silent.

## What is measured, and with what

### TypeScript — the `typescript` compiler API

Already a devDependency; the API gives every number below in a few hundred lines, which
is why `eslint`'s complexity rules (about a hundred packages for two rules) and
`dependency-cruiser` were rejected in #2128 part 5.

`ts.createSourceFile` per file, never a `Program`: nothing here needs a type checker,
and building one over 150-odd files would cost seconds this job should not spend.

| Row | How |
| --- | --- |
| Function lines | `fn` start line to closing brace, from the AST. A nested function is counted in its parent's span AND gets its own row, the same convention clippy's `too_many_lines` uses. |
| Nesting depth | Maximum depth of branching constructs inside the body, relative to the body. `if`/`for`/`while`/`do`/`switch`/`try`/`catch`/ternary count; a bare block, an object literal and a class body do not. A nested function starts its own scale. |
| Argument count | `node.parameters.length`. |
| Import graph | Relative specifiers only, resolved against the scanned file set (`./x.ts`, `./x`, `./x/index.ts` all resolve). Bare specifiers are external and produce no edge. |
| Import cycles | Tarjan strongly-connected components over that graph, iterative. A component of two or more, or a self-import. |
| Exports with no importer | An exported name that no import specifier anywhere in `src/`, `test/` or `e2e/` names, and whose file is not namespace-imported or `export *`-ed. |
| Fan-in / fan-out | Edge counts per file. |
| Lines / comment / blank per root | A comment line is one whose first non-blank characters are `//` — the same definition #2128 part 1 measured by hand, so these numbers continue that baseline rather than starting a new one. Block comments are not tracked. |

**Known limits, stated because the rows are read by people.** Dead exports are decided
from import specifiers with no type checker, so a dynamic `import()`, a consumer outside
those three roots, and an entrypoint reached from `index.html` all read as dead —
`test/codemetrics.test.ts` pins the entrypoint case as a fixture rather than leaving it
as prose. Nesting depth is a structural count, not cognitive complexity; the TS side has
no equivalent of clippy's `cognitive_complexity` and none is claimed.

### Rust — clippy's JSON

`cargo clippy --message-format=json`, parsed by the same script. No new dependency:
clippy ships with the toolchain. `rust-code-analysis-cli` was considered and kept only
as a documented fallback (#2128 part 5) — its releases are stale and it needs a
`cargo install`, which is a build.

The three lints that carry numbers are **threshold lints**: they fire only above their
threshold, and the value they measured appears **only in the message text** ("this
function has too many lines (140/1)"). There is no clippy mode that just prints the
number. So the job points `CLIPPY_CONF_DIR` at `.github/clippy/clippy.toml`, whose
thresholds are all `1`, every function fires, and the parser reads each value back out
of the message.

Two consequences worth stating:

- **`--force-warn`, not `-W`.** These lints are allow-by-default, so they must be
  turned on; and 20 `allow(clippy::…)` attributes already sit in the tree (#2128 part
  3), which `-W` would honour — leaving exactly the functions someone already thought
  were too big unmeasured. `--force-warn` overrides the attribute.
- **The parser's contract is with clippy's wording.** `CLIPPY_VALUE_PATTERNS` in the
  script is that contract. If a clippy release rewords a message, the row goes MISSING
  and `unparsed` says so; it never becomes a wrong number. A fixture in
  `test/fixtures/codemetrics/clippy-ubuntu.json` carries a deliberately unreadable
  message to pin that behaviour.

`unwrap_used`, `expect_used` and `panic` are counted per file from the same run.

`clippy.toml` lives under `.github/clippy/` and not at the repo root on purpose: the
env var scopes it to one step, so `cargo check`, `cargo test`, a local `cargo clippy`
and every product build see none of it. A root `clippy.toml` with thresholds of 1 would
make every human clippy run unreadable.

### Why clippy runs on every build leg

A `cfg(windows)` body is invisible to a clippy run on ubuntu, and this project has a
lot of platform-specific code. Each leg uploads a compact per-leg JSON; the
`code-metrics` job merges them and keeps the **larger** value per function, so a
`cfg`-gated body is never under-reported. `test/codemetrics.test.ts` pins that merge
against two fixture legs that disagree.

If a leg ever costs more than it is worth, the honest fix is to drop that leg **and**
record here that its `cfg` bodies are unmetered — not to leave the merged number
looking complete. The measured per-leg cost is in the table below.

## The artifact

`code-metrics.json`, uploaded by the `code-metrics` job with 30-day retention. It is a
**persisted schema** read by the delta comment and, later, by
`scripts/orch-scorecard.cjs` (#2128 slice D), so its top-level keys are a contract, not
an implementation detail, and `test/codemetrics.test.ts` pins them:

```
schemaVersion  generator  commit  ref  generatedAt  ts  rust  roots  modRs  diff
```

`schemaVersion` is `1`. Bump it when a reader could misread an old file as a new one.
The sticky comment's marker string, `<!-- code-metrics -->`, is part of the same
contract: changing it orphans every comment already posted.

## The per-PR delta comment

One comment per PR, found by its marker and edited in place — no marketplace action,
just `gh api`. The search matches a comment **by the workflow's own bot** whose body
starts with the marker, so a human quoting the marker cannot capture the slot.

**Both sides are measured.** The comment never derives a base figure by subtracting a
delta from a head figure; that is the CLAUDE.md rule this instrument exists to serve.
The base is resolved in three arms, in order:

1. the base SHA's own stored `code-metrics.json` artifact, found through the Actions
   API;
2. a second measurement, taken here, on a `git worktree` checked out at the merge-base.
   The **instrument stays the head's** — the script is run from the PR's checkout with
   `--repo-root` pointed at the base worktree — because a script that changed shape
   with its subject would make the two sides incomparable. `require('typescript')`
   resolves from the head checkout for the same reason, so the base tree needs no
   install. No clippy on this arm: it is a full workspace build, and `n/a` on the Rust
   rows is the honest outcome;
3. `n/a` — the comment prints "base unavailable" and claims no delta.

**B never fails on a missing base.** That is the acceptance criterion, and it is why
arm 3 exists at all rather than an error.

Rows: the percentile table (base → head per cell), functions NEW at head above the base
p95 by name, import cycles new at head, added lines with their comment share, new
`.unwrap()`/`.expect(`/`panic!(`/`allow(clippy::…)` on added **product-Rust** lines
only, and `orchestration/mod.rs`'s delta. Every row says it is report-only.

`.github/agents/rev-std.md` and `rev-final.md` each carry one section telling the
reviewer to read the comment and treat a new function over the base p95, a new cycle,
or a new product `unwrap` as a finding to **raise** — reproduced like any other, never
a verdict on its own.

**The comment is the instrument, not a second surface for the base-and-head rule.**
CLAUDE.md's "every number in a PR body is measured at the base AND at the head" stays
on PR-BODY numbers. This comment is where a worker can get them (#2128 part 11).

## Job scopes

`ci.yml` declared no job-level `permissions` before this. The `code-metrics` job
declares exactly three: `contents: read` for the checkout, `actions: read` to find the
base commit's run and download its artifact, and `pull-requests: write` for the one
sticky comment. PRs here come from same-repo branches, so the token is not the
read-only fork variant (#2128 part 6d).

## Measured distributions

<!-- MEASUREMENTS -->

## What slice C will gate on, when it does

Nothing yet. #2128 part 7's proposal, restated so a future reader does not have to dig:
C1 = new function over the tree's p95 for length and complexity; C2 = a new import
cycle; C3 = a new `.unwrap()` on an added product-Rust line. Each is keyed on a NEW
entity so existing debt never blocks, each threshold is a constant in the CI config
with the measuring run id beside it, and each PR that arms one carries its own
report-mode false-block count since this slice landed.

Deliberately **not** gated, and recorded here so nobody re-proposes them as oversights:
`orchestration/mod.rs` may not grow (#2128 part 7 measured two false blocks in ten
merged PRs — it stays a delta column); comment share of added lines (density is not
why-ness); a `src/` module without a test twin (DOM glue is hand-validated by
convention, CLAUDE.md).
