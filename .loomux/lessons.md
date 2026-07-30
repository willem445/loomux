# Repo-recorded lessons

Hard-won knowledge about this repo that should survive past the orchestration group that
learned it. See `doc/design/lessons.md` for what this file is, how it's injected, and its
trust guardrails. Newest entries go at the bottom (an append log); nothing here parses these
headings — they're for readability only.

## No getrandom-based crates in src-tauri

`uuid` v4, `rand`, `tempfile` with default features, or anything else that pulls in the
`getrandom` crate must stay out of `src-tauri`'s dependency tree. They import
`bcryptprimitives.dll!ProcessPrng`, which this project's Windows 10 baseline doesn't export —
the binary then fails to load with `0xc0000139`. Agent ids/tokens use std's OS-seeded
`RandomState` instead. Before adding any new dependency, check the notes in
`src-tauri/Cargo.toml` and audit its transitive deps for a `getrandom` edge
(`cargo tree -e normal --target all -i getrandom@<version>`).

## Never resize the PTY for a UI feature

Git view, task board, audit viewer, badges, compose strip — all of these are overlays or
header/board chrome floating over the terminal, never a resize of it. Resizing ConPTY
triggers full repaints that pollute scrollback. Visual padding belongs on the `.xterm`
element, not on the layout.

## Never `git stash` in a loomux worktree

The stash stack lives in the shared `.git` and is one stack across *every* worktree of a
repo, not per-worktree. Two workers running concurrently in separate worktrees collided on
`git stash`: one popped/dropped entries assuming the stack was its own and nearly destroyed
the other's WIP before noticing mid-operation and recovering it (#299, a live near-miss —
pure luck it was caught). Commit WIP to your own branch instead (a small commit you
amend/reset/squash later). If you must stash, `git stash push -m "<your agent id>: ..."` and
only ever `pop` an entry carrying your own marker.

## A claim is a deliverable

A code comment, design note, audit label, or PR body that states something the
code doesn't do is a defect, not a harmless slip — a confident wrong claim
stops the next reader from checking. One session produced nine instances of
the same shape: a comment citing a mutation test that didn't exist; a design
note naming an API field from a WebFetch *summary* instead of the raw doc, and
getting it wrong; an audit action labeling a failed delivery as
`delivery-confirmed-late`; a doc claiming a check ran before a step it
actually ran after; `doc/design/orchestration.md` describing reviewer
containment that is instruction-backed only, not CLI-enforced; a README
describing removed-gate behavior a later fix had already reversed; PR #489
offering a persona `allow:` opt-in as its regression's mitigation when the
workflow parser structurally refuses `allow:` on a read-only block — a
documented mitigation that cannot work is worse than none, because it reads as
an answer and stops the search; and PR #486 saying `Closes #464` while its own
body claimed a follow-up issue existed before one had been filed (caught in
review, not before it shipped).

Before you push: every claim you can't point at code for gets **deleted, not
softened**. When a change falsifies something documented, sweep every surface
that states it — README, `doc/design/`, comments — not just the one you were
told about. A sweep report ("nothing further is stale") is itself a claim;
verify it by reading the rendered result, not by grepping for the string you
replaced. For CLI/API facts, quote the raw fetched text verbatim, never a
summarized paraphrase (#453).
