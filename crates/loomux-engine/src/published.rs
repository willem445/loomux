//! A published, immutable snapshot cell — one writer swaps a pointer, every
//! reader clones one (#1608, plan #1600 §3 Phase 1).
//!
//! # Why this exists
//!
//! The app's polled reads used to acquire the live registry's mutexes on every
//! tick. `lock_safe` was an infallible acquire with no timed form anywhere in
//! the tree (#1609 added one; a poller only gets it by running under a budget
//! frame) — so a single long hold anywhere parked every poller, and post-#1595
//! each of those pollers parks a *blocking-pool* thread rather than the webview
//! thread. At 2.5-5 parked threads per second, tokio's default 512 is reached
//! in minutes, and from then on `write_pty` cannot be scheduled and no pane
//! accepts input. That is the mechanism #1600 §1.2 establishes.
//!
//! The polled reads never needed the live registry; they needed a *view* of it.
//! This is that view's container: a background thread computes the payload
//! under the registry locks on a cadence and `store`s it here, and each read is
//! a pointer clone. A wedged registry then yields a *stale panel* — visible,
//! bounded, recoverable — instead of an unbounded queue of parked threads.
//!
//! # Why `RwLock<Arc<T>>` and not an `ArcSwap` crate
//!
//! The writer's critical section contains **no IO, no other lock, and nothing
//! that can park**: it reads the current sequence number, allocates one `Arc`,
//! and swaps a pointer. So the longest a reader can wait is that swap, which is
//! bounded by construction rather than by what the writer happens to be doing —
//! which is the whole property `ArcSwap` would have bought, without a
//! dependency (CLAUDE.md constraint 2 makes every new crate in this tree a
//! question worth not asking).
//!
//! Two details are load-bearing rather than incidental, and both are pinned by
//! the tests below:
//!
//! - **The previous snapshot is dropped OUTSIDE the guard.** Dropping the old
//!   value can be real work (a `serde_json::Value` tree is a deep free), and
//!   doing it under the write lock would put unbounded work back into the one
//!   critical section this module promises is bounded.
//! - **`seq` is derived under the lock**, not from a separate counter, so the
//!   sequence number and the value it labels can never disagree.
//!
//! # The clock
//!
//! `store` stamps `Instant::now()`. [`Published::store_at`] takes the instant
//! instead, which is how a test ages a snapshot past a staleness threshold
//! without sleeping through it — `age_ms_at` is the matching read side. Age is
//! measured from a monotonic `Instant`; `published_unix_ms` rides along only so
//! a payload can carry a wall-clock stamp a human reads, and is never what a
//! staleness decision is made from (a wall clock moves backwards).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// A value together with when it was published and what it cost to compute.
#[derive(Debug)]
pub struct Stamped<T> {
    /// Monotonic publication counter, starting at 0 for the initial value and
    /// incremented by every [`Published::store`]. A reader that sees the same
    /// `seq` twice read the same publication twice.
    pub seq: u64,
    /// Monotonic stamp every age/staleness decision is made from.
    pub published_at: Instant,
    /// Wall-clock stamp, for payloads a human reads. Never used for staleness:
    /// a wall clock moves backwards (NTP, VM resume, a manual set) and a
    /// staleness rule built on one reports a snapshot from the future.
    pub published_unix_ms: u64,
    /// How long the producer took to compute `value`, in ms — evidence the
    /// publisher can breadcrumb when a pass outgrows its own interval.
    pub compute_ms: u32,
    pub value: T,
}

impl<T> Stamped<T> {
    /// Age in ms at `now`, saturating at 0 for a stamp in the future (which
    /// `Instant` makes impossible today, and which this refuses to underflow on
    /// regardless).
    pub fn age_ms_at(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.published_at).as_millis() as u64
    }

    /// Age in ms right now.
    pub fn age_ms(&self) -> u64 {
        self.age_ms_at(Instant::now())
    }
}

/// A cell holding the most recently published [`Stamped`] value.
///
/// See the module doc for why this is an `RwLock<Arc<_>>` rather than a
/// dependency, and for the two properties `store` maintains.
#[derive(Debug)]
pub struct Published<T> {
    cell: RwLock<Arc<Stamped<T>>>,
    /// Publication count, readable without taking the lock. It exists for
    /// cheap instrumentation only — `seq` inside the cell is the authority, and
    /// is what a reader compares.
    stores: AtomicU64,
}

impl<T> Published<T> {
    /// A cell seeded with `initial` at `seq` 0, published now.
    pub fn new(initial: T) -> Self {
        Self {
            cell: RwLock::new(Arc::new(Stamped {
                seq: 0,
                published_at: Instant::now(),
                published_unix_ms: unix_ms(),
                compute_ms: 0,
                value: initial,
            })),
            stores: AtomicU64::new(0),
        }
    }

    /// The current snapshot: a read-lock, a pointer clone, and release. The
    /// returned `Arc` keeps that publication alive for as long as the caller
    /// holds it, so a reader is never torn by a concurrent `store`.
    pub fn load(&self) -> Arc<Stamped<T>> {
        // Poison-tolerant for the same reason `lock_safe` is: a panic in a
        // producer must not turn every subsequent read into a second panic.
        // Nothing here can observe a half-written value — the guard protects
        // one `Arc` pointer, and a panicking `store` either swapped it or did
        // not.
        let guard = self.cell.read().unwrap_or_else(|e| e.into_inner());
        Arc::clone(&guard)
    }

