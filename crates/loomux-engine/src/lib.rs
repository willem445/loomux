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

pub mod groupid;
pub mod lessons;
pub mod notify;
pub mod report;
pub mod termgrid;
pub mod text;
