//! The delivery subsystem's two mutable maps, behind doors that cannot be
//! opened without paying what opening them costs (#562, #497).
//!
//! **Why this module exists at all, given the maps used to be two plain
//! fields on `OrchRegistry`.** Rust's privacy is per-MODULE, and
//! `orchestration/mod.rs` is one 31k-line module: a field declared private
//! there is still reachable from every line of it, so "the only way to
//! mutate `queues` is through the sanctioned path" was not a claim the
//! compiler could check — it was a claim `doc/design/orchestration.md`'s
//! #470/#523 mutation-site table made on the compiler's behalf, and that
//! table was wrong in both possible directions on the same day (stale for
//! #533's two new mutators, and mis-transcribed for a row its own grep DID
//! return). Moving the two maps into a module of their own is what turns
//! the rule into something `rustc` rejects rather than something a reader
//! has to notice. The *file boundary* is the mechanism; everything below is
//! just the API it makes unavoidable.
//!
//! Two invariants live here, one per map:
//!
//! - **`QueueMap` (#562) — "every mutation of `queues` rewrites the
//!   snapshot" (#468).** `&mut` access is available only inside
//!   [`QueueMap::mutate`], which takes the snapshot writer as an argument
//!   and runs it, with the lock released, as soon as the mutation returns.
//!   A caller cannot mutate and forget to persist, because there is no
//!   expressible order of operations in which the persist does not follow.
//!   The mutation states which of the two it did — [`QueueDirty::snapshot`]
//!   or [`QueueDirty::nothing_persisted`] — and that is a *typed value it
//!   must produce*, not a call it can omit.
//!
//! - **`DrainerRegistry` (#497) — "every removal from `queue_draining` is
//!   generation-checked" (#470 B1 round 2).** The map exposes exactly one
//!   removal, [`DrainerRegistry::release`], and it takes the generation.
//!   An ungenerationed removal is not a thing a future call site can write
//!   incorrectly; it is a thing it cannot write.
//!
//! **What this does NOT claim.** A type can only reject the constructions
//! it can name. `QueueDirty::nothing_persisted` is a claim the mutation
//! makes about itself ("nothing the snapshot carries changed"), and a wrong
//! one is still wrong — what changed is that it is now an explicit,
//! greppable, reviewable assertion at the mutation site instead of the
//! silent absence of a call. That is the whole delta, and it is the one
//! that mattered for #533's two sites: both would have had to state a false
//! claim in the diff rather than say nothing at all.

use std::collections::{HashMap, VecDeque};
use std::ops::Deref;
use std::sync::{Mutex, MutexGuard};

use crate::obs::LockExt;

use crate::queue::QueuedDelivery;

/// Per-pane FIFO delivery queues, keyed by pty id — what [`QueueMap`]
/// guards. Named because both the read guard and the mutation closure hand
/// it out.
pub type QueueEntries = HashMap<u32, VecDeque<QueuedDelivery>>;

/// What one mutation of [`QueueMap`] did to the *persisted* set (#562).
///
/// Returned by every mutation and consumed by [`QueueMap::mutate`], which
/// is what turns it into a `queue.json` write. `#[must_use]` is the
/// backstop for the one shape the closure's return type does not already
/// force — a `QueueDirty::snapshot();` evaluated and dropped as a
/// statement.
#[must_use = "a `queues` mutation's persistence obligation is discharged by RETURNING this \
              from the `QueueMap::mutate` closure, which is what writes the snapshot (#562/#468)"]
pub struct QueueDirty(bool);

impl QueueDirty {
    /// This mutation changed what `queue.json` must carry — write it.
    ///
    /// The default answer, and the right one whenever there is any doubt:
    /// a redundant snapshot write costs one `atomic_write` of a file that
    /// is already being written on every admission, while a missing one
    /// costs a delivery replayed or resurrected across a restart.
    pub fn snapshot() -> Self {
        Self(true)
    }

    /// This mutation left the persisted set byte-identical, so no write is
    /// owed.
    ///
    /// **A claim, and the only one in this design that a type cannot
    /// check** — see the module doc. Legitimate uses are narrow and each
    /// one states its reason at the call site: a mutation that did not fire
    /// (a conditional pop whose id did not match the front), a removal that
    /// took nothing out (`commit_exit`'s normal empty-queue exit), or a
    /// touch that the snapshot writer provably does not read
    /// (`entry(pty).or_default()` interning an EMPTY deque —
    /// `group_queue_entries` iterates deliveries, so an empty deque
    /// contributes no entry).
    pub fn nothing_persisted() -> Self {
        Self(false)
    }

    fn write_needed(&self) -> bool {
        self.0
    }
}

/// What [`QueueMap::mutate`] runs once it has released the lock —
/// implemented by `OrchRegistry` over `persist_queues`.
///
/// A trait rather than a closure argument so the obligation has a NAME at
/// every call site (`self`, passed to `mutate`) instead of being an
/// anonymous thunk a future caller could pass `|| {}` for without it
/// reading as anything unusual.
pub trait QueueSnapshotWriter {
    /// Write `group`'s live queues to disk. Called with no queue lock held.
    fn write_queue_snapshot(&self, group: &crate::groupid::GroupId);
}

/// Read-only access to [`QueueMap`]'s contents.
///
/// A wrapper rather than the bare `MutexGuard` precisely because a
/// `MutexGuard` also derefs MUTABLY: handing one out would reopen the door
/// this module exists to close, from a method named `read`.
pub struct QueueRead<'a>(MutexGuard<'a, QueueEntries>);

