//! The loomux orchestration engine — the part of loomux that has nothing to do
//! with being a desktop app.
//!
//! **This crate is an empty scaffold right now** (#888 slice A1 / #847 Phase
//! 0). It exists before it has contents on purpose: converting the repo to a
//! Cargo workspace moves the lockfile, the `target/` directory and the release
//! profile, and that is a release-plumbing change worth landing and proving on
//! CI by itself, separately from thousands of moved lines.
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
