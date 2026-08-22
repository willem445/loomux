# Filesystem identity after the rename (#1153 phase 4)

The rename from *loomux* to *Orrerix* is free everywhere the name is only text.
It stops being free at three places where the name is **an identity on somebody's
disk**, and those three are not one problem with one answer:

| Identity | Whose is it? | Policy |
| --- | --- | --- |
| `<platform data dir>/loomux` — the app's own state root | **Ours.** Only orrerix writes it. | **Moved once**, on the first launch that finds only the old name. |
| `<repo>/.loomux/` — `workflow.yml`, `lessons.md`, `workflow.layout.json` | **The user's**, and it is *committed* — it lives in their git history, on their branches. | **Discovered, never moved.** `.orrerix/` preferred, `.loomux/` read forever. |
| `%LOCALAPPDATA%\loomux\whisper\` — the opt-in voice runtime | **The user's** — a whisper.cpp build and a ggml model they downloaded by hand. | **Discovered, never moved.** |
| `LOOMUX_*` environment variables | **The operator's** — shell profiles, CI configs, wrapper scripts we cannot edit. | `ORRERIX_X` preferred, `LOOMUX_X` read forever. |

The dividing line is ownership, not effort. We may rename a directory we are the
only writer of. We may not rewrite a directory that is tracked in someone else's
repository, or relocate files they downloaded and wrote scripts around.

Every decision below is a **pure function** — `brand::pick_env`,
`brand::pick_repo_path`, `obs::plan_default_root` — with the filesystem probes
supplied by the caller. That is what makes each policy one `match` arm to change
and one test to read, rather than something you have to reconstruct from I/O code.

---

## 1. The data root: move once, on first launch

**Shipped policy: move-on-first-launch.** `<data>/loomux` is renamed to
`<data>/orrerix` by the first launch that finds the old name and not the new one.

> **Status: RATIFIED by the human (#1153 q-4).** Phase 0 flagged move-vs-fallback
> as a human checkpoint, and this shipped as the plan's recommended default while
> that answer was outstanding; the human has since confirmed move-on-first-launch.
> The *Reverting* section below stays — a ratified policy is still one somebody may
> need to undo, and the escape hatch is what made shipping it ahead of the answer
> defensible in the first place.

### The decision, in full

```
new exists            -> use new                     (never touch the old one again)
only legacy exists    -> rename legacy -> new; use new
  ...rename refused   -> use LEGACY for this run; retry next launch
