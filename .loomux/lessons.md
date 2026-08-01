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

## Any suppression driven by a fallible signal must be BOUNDED

A guard that holds an action back "while X is true" needs an answer to *what
if X is wrong and never clears*. On this repo the answer keeps being "a human
notices and does it by hand": #496, #513 (27 min — per-attempt bound,
unbounded retry loop), #518 (a false "user typing" pinning a delivery's
human-input block for the late monitor's full 4h life). Bounds landed one at
a time as each fired — #500 for the idle tick, #518 for the delivery hold.

Three things learned the hard way:

- **Fixing the signal does not excuse bounding the consumer.** #499 gated the
  stamp at its source and #518 happened anyway: that gate is a byte-shape
  match over an OPEN set of terminal auto-reply shapes, and the next shape is
  always unmodelled. Fix the signal *and* bound what reads it.
- **Releasing on evidence beats releasing on elapsed time.** A pure timeout
  must be tuned against "what if the human really is mid-sentence" — which is
  why #500's clamp needed a per-group knob. #518's releases only when a
  second, independent reading says there is nothing to clobber
  (`input_pending` false), so it needs no knob and cannot erode the rule it
  sits under.
- **Enumerate in writing.** "Extend this to every consumer" means grep the
  subsystem and publish the list, the already-fine ones included. #518's
  enumeration refuted the issue's own premise about where the gap was.

## `rustfmt --check` is the one local Rust check agents may run

Local cargo is banned outright for agents (#320 CPU, #488 disk) — but that left no way to
know whether newly written Rust even *parses*, so the cheapest defect cost a full CI round: a
scripted rewrite cut at a `);` inside a string literal, and all three build jobs failed on
parsing, not assertions (#558). `rustfmt` is a parser, not a build — no cargo, no dependency
resolution, no `target/` bytes, ~3-5s for this whole crate — so it sits inside both bans. From
`src-tauri/`: `rustfmt --check --edition 2021 <changed .rs files> >/dev/null`. The flag is
mandatory (rustfmt's CLI defaults to edition 2015, where `async fn` is a false parse error);
stdout is discarded because `--check` also prints *formatting* diffs and this repo is
deliberately not rustfmt-formatted — 12,483 lines of them for `orchestration/mod.rs` alone, so
dropping the redirect floods your own context; the exit code is ambiguous (1 means either), so
**any output on stderr is the signal — read it, don't grep it for `error:`** (an unresolved
`mod` arrives as `Error writing files: …`, with no `error:` token). It is a syntax check and
nothing more — never run bare `rustfmt`, never
commit a reformatting, and never cite a clean run as validation: it catches no type error, no
unresolved name, not even a `format!` with the wrong argument count. `cargo check` stays
banned and this argument does not reach it — it writes dependency metadata to `target/`, which
is exactly the cost #488 banned.
