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
//! Each module keeps its own `#[cfg(test)]` unit tests, which is only possible
//! on this side of the boundary: CLAUDE.md constraint 4 forces `src-tauri`'s
//! lib-linking tests to be integration tests because Windows test executables
//! need the comctl32-v6 manifest `build.rs` embeds, and nothing here links
//! Tauri or that manifest. `src-tauri/tests/smoke.rs` still has to exist for
//! the app's own sake.

pub mod report;
pub mod termgrid;
