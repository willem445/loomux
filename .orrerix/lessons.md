# Repo-recorded lessons

Rules that must survive the group that learned them — rule + fix only, a bare issue ref
as provenance. Newest at the bottom. Only ~4 KB reaches a kickoff, and eviction is
silent to whoever wrote the entry: append, then check the file still fits, and say in
the PR what you displaced. A permanent rule belongs on an uncapped surface (`CLAUDE.md`,
a skill, the role templates) — this file is for what must reach every kickoff. See
`doc/design/lessons.md`.

## No getrandom-based crates in src-tauri [pinned]

No `uuid` v4, `rand`, `tempfile`-with-defaults, or anything pulling `getrandom` into
`src-tauri` — the binary fails to load (`0xc0000139`) on the Windows 10 baseline. Use
std's OS-seeded `RandomState` for ids/tokens. Before adding a dependency: check
`src-tauri/Cargo.toml`'s notes and run `cargo tree -e normal --target all -i
getrandom@<version>`.

## Never resize the PTY for a UI feature [pinned]

UI features are overlays or header/board chrome floating over the terminal — never a
resize. Visual padding goes on the `.xterm` element, not the layout.

## Never `git stash` in an orrerix worktree [pinned]

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
elapsed time. Extending to "every consumer" means grepping the subsystem and publishing
the list.

## `rustfmt --check` is the one local Rust check agents may run

From the repo root: `rustfmt --check --edition 2021 <changed .rs> >/dev/null`.
`--edition` is mandatory (2015 false-errors `async fn`); discard stdout — those are
*unenforced* formatting diffs (~15k lines on the big module), noise here and never a
finding; the exit code is ambiguous, so **stderr is the signal**. Never run bare
`rustfmt`, commit a reformat, or cite a clean run as validation. `cargo check` stays
banned (#488, #558).

## Never block a turn waiting on CI

An `[orrerix]` notice arrives by typing into a pane, and a mid-turn pane can't take
delivery — waiting on CI queues the answer behind the turn (#590). Register the watch,
END the turn, act on the notice. Same for any pane-delivered answer: reading once is
fine, waiting is the defect.