neither exists        -> use new                     (first-ever launch)
```

Three properties do the work:

- **It is one `fs::rename`.** A directory rename within a volume is atomic: either
  the whole profile moved or none of it did. There is no copy loop to be
  interrupted halfway through somebody's orchestration history, and no
  half-migrated state to recover from.
- **A refused rename falls back to the OLD root, not to a fresh one.** This is the
  arm that matters most and the one an obvious implementation gets wrong: starting
  an empty profile because a rename failed is indistinguishable, to the user, from
  "all my groups are gone".
- **Nothing is ever deleted, on any path.**

### Why not read-old-fallback-forever?

Because the data root is not one file, it is a *tree* of singletons —
`orchestration/<group>/`, `logs/`, `tabs.json`, `session-index.json`,
`running.lock`. A permanent fallback means every one of them needs a rule for
"which root does this live in *this* time", and the moment anything is written
under the new name while the rest sits under the old one, the profile is split.
The `.loomux/` config dir tolerates per-file resolution precisely because its
files are independent of each other (§2); the data root's are not.

### The hazard: an old instance running during the swap

Phase 0 named this, and it is real. A pre-rename build can be running with
`<data>/loomux` open — `running.lock` held, log files open, a live group's
`audit.jsonl` being appended to — at the moment a new build renames it.

We cannot detect that reliably. `running.lock` is exactly the wrong signal to
gate on: it is left behind by a crash, so "lock present ⇒ do not migrate" is an
unbounded suppression driven by a fallible signal, and a user who crashed once
would never migrate again. So instead of detecting the case, the design makes it
**non-destructive in every branch**:

- **On Windows** — the platform this ships on — the rename simply *fails*. A
  directory containing an open file handle cannot be renamed, so the OS arbitrates
  for us and we take the `UseLegacy` arm. This is the good outcome, and it is the
  most likely one.
- **On POSIX** the rename succeeds. The old instance's *already-open* handles
  follow the moved inode — same files, new name, no corruption. Any path it
  re-opens afterwards lands in a freshly recreated `<data>/loomux`. So the failure
  mode is a *split*, not a loss: some state written by that old instance stays
  behind under the old name.
- **The next launch then sees both roots and takes `UseNew`** — leaving the stray
  directory on disk, untouched, for a human to look at. Nothing overwrites it and
  nothing deletes it.

The residual cost is therefore bounded and named: state written by a *concurrently
running old build*, on a *non-Windows* platform, during the *one* launch that
migrates. Not zero — but recoverable by hand, and every alternative we considered
(copy-and-verify, lock-file gating, a first-run prompt) trades that for either a
non-atomic migration or an unbounded suppression.

### The signpost

After a successful move the old directory is recreated containing one file,
`MOVED-TO-ORRERIX.txt`, naming the new location and saying how to undo the move.
This is not decoration: it is what makes the migration reversible **by a human**
rather than only by us. Someone who goes looking for `<data>/loomux` — because a
script points there, because they rolled back to an older build, or because that
is simply where their data used to be — finds an explanation instead of an
absence. Writing it is best-effort; a signpost we could not write is a worse
experience, not a failed migration.

### Where it runs, and where it deliberately does not

`obs::init_data_root()` is called once, early, from `src-tauri/src/lib.rs::run()`
— **before** `check_and_arm` (which writes `running.lock` into the root) and before
the startup breadcrumb (which writes into `<root>/logs`). Either of those going
first would pin the old root for the whole process and defer the move a launch.

It is *not* called from `obs::data_root()`. A rename of the user's live profile is
not something a getter invoked on every breadcrumb write should be able to trigger
— and concretely, `data_root()` is reachable from unit tests, integration tests and
the daemon's config probing, so a lazy migration would mean running the test suite
could rename the profile of the app the developer has open.

The contract is *enforced*, not merely requested: `init_data_root()` and the
read-only `resolve_default_root()` initialize the same `OnceLock`, so whichever
runs first decides, exactly once, and the answer cannot change under a running
process. If something reads the root before startup reaches `init_data_root()`, no
move happens that launch and the old root keeps being used — the same safe state as
a refused rename.

`crates/loomux-server` does **not** call it. Its `state_root()` is documented to
touch no disk, because `--check-config` must be free of side effects, and the
daemon has no serve loop yet. When it grows one, `init_data_root()` belongs at the
top of that loop.

**An explicit `ORRERIX_DATA_DIR` / `LOOMUX_DATA_DIR` is never migrated** and never
probed for a sibling. An operator who names a root has named the root, and an E2E
run's isolated profile must stay isolated. `init_data_root()` returns immediately
when one is set — not merely `data_root()`, which is the distinction that matters:
a startup that resolved the *override* for its own use while still migrating the
*platform default* would rename the user's real `<data>/loomux` out from under the
app they have open, which is the one directory #394's override exists to keep its
hands off.

Both guards read that condition through the **same** function, `override_root` —
which is also the only place a bad value is rejected and reported. Two guards
deciding "is an explicit root in force?" by their own slightly different rules
would be a bypass exactly the width of the difference, so there is one rule and
one definition, and `a_rejected_override_is_not_treated_as_an_explicit_root` pins
that they cannot drift apart.

### Reverting

**The procedure, stated once and identically everywhere it appears** (this note,
the `RootPlan` doc comment, `brand.rs`'s module doc, and the PR body):

> In `obs::plan_default_root`, change the `(false, true)` arm from
> `RootPlan::Migrate` to `RootPlan::UseLegacy`. Then update this note.

That is the whole edit. `root_action` dispatches `UseLegacy` to
`RootAction::UseLegacy`, which uses `<data>/loomux` as it stands and renames
nothing; `resolve_default_root()` already resolves the same way on the read-only
path, so the two `OnceLock` initializers agree and no caller learns the
difference.

**Why this section is emphatic about *which* function.** The first cut of this
change folded `UseLegacy` in with `Migrate` in the dispatch, on the reasoning
that `plan_default_root` could never return it — which made the documented
revert **inert**: the edited arm still reached `migrate_default_root`, so a
maintainer who declined the policy would have shipped a build they believed
never migrated while every user's data root moved anyway (and the two
initializers would have disagreed, with the app's startup order picking the
migrating one). A comment whose premise the documented next edit voids is not a
guard. `UseLegacy` now has its own dispatch arm, and
`the_documented_revert_really_stops_the_migration` plus
`exactly_one_plan_variant_moves_anything` pin it — the first simulating the
revert at its own seam, the second asserting that the set of variants which move
anything is exactly `[Migrate]`, so a future arm cannot be quietly folded back
in. Found in review (rev-lead round 1, B1), not by the author.

---

## 2. The repo config dir: discovered per file, never renamed

`.orrerix/` is preferred; `.loomux/` is read when the preferred spelling is not
there. That is the whole rule, and it is permanent. The reason it can never become
a migration is one sentence: **that directory is tracked in the user's git
repository**, so "migrating" it would mean writing a commit-shaped change into a
working tree we do not own, on a branch we did not pick, in the middle of whatever
they were doing.

Two details are load-bearing:

- **Resolution is per FILE, not per directory.** A repo may legitimately hold
  `.loomux/lessons.md` and `.orrerix/workflow.yml` at once, and both are read. The
  alternative — pick a config *dir* first, then look only inside it — means adding
  `.orrerix/workflow.yml` to a repo silently stops its `.loomux/lessons.md` from
  being read. Silently ignoring a file the user still has is worse than a mixed
  directory, and it lets a repo migrate one file at a time and see each move take
  effect immediately.
- **With both present, the preferred one wins.** If the legacy file won, adding
  `.orrerix/workflow.yml` would have no effect until its author *also* deleted
  `.loomux/` — the "why is my edit being ignored" trap a fallback exists to prevent.

### Naming what was actually read

`workflow::workflow_path(repo)` and `lessons::lessons_path(repo)` return the
spelling that repo really uses, and every surface that *reports* a path calls them:
the audit records (`workflow-loaded`, `workflow-invalid`, `workflow-ignored`,
`workflow-changed-since-launch`), the launcher's preview, the workflow-mode toggle's
refusals, the orchestrator's roster and lessons notes, and the workflow section of
its instructions. A resolver that reads the right file while *reporting* the wrong
one sends a human — or an agent — to edit a file that does not exist. Where no repo
is in hand at all (the generated merge-gate file's header comment), the text names
the file generically rather than guessing.

The canvas layout file follows the same rule by construction:
`layoutFileFor(workflowRel)` derives it as the workflow file's **sibling**, so the
two cannot separate whichever spelling — or explicit path — the pane is showing.
That is a fix as well as a rename: the previous hard-coded constant would have
written a `.loomux/`-repo's canvas positions into `.orrerix/`.

In the pane, the fallback is conditional on the legacy read *succeeding*. A repo
with neither file stays on the preferred path, so the "no workflow yet" empty state
offers to create `.orrerix/workflow.yml` — never the deprecated name. A
`binary`/permission error is not a fallback trigger either: that file *is* there,
and quietly opening a different one would hide it.

**This repo's own `.loomux/` is deliberately untouched.** Renaming it is a
human-owned repo change, and doing it here would have swapped the workflow file out
from under every orchestration group running against this checkout. A useful side
effect: the existing backend suite, which creates `.loomux/` fixtures throughout,
now exercises the legacy path end to end without a line of it changing.

---

### The whisper directory, and what "never moved" costs

`%LOCALAPPDATA%\<name>\whisper\` follows the same rule and the same function
(`brand::pick_repo_path`): preferred name first, legacy name when only it is
there, never moved. The plan's Class-C enumeration missed this directory; it is
filesystem identity, and shipping the voice *env-var* rename while leaving the
*directory* unmentioned would have been a half-rebrand.

The honest cost, recorded because it is a real user-visible consequence rather
than a hypothetical: `scripts/stage-whisper.ps1`'s `-Dest` default now points at
the new name, so a user who already staged under the old one and re-runs the
script downloads a **second** several-hundred-megabyte copy instead of updating
the one they have. That is the correct consequence of "never moved" — the
alternative is relocating files we did not put there — and it is called out in
`docs/features/voice-prompts.md` with the `-Dest` override that avoids it.

**It has no test of its own.** `local_whisper_dir` lives inside `mod win` and
resolves against the real `%LOCALAPPDATA%`, so there is nothing to point at a
temp tree. The *rule* it applies is well covered (`pick_repo_path`, all four
crossings); what is unpinned is the wiring — that this function calls that rule
— and that is stated here rather than left for a reader to assume otherwise.

## 3. Environment variables: presence wins, on both sides alike

`brand::pick_env` reads `ORRERIX_X` and falls back to `LOOMUX_X`. The rule is
**presence**: if the `ORRERIX_` name is set at all it wins.

Deliberately *not* "the first non-empty one". That would make the two names obey
different rules depending on their contents, and an operator who sets
`ORRERIX_DATA_DIR=` to blank out an inherited `LOOMUX_DATA_DIR` — a normal thing to
do in a CI job or a wrapper script — would silently get the inherited value back.
Empty and malformed values are the *consumer's* business: `obs::data_root_from`
already rejects an empty or relative data dir and says so on stderr. This function
must not pre-empt that by substituting a different variable's value for the one
that was actually set.

Dual-read applies to the **user-documented** variables — `DATA_DIR`,
`WHISPER_{CLI,MODEL,ARGS,PROMPT}`, `VOICE_KEEP_WAV`. CI- and test-only variables
(`LOOMUX_E2E*`, `LOOMUX_LEAK_*`, `LOOMUX_NO_*`, `LOOMUX_PERF*`, `LOOMUX_TEST_VAR`)
have no external contract and rename with their own code, whenever that code moves.

Every message that names one of these variables names **both** spellings, via
`brand::env_names`: a user who set the legacy name and got the value wrong must not
be told to check a variable they never set, and a user who set neither must be told
the current name first.

---

## What this phase deliberately does not touch

Phase 3 owns the protocol identities — the notice marker (`[loomux]`, not
`(loomux)`: the parenthesised form is what the anti-forgery sanitizer PRODUCES,
and the phase-0 plan's spelling of it was wrong), the MCP server name and token
header, the audit actor, the shim internals, and the agent-visible group-dir /
agent-id exports. Nothing here anticipated that; it has since landed — see
[`rebrand-protocol.md`](rebrand-protocol.md), which states the rule this phase's
dual-discovery could not use: emit one spelling, accept every spelling. Phase 5
owns the published identities (`productName`, the bundle identifier, the npm package, the
GitHub repo), which are human-performed.

`brand.rs` is meant to shrink and eventually die. Every `LEGACY_` constant in it is
a deprecation, and deleting one is a deliberate, separately-argued break of
somebody's working setup — not tidying.
