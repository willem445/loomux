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

A comment, design note, audit label, or PR body stating something the code
doesn't do is a defect, not a slip — it stops the next reader from checking.
This pattern has recurred repeatedly on this repo: #461 catalogues seven
instances from one session (e.g. an audit action labeling a failed delivery
a success), and the batch that filed this entry produced another — PR #489
offering a persona `allow:` opt-in as a regression's fix when it's actually
blocked at *two* structural layers, the workflow parser and `persona_inject`'s
#222 capability closure (#490) — an impossible mitigation reads as an answer
and stops the search. This entry itself first shipped with a miscount and a
one-layer version of that same claim, caught in review, not before. Delete a
claim you can't point at code for, don't soften it; quote raw fetched text
for CLI/API facts, never a paraphrase (#453).

## Commit real work BEFORE capturing red evidence

Red-before-green on this repo usually means running the NEW tests against
the OLD behavior — which means temporarily patching a file back and then
restoring it. `git checkout -- <file>` restores by discarding ALL
uncommitted work in that file, not just the experiment: it silently ate a
real, minutes-old correctness fix during #493's evidence capture (re-done
later as b4c7d96; same destructive-git class as the `git stash` entry
above). The cheap rule: commit the real work first — a small commit you
amend or squash later — then patch for the red run, then restore. Never
point a destructive git command at a file holding anything you haven't
committed.
