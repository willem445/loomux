//! The host half of the server-declared-root registry — who may **mint** a
//! declared root (#1042 slice B).
//!
//! The mechanism itself is [`loomux_engine::rootreg`] (slice A): a
//! `RootRegistry` of canonicalized paths, and a `DeclaredRoot` that only
//! `RootRegistry::resolve` can mint. Slice A shipped it consuming nothing. This
//! module is the other end — the *population* side — and it exists as its own
//! file rather than a corner of `fileedit.rs` because the whole security
//! argument of #1042 is a sentence about this file's contents: **these are all
//! the ways a root gets declared.** A reader who wants to audit that should be
//! able to read one screen, not grep a twenty-thousand-line module.
//!
//! # The trust rule, and where the teeth are
//!
//! A root is usable iff some trusted source declared it, and the trusted
//! sources are exactly two:
//!
//! 1. **The local trusted webview**, through [`admit_root`] below. It may admit
//!    anything, because it already owns the disk — a rule stopping it from
//!    declaring a root would buy nothing and would cost a folder chip that goes
//!    dead after an agent `cd`s, and a session restore that will not restore.
//! 2. **The engine's own derivations** — a group's checkout as orchestration
//!    creates or resumes the group, and a worktree the engine itself cut. These
//!    are values the engine computed, never a caller's string.
//!
//! The teeth are in what is *absent*: there is no third source, and in
//! particular no wire one. [`admit_root`] is classified **`disabled`** (§5.2 of
//! `doc/design/remote-engine-protocol.md`) — off the wire roster and advertised
//! as absent, exactly like `open_in_editor` and `fm_open` — so when the
//! listener's default-deny dispatcher lands, a remote peer can **use** declared
//! roots and can never **mint** one. That is the whole shape of the answer:
//! desktop UX survives by construction, and the enforcement is entirely
//! wire-side.
//!
//! If an authenticated remote admit path is ever wanted, it re-enters the roster
//! as `wire`/**owner** behind auth — a capability *added* later, which is
//! §1.1 H3's rule, and the opposite of one removed later.
//!
//! # What this slice does **not** do
//!
//! Nothing resolves yet. No command refuses a root it did not refuse before,
//! and `root-not-declared` is still returned to nobody. Boundary `resolve` on
//! the `ft_*`/`fm_*`/`git_*`/`gh_*` families — with the choke functions changing
//! signature so the compiler holds the property — is slice C. The ordering is
//! deliberate: **admits land before any refusal exists**, so the desktop keeps
//! working after every merge.
//!
//! One consequence of that ordering is worth naming rather than discovering in
//! slice C. [`admit_derived`]'s caller in `create_group_ex` registers a group
//! checkout that, on the create path, came from a caller argument. Until slice C
//! makes `create_orchestration` (and the rest of the orchestration `repo`
//! boundaries) resolve their `repo` against the registry first, that is a
//! laundering path on paper — a wire `create_group("C:/Users/me/.ssh")` would
//! mint a declared root.
//! It is inert today because no wire exists and nothing enforces, and slice C
//! closes it by the rule the design states in one line: **any command that would
//! cause a root to become registered from a caller argument must itself take an
//! already-declared root.**
//!
//! See `doc/design/groupid-and-path-roots.md`.

use std::path::Path;
use std::sync::Arc;

use loomux_engine::rootreg::RootRegistry;

/// Declare a root, mapping the engine's typed refusal onto the `code: message`
/// string shape every file command already answers in.
///
/// Separate from the command below so the engine-derived call sites (which have
/// no Tauri `State` and no async context) share one error mapping with the
/// webview's, and `pub` so `tests/rootreg.rs` can exercise that mapping without
/// standing up a Tauri runtime.
pub fn admit(roots: &RootRegistry, path: &str) -> Result<(), String> {
    roots
        .admit(Path::new(path))
        .map_err(|e| crate::fileedit::err(e.code(), e.to_string()))
}

/// Declare a root the **engine itself** derived — a group's checkout, a worktree
/// it cut. Best-effort and silent by design.
///
/// Silent because every caller is a side effect of an operation whose success is
/// already decided: a group has been created, a worktree exists on disk. Failing
/// that operation because a path could not be canonicalized would be a new way
/// for group creation to fail, invented by a registration step, and the
/// registry's own contract already answers what happens when a root is missing —
/// the command that later names it is refused, which is the correct and already
/// designed outcome.
///
/// Not every caller passes a real path, either: the reserved solo pseudo-group
/// records `repo: "(standalone)"`, which is a label rather than a directory and
/// is refused here exactly as it should be.
pub(crate) fn admit_derived(roots: &RootRegistry, path: &str) {
    let _ = admit(roots, path);
}

/// Declare a filesystem root for this process.
///
/// **Class: `disabled`** (§5.2). This command is meaningful only where the
/// display and the engine share a machine and a trust domain, so it is absent
/// from the wire roster and advertised as absent. It is deliberately *not*
/// `client-local` — client-local means "runs in the desktop client's own Rust
/// and never crosses", and this one must reach the engine's registry. It
/// therefore has no tier: it is never on the wire to have one.
///
/// The desktop's folder picker is why this exists at all. `pickDirectory` is a
/// client-side dialog the backend never sees, so enforcing server-declared roots
/// without an admit path would break browsing outright. Here the picked path
/// arrives as a declaration from the trusted webview.
///
/// Idempotent, and answers the same is-a-directory question the callers'
/// existing probes do (`RootError::NotADirectory` → `not-found`), so a typo'd or
/// deleted folder keeps failing the way it fails today.
#[tauri::command]
pub async fn admit_root(
    roots: tauri::State<'_, Arc<RootRegistry>>,
    path: String,
) -> Result<(), String> {
    // **Reentrancy.** Two `is_dir` + `canonicalize` probes and a `BTreeSet`
    // insert under the registry's own `RwLock`. Insertion is idempotent on the
    // canonical key, so concurrent admits of one directory — the realistic race,
    // a restore admitting several recorded roots at once — converge on the same
    // single entry regardless of order, and admits of different directories do
    // not interact at all.
    let roots = Arc::clone(&roots);
    crate::blocking::run_blocking(move || admit(&roots, &path)).await
}