impl Deref for QueueRead<'_> {
    type Target = QueueEntries;
    fn deref(&self) -> &QueueEntries {
        &self.0
    }
}

/// The live delivery queues (#445/#468) — the `queues` field of
/// `OrchRegistry`, with its persistence obligation made structural (#562).
pub struct QueueMap {
    inner: Mutex<QueueEntries>,
}

impl QueueMap {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    /// Read the queues. No persistence obligation: nothing changed.
    pub fn read(&self) -> QueueRead<'_> {
        QueueRead(self.inner.lock_safe())
    }

    /// The ONLY `&mut` door to the queues (#562).
    ///
    /// Runs `f` under the lock, RELEASES the lock, and then — if `f` said
    /// the persisted set changed — writes the snapshot through `writer`.
    /// "Mutated but did not persist" is not a bug this can have; it is a
    /// sequence a caller cannot express.
    ///
    /// **The lock is released before the write, and that is load-bearing,
    /// not tidiness.** `persist_queues` does file I/O and takes its own
    /// writer lock (order: `queue_persist` before `queues`, never the
    /// reverse — see its doc), and `enqueue_text`'s whole ordering argument
    /// depends on the `queues` critical section staying short. Doing the
    /// write inside the closure would invert that lock order AND stall
    /// every delivery in the registry behind a disk write.
    ///
    /// `f` returns its own result plus the [`QueueDirty`] verdict, so a
    /// mutation that genuinely changed nothing (see that type's doc) skips
    /// the write without needing an escape hatch to skip it WITH — there is
    /// no `discard`/`forget` method here, deliberately: #562's own caveat is
    /// that a hatch, once present, grows.
    pub fn mutate<R>(
        &self,
        group: &crate::groupid::GroupId,
        writer: &impl QueueSnapshotWriter,
        f: impl FnOnce(&mut QueueEntries) -> (R, QueueDirty),
    ) -> R {
        let (out, dirty) = {
            let mut entries = self.inner.lock_safe();
            f(&mut entries)
        };
        if dirty.write_needed() {
            writer.write_queue_snapshot(group);
        }
        out
    }
}

impl Default for QueueMap {
    fn default() -> Self {
        Self::new()
    }
}

/// Panes with a live drainer thread right now (#445), generation-owned
/// (#470 B1 review round 2) — the `queue_draining` field of
/// `OrchRegistry`, with its generation check made structural (#497).
///
/// The whole point is the *absent* method: there is no unconditional
/// `remove`. [`release`](Self::release) is the only way an entry leaves,
/// and it cannot be called without naming the generation whose removal it
/// is. A stale guard's removal is a no-op by construction — and so is a
/// removal written by code that does not exist yet, which is exactly the
/// case #497 identified as the one a property test over a fixed event set
/// can never cover.
pub struct DrainerRegistry {
    inner: Mutex<HashMap<u32, u64>>,
}

impl DrainerRegistry {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    /// Whether a drainer is registered for `pty_id` right now.
    pub fn is_registered(&self, pty_id: u32) -> bool {
        self.inner.lock_safe().contains_key(&pty_id)
    }

