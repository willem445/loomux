//! The loomux orchestration engine — the part of loomux that has nothing to do
//! with being a desktop app.
//!
//! The crate landed empty (#888 slice A1 / #847 Phase 0) on purpose:
//! converting the repo to a Cargo workspace moves the lockfile, the `target/`
//! directory and the release profile, and that is a release-plumbing change
//! worth landing and proving on CI by itself, separately from thousands of
//! moved lines. Slice A2 fills it, in small batches.
//!
//! # Why it exists
//!
//! `src-tauri` links Tauri, which on Linux means webkit2gtk. A headless server
//! that must build a browser engine in order to run orchestration is not a
//! deployment shape, so the remote-engine work (#888) needs a core that builds
//! without it. That core is this crate.
//!
//! # The one rule
//!
//! **`src-tauri` depends on `loomux-engine`. The arrow never points back.**
//! No `tauri` in this crate's dependency tree, ever — not directly, not
//! transitively. Everything the engine needs from the host arrives as a trait
//! the host implements (`EventSink`, `PaneHost`; slice A3), never as an
//! `AppHandle`.
//!
//! # What lands here, in order
//!
//! Slice A2 moves the already-Tauri-free submodules; A3 introduces the host
//! traits; A4 moves `OrchRegistry` and the decision layer. Each is its own
//! reviewable change, and the bar for all of them is behavioural silence — the
//! existing suite green with no test edits. See
//! `doc/design/engine-extraction.md`.
//!
//! # What is here so far
//!
//! A2 batch 1 — [`report`] (the decision-grade report protocol's pure core,
//! #398) and [`termgrid`] (the dependency-free VT replay behind `get_output`,
//! #520). They went first because they are the two submodules with no outbound
//! dependency on anything else in `orchestration/` at all: both are `std`-only,
//! so they prove the move-and-re-export mechanism end to end without also
//! testing a dependency edge. `src-tauri/src/orchestration/mod.rs` re-exports
//! each under its old path, so every existing call site resolves unchanged.
//!
//! Their test coverage arrived in two different shapes, and the difference is
//! worth knowing before the next module moves. [`report`] brought nine inline
//! `#[cfg(test)]` unit tests with it. [`termgrid`] has **none** — it never had
//! any; it is covered entirely from `src-tauri/tests/orchestration.rs`, which
//! drives `render_screen`/`render_visible` from ~30 call sites and stayed
//! untouched by the move.
//!
//! Inline unit tests are *possible* on this side of the boundary, which is the
//! part that matters for what comes next: CLAUDE.md constraint 4 forces
//! `src-tauri`'s lib-linking tests to be integration tests because Windows test
//! executables need the comctl32-v6 manifest `build.rs` embeds, and nothing
//! here links Tauri or that manifest. So a module arriving with inline tests
//! keeps them, and a module whose coverage lives in the integration suite keeps
//! that instead — neither is converted on the way in.
//! `src-tauri/tests/smoke.rs` still has to exist for the app's own sake.
//!
//! A2 batch 2 — [`groupid`] (#904), the validated group identifier every
//! group-scoped path is built from. It brings the crate's first dependency,
//! `serde`: `GroupId`'s wire transparency is a hand-written
//! `Serialize`/`Deserialize` pair, and the `Deserialize` half is load-bearing
//! rather than convenience — it is what stops a hand-edited state file minting
//! an id the constructor would have refused. See the manifest for why that
//! dependency is safe under CLAUDE.md constraint 2.
//!
//! It also moved a **security tripwire's coverage**, which is the part worth
//! knowing before the next batch. `GroupId` deliberately has no
//! `AsRef<Path>`, and `src-tauri/tests/groupid.rs`'s
//! `the_orchestration_root_is_joined_with_a_group_in_exactly_one_place`
//! asserts that absence by scanning source. Once the type lives here, the
//! orphan rule leaves nowhere else that impl could be written — so the scan now
//! walks BOTH source roots, and a scan that only knew `src-tauri/src` would
//! have gone green forever while enforcing nothing. **Any future batch that
//! moves a type a source-scanning test watches owes the same check**: ask where
//! the violation can now be spelled, not where it used to be.
//!
//! A2 batch 3 — [`lessons`] (#268) and [`notify`] (#243), plus [`text`], which
//! is what made them movable. Both modules were otherwise leaves; each had a
//! single edge left pointing at `orchestration/mod.rs`, and both of those edges
//! were pure string helpers rather than registry state — so the helpers moved
//! ahead of their callers into [`text`] instead of a trait being invented to
//! reach back for them. Read that as the batch's actual finding: **an edge into
//! `mod.rs` is not automatically an edge into the registry.** Some of what
//! `mod.rs` holds is there because it is one large file, not because it is
//! coupled to `AppHandle`, and that kind of edge is cut by moving the callee,
//! not by abstracting the caller.
//!
//! This batch also widened two of [`notify`]'s functions from `pub(super)` to
//! `pub`. That is the crate boundary's doing rather than a policy change: their
//! callers (the `gh pr list` rollup, the merge queue's batch verdict) stayed in
//! `src-tauri`, and no visibility narrower than `pub` still reaches them. Worth
//! expecting again — a batch that leaves a caller behind converts that caller's
//! `pub(super)` into public API, so the question each time is whether the item
//! is one this crate is content to expose, not merely whether it compiles.
//!
//! It also brings `serde_json` ([`notify`] parses `gh --json` payloads) and
//! asks `serde` for `derive`. Both are argued in the manifest; the `derive`
//! half in particular was declared unnecessary there until this batch, and
//! is not any more.
//!
//! A2 batch 4 — [`model`], the shared data layer: [`model::Role`] (the closed
//! capability class), [`model::Containment`] (the deny tier it selects), the
//! per-CLI capability table and the pure functions over them. It is the first
//! batch that moved something *because of what comes next* rather than because
//! it had run out of edges: the `workflow` cluster is the batch after it, and
//! every one of these symbols is something `workflow` reads. Moving `workflow`
//! first would have left it reaching back into `src-tauri` for its own `kind:`
//! type — the arrow pointing the wrong way, in the module that most needed it
//! not to.
//!
//! Its finding is the mirror image of batch 2's, and worth having both. There,
//! a source-scanning tripwire had to FOLLOW the type, because the orphan rule
//! moved the only place its violation could be written. Here, `Role`'s
//! `template()`/`instructions_file()` had to STAY BEHIND: an inherent impl must
//! live in the crate defining its type, so keeping them as methods would have
//! dragged `src-tauri/src/orchestration/templates/*.md` and the byte-golden
//! fixture root that pins them into this crate — a *silent* relocation of
//! product content and its blessing procedure, done as a side effect of moving
//! an enum. They are free functions in `src-tauri` now, and the call-site
//! rewrite that cost is one the compiler checks exhaustively. **Ask of every
//! item on a moving type whether it is data or content**; the compiler will
//! tell you about the rewrite, and nothing will tell you about the relocation.
//!
//! Batch 5 then split that pair, and the amendment belongs here rather than
//! only in [`model`]: `role_template` stays (it loads the fixture-pinned bytes,
//! which is what the paragraph above is actually about) and
//! [`model::role_instructions_file`] came across (it loads nothing, and
//! `workflow::Block` calls it from inside this crate). Read the rule as **ask
//! it of each item, not of the pair** — batch 4 kept them together on the
//! ground that "the name and the bytes are one mapping", and that pairing was
//! the weaker half of its own argument.
//!
//! A2 batch 5 — the `workflow` CLUSTER: [`workflow`] (the
//! `.loomux/workflow.yml` parser, its types, the merge-gate spec file and the
//! capacity advice), [`profiles`] (the persona/profile loader and its
//! sanitizers) and [`locks`] (the named-resource state machine). The first
//! batch that had to move THREE modules at once, and the reason is a shape
//! worth recognising rather than a size: `profiles` calls
//! `workflow::{kind_from_str, resolve_profile_path}` while `parse_workflow`
//! calls `profiles::sanitize_allow`. That cycle is unremarkable inside one
//! crate and unrepresentable across two, so **a dependency cycle is a
//! partition, not an ordering** — no batch order exists that moves either
//! alone, and the only question was where to draw the line around it. `locks`
//! joins because `LockTable::sync` is typed on `workflow::ResourcePolicy`.
//!
//! The line was drawn TIGHT. `mergeq` looked like a fourth member and is not:
//! every mention of it in `workflow` is prose — a doc link and two references
//! in doc comments — and prose does not make an edge. What matters is that no
//! `mergeq` path appears in a body, which is the thing to check; counting the
//! mentions is neither necessary nor, as it turned out, easy to get right.
//! `mqdriver` is `workflow`'s heaviest consumer and stays behind on purpose —
//! it reaches the pane host (`capture_raw_with_timeout`), which is slice A3.
//! **An inbound edge never blocks a move**, because the re-export answers it:
//! `mqdriver` still spells `super::workflow::…` and never learned anything
//! changed. Only outbound edges decide what a batch has to contain.
//!
//! One outbound edge did not exist when this batch was planned, because batch 4
//! created it. `Block::instructions_file` calls `role_instructions_file`, which
//! batch 4 had deliberately left in `src-tauri` paired with `role_template`. So
//! batch 5 split that pair — see [`model`]'s header for the argument. The
//! finding generalises past this batch: **the batch that lifts a data layer
//! ahead of its caller can leave a new edge pointing the wrong way**, because
//! splitting a type's methods off it decides where those methods live, and the
//! caller that forces the question may not have moved yet. Re-derive the edge
//! set from the source at the start of every batch; a map drawn one batch ago
//! is describing a tree that has since changed.
//!
//! It brings `serde_norway` (the YAML parser `parse_workflow` is built on) and
//! `sha2` (`body_digest`, the hash the merge gate compares). Batch 3's rule
//! held: **a dependency a module uses has to be declared, not inherited** —
//! both are already in the shipped binary's graph via `src-tauri`, so no new
//! package joins the lock, but resolver-2's feature unification is not
//! crate-name unification and an undeclared crate does not compile at all. The
//! manifest carries the argument for each, and `src-tauri`'s carries the
//! getrandom audit both inherit.

pub mod groupid;
pub mod lessons;
pub mod locks;
pub mod model;
pub mod notify;
pub mod profiles;
pub mod report;
pub mod termgrid;
pub mod text;
pub mod workflow;
