//! `mqdriver` moved to `loomux_engine::mqdriver` (#888 slice A3 batch 12a).
//! This file is what is left behind: a **curated re-export module** that
//! reproduces the moved module's visibility table exactly, so every call site
//! that spells `mqdriver::…` — here, in `mqloop.rs`, and in
//! `src-tauri/tests/mergequeue.rs` / `tests/orchestration.rs` — resolves
//! unchanged. Read the code in `crates/loomux-engine/src/mqdriver.rs`; the
//! design note is `doc/design/merge-queue.md` and the extraction's is
//! `doc/design/engine-extraction.md`.
//!
//! # Why a module rather than a `pub use` line in `mod.rs`
//!
//! Every consumer spells the **module path**, so batches 10 and 11's rule ("pick
//! the re-export shape from what the callers spell") points at the plain
//! `pub use loomux_engine::mqdriver;`. That line was not taken, and the reason is
//! the other half of the same rule — "take the item list when it buys a
//! narrowing that is real" (#988).
//!
//! Unlike `queue`/`queuestate`/`intake`, this module **does** contain
//! `pub(super)` items: `as_args`, `landable` and `declares_ci_green`, whose only
//! caller is `mqloop` (still in this crate; batch 12b). They have to be `pub` in
//! the engine to cross the boundary at all, and no re-export can narrow the item
//! itself — `loomux_engine::mqdriver::landable` **is** that crate's public API
//! now, forced, on the same terms batch 9 states for `fsatomic::atomic_write`
//! and `model.rs:61-73` states generally. What a re-export *can* fix is the reach
//! of the `orchestration::mqdriver::…` spelling, and that is what the
//! `pub(super) use` at the bottom of this file does: within `orchestration` and
//! its descendants, exactly as before.
//!
//! `landable` is why the lines are worth paying. It is **half** of the
//! constraint-7 refusal — the refspec-shape predicate — and `validate_target` is
//! the whole of it. Leaving a half-check publicly reachable under the path
//! everything in this crate already spells is an invitation to build the next
//! branch-name guard on the wrong one. `as_args` and `declares_ci_green` ride
//! along because the honest form of this file is "the visibility table, copied":
//! every item that was `pub` is `pub`, every item that was `pub(super)` is
//! `pub(super)`, and nothing that was private appears at all.
//!
//! Two consequences, stated rather than left to be re-derived. **This list is
//! not automatic** — an item added to the engine module is not reachable here
//! until somebody adds it, which is batch 7's stance for `obs.rs`: what
//! `src-tauri` re-exports should be a list somebody chose, not whatever the
//! engine module makes public next. And **it collapses in batch 12b**: once
//! `mqloop` is in the engine and spells `crate::mqdriver::…`, the three narrow
//! lines have no consumer left and this whole file should become the one
//! `pub use loomux_engine::mqdriver;` that batches 10 and 11 would have written.
//!
//! The private members stay private in the engine and appear nowhere here:
//! `is_not_found`, `RawPrFacts`, `usable_for_comparison`, `same_branch`,
//! `is_object_name`, `RawCheck`, `land_refspec`.

// Everything that was `pub` in this file before the move, in the order the
// engine module defines it.
pub use loomux_engine::mqdriver::{
    audit_action, classify_checks, cleanup_scratch, close_draft_argv, default_branch_argv,
    delete_scratch_argv, land_batch, land_push_argv, ls_remote_argv, mint_scratch, pr_checks_argv,
    pr_ci_green, pr_ci_green_detailed, pr_facts_argv, push_scratch, resolve_and_validate_target,
    resolve_and_validate_target_detailed, resolve_default_branch, resolve_default_branch_detailed,
    resolve_pr, resolve_pr_detailed, runner_for, scratch_exists, scratch_push_argv,
    validate_target, BatchVerification, CleanupFailure, CmdOut, LandRefusal, Landed, MintError,
    Minted, MqRunner, PrFacts, ProcessRunner, ResolveFailure, TargetRefusal, MINT_ATTEMPTS,
    MQ_CMD_TIMEOUT, REMOTE,
};

// The three the crate boundary forced wide. `pub(super)` here is
// `orchestration`-and-below — the reach they had as `pub(super) fn` in this file
// before the move, and the reach `mqloop`'s `super::mqdriver::…` needs.
pub(super) use loomux_engine::mqdriver::{as_args, declares_ci_green, landable};