    /// Claim `pty_id` for a new drainer if nothing holds it, minting the
    /// generation UNDER the lock; `None` means someone else already holds
    /// it and the caller must not spawn.
    ///
    /// `mint` runs under the lock so the generation a claim installs is the
    /// one the claim decided on — the check and the insert are one critical
    /// section, which is what makes `ensure_drainer` idempotent under
    /// concurrent callers rather than merely usually-idempotent.
    pub fn claim(&self, pty_id: u32, mint: impl FnOnce() -> u64) -> Option<u64> {
        let mut inner = self.inner.lock_safe();
        if inner.contains_key(&pty_id) {
            return None;
        }
        let generation = mint();
        inner.insert(pty_id, generation);
        Some(generation)
    }

    /// Deregister `pty_id` IF `generation` is still the registered one —
    /// the ONE removal that exists (#497). Returns whether it fired.
    ///
    /// Both real callers (`OrchRegistry::commit_exit` and
    /// `DrainerGuard::drop`) race each other by design, and both are
    /// no-ops when the other won: see `DrainerGuard`'s doc in `mod.rs` for
    /// why an unconditional second removal could erase a SUCCESSOR
    /// drainer's live registration and run two drainers over one queue.
    /// That argument used to hold because every current call site was
    /// checked by review; it now holds because there is nothing else to
    /// call.
    pub fn release(&self, pty_id: u32, generation: u64) -> bool {
        let mut inner = self.inner.lock_safe();
        if inner.get(&pty_id) == Some(&generation) {
            inner.remove(&pty_id);
            return true;
        }
        false
    }
}

