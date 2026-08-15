# Repo-recorded lessons

Rules that must survive the group that learned them — rule + fix only, a bare
issue ref as provenance. Only ~4 KB reaches a kickoff: curate, don't append.
See `doc/design/lessons.md`.

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

A comment, design note, audit label or PR body claiming something the code doesn't do is
a defect (#461, #490) — delete it, don't soften it. Quote raw fetched text for CLI/API
facts (#453). Prose written against a plan stays dated to it: before marking ready,
re-read every doc and `SKILL.md` claim against what the sibling slice actually SHIPPED,
not the plan both were written from (#715, #721).

## Commit real work BEFORE capturing red evidence [pinned]

`git checkout -- <file>` discards ALL uncommitted work in it, not just your evidence
patch (#493). Commit the real work first (amend/squash later), then patch for the red
run, then restore — never point a destructive git command at a file holding anything
uncommitted.

## Any suppression driven by a fallible signal must be BOUNDED

A guard holding an action "while X is true" needs an answer for "X never clears" (#496,
#513, #518). Fix the signal AND bound the consumer; release on independent evidence, not
elapsed time.

## `rustfmt --check` is the one local Rust check agents may run

From the repo root: `rustfmt --check --edition 2021 <changed .rs> >/dev/null`.
`--edition` is mandatory (2015 false-errors `async fn`); discard stdout (~15k lines of
unenforced diff); the exit code is ambiguous — **stderr is the signal**. Never run bare
`rustfmt` or cite a clean run as validation. `cargo check` stays banned (#488, #558).

## Never block a turn waiting on CI

A `[loomux]` notice arrives by typing into a pane, and a mid-turn pane can't take
delivery — waiting on CI queues the answer behind the turn itself (#590). Register the
watch, END the turn, act on the notice. Same for any pane-delivered answer: reading once
is fine, waiting is the defect.

## What a green run actually evidences

- **A coverage claim is a claim.** When one says a test polices a property, run the
  mutation that removes it and watch WHICH tests redden; disclose a mismatch (#664, #673,
  #682). A red evidences only the assertion it REACHED and MOVED — a panic before it, or
  a companion that also passed broken, prove nothing (#710, #712, #727). A mutation a
  *reviewer* named is still unrun (#868).
- **A specimen must stay in the class it witnesses.** When a value converges with the
  default or a list goes stale, relocate the property onto a witness that still
  distinguishes — never relax the assertion (#689). Same drift outside tests: a
  hand-derived value a claim rests on (a line cite, a count) is valid only at the commit
  it was derived on — your own next commit invalidates it as silently as a rebase. Cite a
  SYMBOL (#763); a position that must be recorded is swept in the LAST commit touching
  its source (#752).

## A multi-line shell script is a file, not a `-c` argument

Inline Bash dies on Git Bash quoting (`unexpected EOF ... matching '`, reported far
from the quote). Write it to `./.scratch/` and run the file; pipe prose via
`--body-file -`.
