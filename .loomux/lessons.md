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