impl Default for DrainerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// **Why these exist, and why they are here rather than in the model.**
///
/// The experiment #497 asked for (PR #606, scratch commit `0ed4dfe`, CI run
/// 30690784043) put a raw ungenerationed `queue_draining` removal into REAL
/// code at two sites and the whole suite stayed green — including at the
/// *existing* guard site, which the issue's triage had recorded as covered
/// by `queue.rs`'s `unconditional_guard_removal_reproduces_the_round_2_
/// double_drain`. It is not: `drainer_lifecycle` is a pure simulator with
/// its own re-implementation of the algorithm and a
/// `guard_checks_generation` knob, and no test reads `mod.rs`.
/// `DrainerGuard` cannot even execute in the suite — it is built only by
/// `run_queue_drainer`, which needs an `AppHandle`.
///
/// So the model proved the algorithm and nothing pinned the code to it.
/// The newtype closes that by making the wrong construction impossible,
/// and these tests close the other half: they exercise the REAL removal,
/// headlessly, because moving it into a type is exactly what made it
/// reachable without a live drainer thread.
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A minimal queued entry. Built here rather than through a
    /// production helper: these tests are about the MAP, and what is in it
    /// is incidental.
    fn entry(id: u64) -> QueuedDelivery {
        QueuedDelivery {
            id,
            agent_id: "w-1".to_string(),
            from: "orch-1".to_string(),
            payload: crate::queue::QueuedPayload::Text("hi".to_string()),
            reason: crate::queue::EnqueueReason::Arrival,
            enqueued_ms: 0,
            coalesced: 0,
            group: Some("g1".try_into().unwrap()),
            to_orchestrator: false,
            session_id: None,
            delivery_kind: crate::model::Delivery::MidSession,
        }
    }

    /// Records what the writer was asked to do, and — the property that
    /// matters most — whether the queue lock was already free when it ran.
    struct SpyWriter<'a> {
        map: &'a QueueMap,
        writes: RefCell<Vec<String>>,
        lock_free_at_write: RefCell<Vec<bool>>,
    }

    impl<'a> SpyWriter<'a> {
        fn new(map: &'a QueueMap) -> Self {
            Self { map, writes: RefCell::new(Vec::new()), lock_free_at_write: RefCell::new(Vec::new()) }
        }
    }

    impl QueueSnapshotWriter for SpyWriter<'_> {
        fn write_queue_snapshot(&self, group: &crate::groupid::GroupId) {
            self.writes.borrow_mut().push(group.to_string());
            // `try_lock`, never `lock`: asserting the lock is free must not
            // be able to HANG the suite if it ever stops being free.
            self.lock_free_at_write.borrow_mut().push(self.map.inner.try_lock().is_ok());
        }
    }

    #[test]
    fn a_mutation_that_changed_the_snapshot_writes_it() {
        let map = QueueMap::new();
        let spy = SpyWriter::new(&map);
        let depth = map.mutate(&"g1".try_into().unwrap(), &spy, |queues| {
            queues.entry(7).or_default().push_back(entry(1));
            (queues[&7].len(), QueueDirty::snapshot())
        });
        assert_eq!(depth, 1);
        assert_eq!(*spy.writes.borrow(), ["g1"], "a snapshot-changing mutation owes exactly one write");
    }

    #[test]
    fn a_mutation_that_changed_nothing_writes_nothing() {
        let map = QueueMap::new();
        let spy = SpyWriter::new(&map);
        map.mutate(&"g1".try_into().unwrap(), &spy, |queues| {
            // The `entry().or_default()` shape `enqueue_text`'s RejectFull
            // arm hits: the map grew an EMPTY deque, which no byte of
            // `queue.json` is derived from.
            queues.entry(7).or_default();
            ((), QueueDirty::nothing_persisted())
        });
        assert!(spy.writes.borrow().is_empty(), "a mutation that changed nothing persisted must not write");
    }

    #[test]
    fn the_queue_lock_is_released_before_the_snapshot_is_written() {
        // The load-bearing half of `mutate`'s contract, and the one a
        // later "simplification" would break by moving the write inside
        // the guard scope: `persist_queues` does file I/O and takes
        // `queue_persist`, whose lock order is BEFORE `queues`. Writing
        // under the queue lock would invert that order and stall every
        // delivery in the registry behind a disk write.
        let map = QueueMap::new();
        let spy = SpyWriter::new(&map);
        map.mutate(&"g1".try_into().unwrap(), &spy, |queues| {
            queues.entry(7).or_default().push_back(entry(1));
            ((), QueueDirty::snapshot())
        });
        assert_eq!(*spy.lock_free_at_write.borrow(), [true],
            "the writer must run with the queue lock released");
    }

    #[test]
    fn release_only_fires_for_the_generation_that_currently_holds_the_pane() {
        let reg = DrainerRegistry::new();
        let g1 = reg.claim(7, || 1).expect("an unregistered pane is claimable");
        assert!(reg.is_registered(7));
        assert!(!reg.release(7, g1 + 1), "a generation that never held this pane removes nothing");
        assert!(reg.is_registered(7), "and leaves the real registration in place");
        assert!(reg.release(7, g1), "the holder's own release fires");
        assert!(!reg.is_registered(7));
    }

    #[test]
    fn a_stale_release_cannot_erase_a_successors_registration() {
        // #470 B1 round 2's actual defect, pinned against the real code for
        // the first time (see this module's test-mod doc): drainer 1 exits
        // and deregisters, drainer 2 claims the pane, and THEN drainer 1's
        // RAII guard finally drops. An unconditional removal here strips a
        // live successor's registration, a third arrival spawns drainer 3
        // alongside drainer 2, and the same queue entry is pasted twice.
        let reg = DrainerRegistry::new();
        let g1 = reg.claim(7, || 1).unwrap();
        assert!(reg.release(7, g1), "drainer 1's commit_exit deregisters");
        let g2 = reg.claim(7, || 2).expect("drainer 2 claims the now-free pane");
        assert!(!reg.release(7, g1), "drainer 1's LATE guard drop must be a no-op");
        assert!(reg.is_registered(7), "drainer 2 is still registered");
        assert!(reg.release(7, g2), "and only drainer 2's own release ends it");
    }

    #[test]
    fn claim_declines_while_a_registration_is_live_and_does_not_mint() {
        // `ensure_drainer`'s idempotence: at most one drainer per pane, and
        // the generation counter must not advance on a declined claim or
        // the tokens stop meaning "this spawn".
        let reg = DrainerRegistry::new();
        let minted = RefCell::new(0u64);
        let mut mint = || {
            let mut n = minted.borrow_mut();
            *n += 1;
            *n
        };
        assert_eq!(reg.claim(7, &mut mint), Some(1));
        assert_eq!(reg.claim(7, &mut mint), None, "a live registration declines a second claim");
        assert_eq!(*minted.borrow(), 1, "a declined claim must not burn a generation");
    }
}
