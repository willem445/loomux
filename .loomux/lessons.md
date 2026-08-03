# Repo-recorded lessons

Rules that must survive past the group that learned them. Rule + fix only —
history and rationale live in the referenced issues/PRs and `doc/design/`.
See `doc/design/lessons.md` for the write path, injection point, and trust
posture. Newest at the bottom. Only ~4 KB reaches a kickoff: whole entries
drop oldest-first past that (a `## ` heading is the eviction boundary), and
`[pinned]` in a heading moves an entry to the back of the queue (#498). Pin
only what breaks the build or the machine if it goes missing — over cap,
pinning everything just restores eviction by position.

## No getrandom-based crates in src-tauri [pinned]

No `uuid` v4, `rand`, `tempfile`-with-defaults, or anything pulling `getrandom` into
`src-tauri` — the binary fails to load (`0xc0000139`) on the Windows 10 baseline. Use
std's OS-seeded `RandomState` for ids/tokens. Before adding a dependency: check
`src-tauri/Cargo.toml`'s notes and run `cargo tree -e normal --target all -i
getrandom@<version>`.

## Never resize the PTY for a UI feature [pinned]

UI features are overlays or header/board chrome floating over the terminal — never a
resize. Visual padding goes on the `.xterm` element, not the layout.

## Never `git stash` in a loomux worktree [pinned]

The stash stack is shared across ALL worktrees of a repo (#299). Commit WIP to your own
branch instead. If you must stash: `git stash push -m "<your agent id>: ..."` and only
`pop` entries carrying your own marker.

## A claim is a deliverable

A comment, design note, audit label, or PR body claiming something the code doesn't do is
a defect (#461, #490) — delete it, don't soften it. Quote raw fetched text for CLI/API
facts, never a paraphrase (#453).

## Commit real work BEFORE capturing red evidence [pinned]

`git checkout -- <file>` discards ALL uncommitted work in it, not just your evidence
patch (#493). Commit the real work first (amend/squash later), then patch for the red
run, then restore — never point a destructive git command at a file holding anything
uncommitted.

## Any suppression driven by a fallible signal must be BOUNDED

A guard holding an action "while X is true" needs an answer for "X never clears" (#496,
#513, #518). Fix the signal AND bound the consumer; prefer releasing on independent
evidence over elapsed time; extending to "every consumer" means grepping the subsystem
and publishing the list.

## `rustfmt --check` is the one local Rust check agents may run

From `src-tauri/`: `rustfmt --check --edition 2021 <changed .rs> >/dev/null`. `--edition`
is mandatory (default 2015 false-errors `async fn`); discard stdout (unenforced
formatting diffs); exit code is ambiguous (1 = parse error OR formatting diff) — **stderr
is the signal, don't grep for `error:`**. Parse check only: never run bare `rustfmt`,
commit a reformat, or cite a clean run as validation. `cargo check` stays banned (#488,
#558).

## Never block a turn waiting on CI

A `[loomux]` notice arrives by typing into a pane; a mid-turn pane can't take delivery, so
waiting on CI queues the answer behind the turn itself (#590). Register the watch, END the
turn, act on the notice. Applies to any answer that arrives as a pane delivery — reading
state once is fine, waiting is the defect.

## A PR body's claims are about a SHA and a scope

- After any push or rebase, re-derive every run citation for the NEW head (`gh run list
  --branch <branch> --json headSha,databaseId,conclusion`, assert headSha == `git
  rev-parse HEAD`) and update the body before reporting (#596). A rebased-away RED run
  keeps its own SHA plus a pre->post map — never relabel it (#695, #696).
- `Closes #N` closes on squash regardless of partial-scope prose; partial scope links as
  `Part of #N` / `Mitigates #N`.
- The scan is textual, context-blind: `close`/`fix`/`resolve` next to `#N` fires even in
  blockquotes/caveats/aggregated commits. Grep body and `git log` for it before
  opening/updating a `Part of` PR, and reword (#569, #615).
- Prose written against a plan stays dated to it, even across a rebase onto its own
  subject: re-read every doc/design-note/`SKILL.md` claim against what a sibling slice
  actually SHIPPED, never the plan both were written from, before marking ready
  (#715, #721).

## A model that re-implements the algorithm proves the algorithm, not the code

A property/mutation test over a model bounds the design; only a test executing the real
function bounds the code (#606). Before crediting a path's tests, grep for its
constructor and ask what constructs THAT — if nothing a headless test can build, move the
logic somewhere reachable (a newtype with unit tests), don't write a better comment.

## `/tmp` is one namespace shared by the whole fleet

Every agent shares one `/tmp` and reaches for the same filenames; the second writer wins
silently (#625). Scratch files live under the agent's own worktree (`./.scratch/`,
gitignored) — never a bare `/tmp` name.

## A green suite's coverage claim is a claim like any other

When a PR body or code comment claims a test/mechanism polices a property, run the one
mutation that removes it and watch WHICH tests redden — a match is evidence, a mismatch is
a correction; disclose it (#664, #673, #682). A red evidences only the assertion it
REACHED and MOVED — a panic before it, a split test's already-green half, or a companion
that also passed broken prove nothing; split the test, or say which half moved (#710,
#712, #727).

## A test's specimen must stay a member of the class it witnesses

When a directive moves a real specimen out of the class a test needs it in (a declared
value converging with the default; a file gaining its "absent" block; a concrete list
going stale), relocate the property onto a witness that still distinguishes — never relax
the assertion to fit today's specimen. If the converged case still deserves coverage, give
it its own strictly-weaker, explicitly-labeled assertion (#689).

## A subsystem isn't done until a production path calls it

Slice tests drive the seams, so a lifecycle nothing invokes stays green while doing
nothing. List each new lifecycle fn's call sites, discarding the module's own and the
tests' — nothing left means it's wired to nothing. Wire it, or name the deferred caller
and its issue in the PR (#661 `e20`, #698, #700).

## A multi-line shell script is a file, not a `-c` argument

Long inline Bash dies on Git Bash quoting (`unexpected EOF ... matching '`, reported 50+
lines into a compound command) from a quote you cannot see. Write it to `./.scratch/` and
run the file, or pipe prose via `--body-file -`. Re-trying inline is the second cost.
