# Repo-recorded lessons

Rules that must survive past the group that learned them. Rule + fix only —
history and rationale live in the referenced issues/PRs and `doc/design/`.
Newest at the bottom; headings are for readability only.

## No getrandom-based crates in src-tauri

No `uuid` v4, `rand`, `tempfile`-with-defaults, or anything pulling `getrandom` into
`src-tauri` — the binary fails to load (`0xc0000139`) on this project's Windows 10
baseline. Use std's OS-seeded `RandomState` for ids/tokens. Before adding a dependency:
check `src-tauri/Cargo.toml`'s notes and run
`cargo tree -e normal --target all -i getrandom@<version>`.

## Never resize the PTY for a UI feature

UI features are overlays or header/board chrome floating over the terminal — never a
resize. Visual padding goes on the `.xterm` element, not the layout.

## Never `git stash` in a loomux worktree

The stash stack is shared across ALL worktrees of a repo (#299). Commit WIP to your own
branch instead. If you must stash: `git stash push -m "<your agent id>: ..."` and only
`pop` entries carrying your own marker.

## A claim is a deliverable

A comment, design note, audit label, or PR body stating something the code doesn't do is
a defect (#461, #490). Delete a claim you can't point at code for — don't soften it.
Quote raw fetched text for CLI/API facts, never a paraphrase (#453).

## Commit real work BEFORE capturing red evidence

`git checkout -- <file>` discards ALL uncommitted work in the file, not just your
evidence patch (#493). Commit the real work first (amend/squash later), then patch for
the red run, then restore. Never point a destructive git command at a file holding
anything uncommitted.

## Any suppression driven by a fallible signal must be BOUNDED

A guard holding an action "while X is true" needs an answer to "what if X is wrong and
never clears" (#496, #513, #518). Fix the signal AND bound the consumer; prefer releasing
on independent evidence over elapsed time; when extending to "every consumer", grep the
subsystem and publish the list.

## `rustfmt --check` is the one local Rust check agents may run

From `src-tauri/`: `rustfmt --check --edition 2021 <changed .rs> >/dev/null`. The
`--edition` flag is mandatory (default 2015 false-errors `async fn`); discard stdout
(formatting diffs, unenforced); **stderr is the signal — read it, don't grep for
`error:`**. It is a parse check only: never run bare `rustfmt`, never commit a
reformat, never cite a clean run as validation. `cargo check` stays banned (#488, #558).

## Never block a turn waiting on CI — the resolution is queued behind the turn

A `[loomux]` notice is delivered by typing into a pane; a mid-turn pane can't take it, so
a turn blocked on CI waits on a resolution queued behind itself (#590). Register the
watch, END the turn, act on the notice. Applies to anything whose answer arrives as a
pane delivery. Reading state once is fine; waiting is the defect.

## A PR body's claims are about a SHA and a scope — the body doesn't know when either moved

- After any push or rebase, re-derive every run citation for the NEW head
  (`gh run list --branch <branch> --json headSha,databaseId,conclusion`, assert headSha
  == `git rev-parse HEAD`) and update the body before reporting (#596).
- `Closes #N` closes on squash regardless of partial-scope prose. Partial scope links as
  `Part of #N` / `Mitigates #N`.
- The scan is textual and context-blind: `close`/`fix`/`resolve` next to `#N` fires from
  blockquotes, caveats, and aggregated commit messages too. Before opening/updating a
  `Part of` PR, grep body AND `git log` for keyword-next-to-`#N` and reword; whoever
  merges scrubs the aggregated message and re-reads partly-addressed issues after
  (#569, #615).

## A model that re-implements the algorithm proves the algorithm, not the code

A property/mutation test over a model bounds the design; only a test executing the real
function bounds the code (#606). Before crediting a path's tests, grep for its
constructor and ask what constructs THAT — if nothing a headless test can build, move the
logic somewhere a test can reach (a newtype with unit tests), don't write a better
comment.

## `/tmp` is one namespace shared by the whole fleet — scratch files go in your own worktree

Every agent shares one `/tmp` and reaches for the same filenames; the second writer wins
silently (#625). Temp/scratch files live under the agent's own worktree (`./.scratch/`,
gitignored) — never a bare `/tmp` name.

## A green suite's coverage claim is a claim like any other — the mutation round corrects it

When anything (PR body OR code comment) claims a specific test or mechanism polices a
specific property, run the one mutation that removes it and watch WHICH tests redden —
the predicted-vs-actual failure diff is the value. A matching result is evidence; a
mismatch is a correction, and disclosing it beats a quiet re-run (#664, #673, #682).

## A test's specimen must stay a member of the class it witnesses

When a directive moves a real-world specimen out of the class a test needs it in
(a declared value converging with the default; a file gaining the block it was the
"absent" specimen for; a concrete list going stale), relocate the property onto a witness
that still distinguishes — a synthetic specimen or a generic rule — and never relax the
assertion until today's specimen passes. If the converged case still deserves coverage,
give it its own strictly-weaker, explicitly-labeled assertion (#689).