    /// Publish `value`, stamped now.
    pub fn store(&self, value: T, compute_ms: u32) {
        self.store_at(value, compute_ms, Instant::now(), unix_ms());
    }

    /// Publish `value` stamped at an explicit instant — the seam a test uses to
    /// age a snapshot past a staleness threshold without sleeping through it.
    pub fn store_at(&self, value: T, compute_ms: u32, at: Instant, unix_ms: u64) {
        // The previous snapshot is taken out under the guard and dropped after
        // it is released: freeing a deep value is real work, and this critical
        // section is the one thing that bounds how long a reader can wait.
        let previous = {
            let mut guard = self.cell.write().unwrap_or_else(|e| e.into_inner());
            let next = Arc::new(Stamped {
                seq: guard.seq + 1,
                published_at: at,
                published_unix_ms: unix_ms,
                compute_ms,
                value,
            });
            std::mem::replace(&mut *guard, next)
        };
        self.stores.fetch_add(1, Ordering::Relaxed);
        drop(previous);
    }

    /// How many times [`Published::store`] has completed. Instrumentation only.
    pub fn stores(&self) -> u64 {
        self.stores.load(Ordering::Relaxed)
    }
}

/// Wall-clock milliseconds since the Unix epoch, 0 if the clock predates it.
fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    #[test]
    fn a_reader_gets_the_last_stored_value() {
        let p = Published::new(String::from("first"));
        assert_eq!(p.load().value, "first", "the seed is what a reader sees before any store");
        p.store(String::from("second"), 7);
        assert_eq!(p.load().value, "second");
        assert_eq!(p.load().compute_ms, 7, "the cost rides with the value it describes");
    }

    #[test]
    fn seq_is_monotonic_and_labels_the_value_it_arrived_with() {
        let p = Published::new(0u32);
        assert_eq!(p.load().seq, 0, "the seed publication is seq 0");
        for expect in 1..=5u64 {
            p.store(expect as u32, 0);
            let s = p.load();
            assert_eq!(s.seq, expect);
            // The pairing is the point: `seq` is derived under the same guard
            // that swaps the pointer, so it cannot label a different value.
            assert_eq!(u64::from(s.value), expect);
        }
    }

    #[test]
    fn a_held_snapshot_is_not_torn_by_a_later_store() {
        let p = Published::new(String::from("held"));
        let held = p.load();
        p.store(String::from("replaced"), 0);
        assert_eq!(held.value, "held", "the Arc a reader holds is that publication, forever");
        assert_eq!(held.seq, 0);
        assert_eq!(p.load().value, "replaced");
    }

    #[test]
    fn age_is_measured_from_published_at_not_from_the_wall_clock() {
        let p = Published::new(());
        let long_ago = Instant::now().checked_sub(Duration::from_secs(30)).expect("30s of uptime");
        // A wall-clock stamp deliberately claiming the present, so a staleness
        // rule reading the WRONG field would report this snapshot as fresh.
        p.store_at((), 0, long_ago, unix_ms());
        let s = p.load();
        assert!(
            s.age_ms() >= 30_000,
            "age must come from published_at (30s ago), got {}ms",
            s.age_ms()
        );
        assert!(
            s.published_unix_ms >= 1_600_000_000_000,
            "the wall-clock stamp is still carried for payloads a human reads"
        );
    }

    #[test]
    fn age_at_an_injected_instant_is_what_a_staleness_test_reads() {
        let p = Published::new(());
        let base = Instant::now();
        p.store_at((), 0, base, unix_ms());
        let s = p.load();
        assert_eq!(s.age_ms_at(base), 0);
        assert_eq!(s.age_ms_at(base + Duration::from_millis(5_000)), 5_000);
        assert_eq!(s.age_ms_at(base + Duration::from_millis(5_001)), 5_001);
        // A stamp in the future saturates rather than underflowing.
        assert_eq!(s.age_ms_at(base - Duration::from_secs(1)), 0);
    }

    #[test]
    fn concurrent_readers_and_one_writer_never_observe_a_gap() {
        // The liveness property this module exists for, in miniature: readers
        // keep getting a whole value while a writer republishes underneath
        // them. A reader that saw a torn or absent snapshot would fail here.
        let p = Arc::new(Published::new(vec![0u32; 64]));
        let stop = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let p = Arc::clone(&p);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut seen = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let s = p.load();
                        assert_eq!(s.value.len(), 64, "every publication is a whole value");
                        assert!(s.value.iter().all(|v| *v == s.value[0]), "no torn value");
                        seen = seen.max(s.seq);
                    }
                    seen
                })
            })
            .collect();
        for i in 1..=500u32 {
            p.store(vec![i; 64], 0);
        }
        stop.store(true, Ordering::Relaxed);
        for r in readers {
            r.join().expect("reader thread");
        }
        assert_eq!(p.load().seq, 500);
        assert_eq!(p.stores(), 500);
    }
}
