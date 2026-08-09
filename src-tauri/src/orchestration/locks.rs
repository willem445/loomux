//! Named lock resources (#858) — the pure state machine.
//!
//! A repo declares scarce things in `.loomux/workflow.yml`:
//!
//! ```yaml
//! resources:
//!   build: { slots: 1, max_hold_minutes: 45 }
//! ```
//!
//! and agents serialize on them with `acquire_lock` / `release_lock` /
//! `list_locks`. What a resource *means* is the repo's business (CLAUDE.md
//! constraint 8): this module knows names, slot counts and clocks, and nothing
//! about compilers, GPUs, ports or devices.
//!
//! **Cooperative, not enforced.** #318's resource guard tried to enforce
//! serialization by shadowing the guarded program on `PATH`; #322 was closed
//! unmerged because a shim only intercepts the shells it shadows — a
//! PowerShell or absolute-path invocation walks straight past it (#335). This
//! mechanism does not pretend otherwise: an agent holds a lock because it
//! asked for one, and the audit log records who held what for how long.
//! Advisory locking that is *honest* about being advisory beats enforcement
//! that is bypassable while claiming not to be.
//!
//! **Nothing here ever blocks.** `acquire_lock` returns immediately — granted,
//! or queued with a position — and a grant that arrives later is delivered as
//! a `[loomux]` pane notice, exactly like a `notify_when` watch resolving. A
//! blocking acquire would be the #590 deadlock by construction: the notice
//! saying "it's yours" is *typed into the pane*, and a pane blocked mid-call
//! cannot take delivery of the thing it is blocked on.
//!
//! **Every wait and every hold is bounded** (the lessons-file rule: a
//! suppression driven by a fallible signal must be bounded). A hold expires at
//! `max_hold_minutes`; a queued request expires at its own `wait_minutes`; a
//! holder or waiter whose pane is gone is dropped on the next sweep. Every one
//! of those is an audited event, never a silent state change.
//!
//! Design note: `doc/design/lock-resources.md`.

use std::collections::{BTreeMap, VecDeque};

/// Longest `note` an agent can attach to a hold or a queued request. It is
/// echoed to a human in `list_locks` and in the board chrome, so it is a label,
/// not a payload.
pub const MAX_NOTE_CHARS: usize = 200;

/// A single hold on one slot of a resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    /// The agent id holding the slot.
    pub agent: String,
    /// The caller's own label for what it is doing (`""` when it gave none).
    pub note: String,
    /// When the slot was granted.
    pub acquired_ms: u64,
    /// When `max_hold_minutes` runs out and the sweep reclaims it. Set once,
    /// at grant time, and **never extended** — see [`LockTable::acquire`].
    pub expires_ms: u64,
}

/// A queued request waiting for a slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiter {
    pub agent: String,
    pub note: String,
    /// When the request joined the queue — the FIFO order is this order.
    pub queued_ms: u64,
    /// When the *wait* gives up (`wait_minutes`). Distinct from a hold's
    /// `expires_ms`: this bounds queueing, that bounds holding.
    pub expires_ms: u64,
}

/// One declared resource's live state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub name: String,
    pub slots: u32,
    pub max_hold_minutes: u32,
    pub holders: Vec<Hold>,
    pub queue: VecDeque<Waiter>,
}

impl Resource {
    fn free_slots(&self) -> bool {
        (self.holders.len() as u32) < self.slots
    }
}

/// What an `acquire_lock` call resolved to. Every variant is a *success*: a
/// queue position is an answer, not a failure, and the caller is told where it
/// stands rather than left hanging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acquired {
    /// A slot was free; the caller holds it until `expires_ms`.
    Granted { expires_ms: u64 },
    /// The caller already held this lock. Idempotent: the original hold and
    /// its original deadline stand, untouched.
    AlreadyHeld { expires_ms: u64 },
    /// No free slot; the caller is now `position`th in line (1-based).
    Queued { position: usize, expires_ms: u64 },
    /// The caller was already queued. Idempotent in the same way as
    /// `AlreadyHeld`: it keeps its original place in line, so re-asking can
    /// never cost an agent its turn.
    AlreadyQueued { position: usize, expires_ms: u64 },
}

/// What a `release_lock` call resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Released {
    /// The caller held a slot and gave it up. `granted` names the waiter that
    /// inherited it, if any — the registry turns that into a pane notice.
    Held { granted: Option<Grant> },
    /// The caller was not holding the lock but *was* queued for it, and its
    /// request has been withdrawn. Not an error: without this, a worker that
    /// no longer needs a resource cannot leave the queue except by timing
    /// out — and would then be granted a slot it never uses, blocking
    /// everyone behind it for a full `max_hold_minutes`.
    QueueCancelled { position: usize },
}

/// A slot handed to a waiter. The registry audits it and types the notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub resource: String,
    pub agent: String,
    pub note: String,
    pub expires_ms: u64,
    /// How long that agent waited in the queue.
    pub waited_ms: u64,
}

/// Something the sweep did on its own initiative. Each one is audited and, for
/// the variants that concern a live agent, typed into that agent's pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reclaimed {
    /// A hold outlived `max_hold_minutes`.
    HoldExpired { resource: String, agent: String, held_ms: u64 },
    /// A holder's pane is gone (killed, crashed, finished without releasing).
    HolderGone { resource: String, agent: String, held_ms: u64 },
    /// A queued request outlived its `wait_minutes`.
    WaitTimedOut { resource: String, agent: String, waited_ms: u64 },
    /// A waiter's pane is gone.
    WaiterGone { resource: String, agent: String, waited_ms: u64 },
}

/// One sweep's output: what was taken away, and what was handed on as a result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sweep {
    pub reclaimed: Vec<Reclaimed>,
    pub granted: Vec<Grant>,
}

impl Sweep {
    pub fn is_empty(&self) -> bool {
        self.reclaimed.is_empty() && self.granted.is_empty()
    }
}

/// One group's live lock state. Built from the repo's declared `resources:`
/// and held in memory only — a loomux restart empties it, exactly as it empties
/// the notification registry, and for the same reason: every pane that could
/// have been holding a lock died with it, so a lock file surviving the restart
/// could only ever describe holders that no longer exist.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockTable {
    resources: BTreeMap<String, Resource>,
}

impl LockTable {
    /// Reconcile a live table against what the repo currently declares, so an
    /// edit to `.loomux/workflow.yml` takes effect on the next call instead of
    /// at the next loomux restart (the same live-re-read posture
    /// `merge_queue_policy` takes on its own block).
    ///
    /// - a newly declared resource appears, empty;
    /// - a resource that is no longer declared is **removed**, and its name is
    ///   returned so the caller can audit the holders and waiters it took with
    ///   it — silently dropping live holders would be exactly the "a claim the
    ///   code doesn't back" defect;
    /// - `slots` / `max_hold_minutes` are updated in place, and existing holds
    ///   keep the deadline they were granted under. A shrunk `slots` therefore
    ///   never revokes a hold: it stops *new* grants until the count falls
    ///   back under the new limit, which is the only reading that cannot
    ///   yank a resource out from under a build already running.
    pub fn sync(&mut self, declared: &BTreeMap<String, super::workflow::ResourcePolicy>) -> Vec<Resource> {
        let dropped: Vec<Resource> = self
            .resources
            .keys()
            .filter(|k| !declared.contains_key(*k))
            .cloned()
            .collect::<Vec<String>>()
            .into_iter()
            .filter_map(|k| self.resources.remove(&k))
            .collect();
        for (name, p) in declared {
            match self.resources.get_mut(name) {
                Some(r) => {
                    r.slots = p.slots;
                    r.max_hold_minutes = p.max_hold_minutes;
                }
                None => {
                    self.resources.insert(
                        name.clone(),
                        Resource {
                            name: name.clone(),
                            slots: p.slots,
                            max_hold_minutes: p.max_hold_minutes,
                            holders: Vec::new(),
                            queue: VecDeque::new(),
                        },
                    );
                }
            }
        }
        dropped
    }

    /// Push every deadline out by `extra` ms — how a paused group's holds and
    /// queued requests survive the pause instead of evaporating at the moment
    /// it resumes. The `notify_tick` TTL-freeze precedent, applied to locks.
    pub fn extend_deadlines(&mut self, extra: u64) {
        for r in self.resources.values_mut() {
            for h in r.holders.iter_mut() {
                h.expires_ms = h.expires_ms.saturating_add(extra);
            }
            for w in r.queue.iter_mut() {
                w.expires_ms = w.expires_ms.saturating_add(extra);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// The declared resource names, in the order a human reads them.
    pub fn names(&self) -> Vec<String> {
        self.resources.keys().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<&Resource> {
        self.resources.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Resource> {
        self.resources.values()
    }

    fn resource_mut(&mut self, name: &str) -> Result<&mut Resource, String> {
        if self.resources.contains_key(name) {
            // Two lookups rather than one, so the error branch can borrow
            // `self` to name the alternatives; the map is a handful of entries.
            return Ok(self.resources.get_mut(name).expect("checked above"));
        }
        Err(unknown_resource_error(name, &self.names()))
    }

    /// Ask for a slot. Never blocks and never fails for contention — the
    /// caller either holds the lock when this returns or knows its place in
    /// line.
    ///
    /// `wait_minutes` bounds only the *queueing*; `max_hold_minutes` (from the
    /// repo's config) bounds the hold. Both are set once, at the moment the
    /// state is entered, and re-calling this refreshes **neither**: a hold
    /// whose deadline moved every time its holder said "still mine" would be
    /// an unbounded hold with extra steps, and a re-queued waiter that went to
    /// the back of the line would be punished for asking.
    pub fn acquire(
        &mut self,
        name: &str,
        agent: &str,
        note: &str,
        now_ms: u64,
        wait_minutes: u32,
    ) -> Result<Acquired, String> {
        let note = trim_note(note);
        let r = self.resource_mut(name)?;
        if let Some(h) = r.holders.iter().find(|h| h.agent == agent) {
            return Ok(Acquired::AlreadyHeld { expires_ms: h.expires_ms });
        }
        if let Some((i, w)) = r.queue.iter().enumerate().find(|(_, w)| w.agent == agent) {
            return Ok(Acquired::AlreadyQueued { position: i + 1, expires_ms: w.expires_ms });
        }
        if r.free_slots() {
            let expires_ms = now_ms + minutes_ms(r.max_hold_minutes);
            r.holders.push(Hold {
                agent: agent.to_string(),
                note,
                acquired_ms: now_ms,
                expires_ms,
            });
            return Ok(Acquired::Granted { expires_ms });
        }
        let expires_ms = now_ms + minutes_ms(wait_minutes);
        r.queue.push_back(Waiter {
            agent: agent.to_string(),
            note,
            queued_ms: now_ms,
            expires_ms,
        });
        Ok(Acquired::Queued { position: r.queue.len(), expires_ms })
    }

    /// Give up a slot — or withdraw a queued request. Errors only when the
    /// caller has no relationship with the resource at all, which is a real
    /// mistake worth telling it about (it believes it is serialized and is
    /// not).
    pub fn release(&mut self, name: &str, agent: &str, now_ms: u64) -> Result<Released, String> {
        let r = self.resource_mut(name)?;
        if let Some(i) = r.holders.iter().position(|h| h.agent == agent) {
            r.holders.remove(i);
            let granted = promote(r, now_ms);
            return Ok(Released::Held { granted });
        }
        if let Some(i) = r.queue.iter().position(|w| w.agent == agent) {
            r.queue.remove(i);
            return Ok(Released::QueueCancelled { position: i + 1 });
        }
        Err(format!(
            "you do not hold '{name}' and are not queued for it — nothing to release. \
             Call list_locks() to see who does hold it."
        ))
    }

    /// Drop every hold and queued request belonging to one agent. Called when
    /// a pane exits, so a finished worker never strands a slot even for one
    /// sweep interval.
    pub fn drop_agent(&mut self, agent: &str, now_ms: u64) -> Sweep {
        let mut out = Sweep::default();
        for r in self.resources.values_mut() {
            if let Some(i) = r.holders.iter().position(|h| h.agent == agent) {
                let h = r.holders.remove(i);
                out.reclaimed.push(Reclaimed::HolderGone {
                    resource: r.name.clone(),
                    agent: h.agent,
                    held_ms: now_ms.saturating_sub(h.acquired_ms),
                });
            }
            if let Some(i) = r.queue.iter().position(|w| w.agent == agent) {
                let w = r.queue.remove(i).expect("position just found");
                out.reclaimed.push(Reclaimed::WaiterGone {
                    resource: r.name.clone(),
                    agent: w.agent,
                    waited_ms: now_ms.saturating_sub(w.queued_ms),
                });
            }
            out.granted.extend(promote_all(r, now_ms));
        }
        out
    }

    /// The periodic reclaim pass: expired holds, expired waits, and holders or
    /// waiters whose panes are gone. `is_live` answers "does this agent still
    /// have a pane?" — injected rather than looked up so this whole module
    /// stays a pure function of (state, clock, liveness).
    ///
    /// Ordering matters and is deliberate: reclaim first, promote second, so
    /// one pass can hand a slot released by a dead holder straight to the head
    /// of the queue instead of leaving it idle until the next tick.
    pub fn sweep(&mut self, now_ms: u64, is_live: &dyn Fn(&str) -> bool) -> Sweep {
        let mut out = Sweep::default();
        for r in self.resources.values_mut() {
            let mut kept: Vec<Hold> = Vec::with_capacity(r.holders.len());
            for h in std::mem::take(&mut r.holders) {
                let held_ms = now_ms.saturating_sub(h.acquired_ms);
                // Liveness first: a dead holder's expiry is uninteresting, and
                // "the pane is gone" is the more useful audit line of the two.
                if !is_live(&h.agent) {
                    out.reclaimed.push(Reclaimed::HolderGone {
                        resource: r.name.clone(),
                        agent: h.agent,
                        held_ms,
                    });
                } else if now_ms >= h.expires_ms {
                    out.reclaimed.push(Reclaimed::HoldExpired {
                        resource: r.name.clone(),
                        agent: h.agent,
                        held_ms,
                    });
                } else {
                    kept.push(h);
                }
            }
            r.holders = kept;

            let mut kept_q: VecDeque<Waiter> = VecDeque::with_capacity(r.queue.len());
            for w in std::mem::take(&mut r.queue) {
                let waited_ms = now_ms.saturating_sub(w.queued_ms);
                if !is_live(&w.agent) {
                    out.reclaimed.push(Reclaimed::WaiterGone {
                        resource: r.name.clone(),
                        agent: w.agent,
                        waited_ms,
                    });
                } else if now_ms >= w.expires_ms {
                    out.reclaimed.push(Reclaimed::WaitTimedOut {
                        resource: r.name.clone(),
                        agent: w.agent,
                        waited_ms,
                    });
                } else {
                    kept_q.push_back(w);
                }
            }
            r.queue = kept_q;

            out.granted.extend(promote_all(r, now_ms));
        }
        out
    }
}

/// Hand the head of the queue a freed slot, if there is one of each.
fn promote(r: &mut Resource, now_ms: u64) -> Option<Grant> {
    if !r.free_slots() {
        return None;
    }
    let w = r.queue.pop_front()?;
    let expires_ms = now_ms + minutes_ms(r.max_hold_minutes);
    r.holders.push(Hold {
        agent: w.agent.clone(),
        note: w.note.clone(),
        acquired_ms: now_ms,
        expires_ms,
    });
    Some(Grant {
        resource: r.name.clone(),
        agent: w.agent,
        note: w.note,
        expires_ms,
        waited_ms: now_ms.saturating_sub(w.queued_ms),
    })
}

/// Fill every free slot from the queue — a sweep can free several at once.
fn promote_all(r: &mut Resource, now_ms: u64) -> Vec<Grant> {
    let mut out = Vec::new();
    while let Some(g) = promote(r, now_ms) {
        out.push(g);
    }
    out
}

fn minutes_ms(m: u32) -> u64 {
    u64::from(m) * 60_000
}

fn trim_note(note: &str) -> String {
    note.trim().chars().take(MAX_NOTE_CHARS).collect()
}

/// The "no such resource" message, in one place so the tools and the sweep
/// never disagree. Naming the declared set is the whole value of it: a typo
/// (`buld`) is otherwise indistinguishable from a repo that never declared the
/// resource, and an agent cannot fix either without knowing which it hit.
pub fn unknown_resource_error(name: &str, declared: &[String]) -> String {
    if declared.is_empty() {
        return format!(
            "this repo declares no lock resources, so there is no '{name}' to lock — \
             a resource has to be declared in .loomux/workflow.yml under `resources:`"
        );
    }
    format!(
        "unknown lock resource '{name}' — this repo declares: {}",
        declared.join(", ")
    )
}

// ---------- tests ----------
//
// Pure: a `LockTable`, a fake clock, and a closure for liveness. Nothing here
// links the lib (CLAUDE.md constraint 4 is unaffected — same posture as
// `notify.rs`'s own inline tests). The wired behaviour — the MCP tools, the
// audit lines, the pane notices — is covered in `tests/orchestration.rs`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::workflow::ResourcePolicy;

    const MIN: u64 = 60_000;

    /// A table built the way production builds one: empty, then reconciled
    /// against the declared set. There is deliberately no other constructor —
    /// a `new(declared)` that only tests called would be a second way in that
    /// nothing shipped exercises.
    fn table(specs: &[(&str, u32, u32)]) -> LockTable {
        let declared: BTreeMap<String, ResourcePolicy> = specs
            .iter()
            .map(|(n, slots, hold)| {
                (n.to_string(), ResourcePolicy { slots: *slots, max_hold_minutes: *hold })
            })
            .collect();
        let mut t = LockTable::default();
        assert!(t.sync(&declared).is_empty(), "an empty table drops nothing");
        t
    }

    fn all_live(_: &str) -> bool {
        true
    }

    #[test]
    fn single_slot_serializes_and_queues_fifo() {
        let mut t = table(&[("build", 1, 45)]);
        assert!(matches!(
            t.acquire("build", "w-1", "", 0, 60).unwrap(),
            Acquired::Granted { .. }
        ));
        // Two more contend; both are told where they stand, neither errors.
        assert_eq!(
            t.acquire("build", "w-2", "", 1_000, 60).unwrap(),
            Acquired::Queued { position: 1, expires_ms: 1_000 + 60 * MIN }
        );
        assert_eq!(
            t.acquire("build", "w-3", "", 2_000, 60).unwrap(),
            Acquired::Queued { position: 2, expires_ms: 2_000 + 60 * MIN }
        );
        // FIFO: w-2 queued first, so w-2 inherits the slot — not w-3.
        let Released::Held { granted } = t.release("build", "w-1", 3_000).unwrap() else {
            panic!("w-1 held it");
        };
        let g = granted.expect("the head of the queue takes the slot");
        assert_eq!(g.agent, "w-2");
        assert_eq!(g.waited_ms, 2_000);
        assert_eq!(t.get("build").unwrap().queue.len(), 1);
        // And w-3 moves up rather than staying pinned at position 2.
        assert_eq!(
            t.acquire("build", "w-3", "", 4_000, 60).unwrap(),
            Acquired::AlreadyQueued { position: 1, expires_ms: 2_000 + 60 * MIN }
        );
    }

    #[test]
    fn multi_slot_admits_up_to_slots_then_queues() {
        let mut t = table(&[("gpu", 2, 30)]);
        assert!(matches!(t.acquire("gpu", "w-1", "", 0, 60).unwrap(), Acquired::Granted { .. }));
        assert!(matches!(t.acquire("gpu", "w-2", "", 0, 60).unwrap(), Acquired::Granted { .. }));
        assert_eq!(
            t.acquire("gpu", "w-3", "", 0, 60).unwrap(),
            Acquired::Queued { position: 1, expires_ms: 60 * MIN }
        );
        assert_eq!(t.get("gpu").unwrap().holders.len(), 2);
    }

    #[test]
    fn double_acquire_is_idempotent_and_never_extends_the_hold() {
        let mut t = table(&[("build", 1, 45)]);
        let Acquired::Granted { expires_ms } = t.acquire("build", "w-1", "first", 0, 60).unwrap()
        else {
            panic!("free slot");
        };
        assert_eq!(expires_ms, 45 * MIN);
        // 40 minutes later the same agent asks again. It still holds it, on the
        // SAME deadline — otherwise max_hold_minutes bounds nothing.
        assert_eq!(
            t.acquire("build", "w-1", "second", 40 * MIN, 60).unwrap(),
            Acquired::AlreadyHeld { expires_ms: 45 * MIN }
        );
        assert_eq!(t.get("build").unwrap().holders.len(), 1);
        // The re-ask did not overwrite the original note either.
        assert_eq!(t.get("build").unwrap().holders[0].note, "first");
    }

    #[test]
    fn releasing_a_lock_you_never_took_is_an_error() {
        let mut t = table(&[("build", 1, 45)]);
        t.acquire("build", "w-1", "", 0, 60).unwrap();
        let err = t.release("build", "w-9", 0).unwrap_err();
        assert!(err.contains("do not hold"), "{err}");
        // And it did not disturb the real holder.
        assert_eq!(t.get("build").unwrap().holders[0].agent, "w-1");
    }

    #[test]
    fn a_waiter_can_withdraw_instead_of_blocking_the_queue() {
        let mut t = table(&[("build", 1, 45)]);
        t.acquire("build", "w-1", "", 0, 60).unwrap();
        t.acquire("build", "w-2", "", 0, 60).unwrap();
        t.acquire("build", "w-3", "", 0, 60).unwrap();
        assert_eq!(
            t.release("build", "w-2", 0).unwrap(),
            Released::QueueCancelled { position: 1 }
        );
        // w-3 inherits, because w-2 left rather than being granted a slot it
        // would have sat on for a full max_hold.
        let Released::Held { granted } = t.release("build", "w-1", 1_000).unwrap() else {
            panic!("w-1 held it");
        };
        assert_eq!(granted.unwrap().agent, "w-3");
    }

    #[test]
    fn an_expired_hold_is_reclaimed_and_handed_to_the_queue() {
        let mut t = table(&[("build", 1, 45)]);
        t.acquire("build", "w-1", "", 0, 60).unwrap();
        t.acquire("build", "w-2", "", 0, 60).unwrap();
        // One minute before the deadline: nothing happens.
        assert!(t.sweep(44 * MIN, &all_live).is_empty());
        let s = t.sweep(45 * MIN, &all_live);
        assert_eq!(
            s.reclaimed,
            vec![Reclaimed::HoldExpired {
                resource: "build".into(),
                agent: "w-1".into(),
                held_ms: 45 * MIN
            }]
        );
        assert_eq!(s.granted.len(), 1);
        assert_eq!(s.granted[0].agent, "w-2");
        // The new holder gets a FULL hold window from the grant, not the
        // remainder of the old one.
        assert_eq!(s.granted[0].expires_ms, 45 * MIN + 45 * MIN);
    }

    #[test]
    fn a_dead_holder_is_reclaimed_before_its_hold_expires() {
        let mut t = table(&[("build", 1, 45)]);
        t.acquire("build", "w-1", "", 0, 60).unwrap();
        t.acquire("build", "w-2", "", 0, 60).unwrap();
        // w-1's pane is gone 2 minutes in — 43 minutes before max_hold would
        // have noticed. Without liveness reclaim, w-2 waits out its own
        // wait_minutes and never gets the slot at all.
        let s = t.sweep(2 * MIN, &|a: &str| a != "w-1");
        assert_eq!(
            s.reclaimed,
            vec![Reclaimed::HolderGone {
                resource: "build".into(),
                agent: "w-1".into(),
                held_ms: 2 * MIN
            }]
        );
        assert_eq!(s.granted.len(), 1);
        assert_eq!(s.granted[0].agent, "w-2");
        assert_eq!(t.get("build").unwrap().holders[0].agent, "w-2");
    }

    #[test]
    fn a_dead_waiter_leaves_the_queue_without_taking_a_slot() {
        let mut t = table(&[("build", 1, 45)]);
        t.acquire("build", "w-1", "", 0, 60).unwrap();
        t.acquire("build", "w-2", "", 0, 60).unwrap();
        t.acquire("build", "w-3", "", 0, 60).unwrap();
        let s = t.sweep(MIN, &|a: &str| a != "w-2");
        assert_eq!(
            s.reclaimed,
            vec![Reclaimed::WaiterGone {
                resource: "build".into(),
                agent: "w-2".into(),
                waited_ms: MIN
            }]
        );
        // No grant: w-1 still holds the only slot.
        assert!(s.granted.is_empty());
        let Released::Held { granted } = t.release("build", "w-1", 2 * MIN).unwrap() else {
            panic!("w-1 held it");
        };
        assert_eq!(granted.unwrap().agent, "w-3");
    }

    #[test]
    fn a_wait_times_out_on_its_own_clock_not_the_holds() {
        let mut t = table(&[("build", 1, 600)]);
        t.acquire("build", "w-1", "", 0, 60).unwrap();
        t.acquire("build", "w-2", "", 0, 5).unwrap();
        let s = t.sweep(5 * MIN, &all_live);
        assert_eq!(
            s.reclaimed,
            vec![Reclaimed::WaitTimedOut {
                resource: "build".into(),
                agent: "w-2".into(),
                waited_ms: 5 * MIN
            }]
        );
        assert!(t.get("build").unwrap().queue.is_empty());
        // The holder is untouched — its own 600-minute window is nowhere near.
        assert_eq!(t.get("build").unwrap().holders[0].agent, "w-1");
    }

    #[test]
    fn dropping_an_agent_releases_its_hold_and_its_queued_request() {
        let mut t = table(&[("build", 1, 45), ("gpu", 1, 45)]);
        t.acquire("build", "w-1", "", 0, 60).unwrap();
        t.acquire("gpu", "w-2", "", 0, 60).unwrap();
        t.acquire("gpu", "w-1", "", 0, 60).unwrap(); // w-1 also waits for gpu
        let s = t.drop_agent("w-1", MIN);
        assert_eq!(s.reclaimed.len(), 2, "{:?}", s.reclaimed);
        assert!(t.get("build").unwrap().holders.is_empty());
        assert!(t.get("gpu").unwrap().queue.is_empty());
        assert_eq!(t.get("gpu").unwrap().holders[0].agent, "w-2");
    }

    #[test]
    fn a_freed_multi_slot_resource_promotes_every_waiter_it_can() {
        let mut t = table(&[("gpu", 2, 45)]);
        t.acquire("gpu", "w-1", "", 0, 60).unwrap();
        t.acquire("gpu", "w-2", "", 0, 60).unwrap();
        t.acquire("gpu", "w-3", "", 0, 60).unwrap();
        t.acquire("gpu", "w-4", "", 0, 60).unwrap();
        // Both holders die at once: one sweep must fill BOTH slots, not one.
        let s = t.sweep(MIN, &|a: &str| a != "w-1" && a != "w-2");
        assert_eq!(s.granted.iter().map(|g| g.agent.as_str()).collect::<Vec<_>>(), ["w-3", "w-4"]);
        assert!(t.get("gpu").unwrap().queue.is_empty());
    }

    #[test]
    fn an_undeclared_resource_names_what_is_declared() {
        let mut t = table(&[("build", 1, 45), ("gpu", 1, 45)]);
        let err = t.acquire("buld", "w-1", "", 0, 60).unwrap_err();
        assert!(err.contains("build, gpu"), "{err}");
        let mut empty = table(&[]);
        let err = empty.acquire("build", "w-1", "", 0, 60).unwrap_err();
        assert!(err.contains("declares no lock resources"), "{err}");
    }

    #[test]
    fn sync_adds_removes_and_retunes_without_revoking_a_live_hold() {
        let mut t = table(&[("build", 1, 45)]);
        t.acquire("build", "w-1", "", 0, 60).unwrap();
        let declared: BTreeMap<String, ResourcePolicy> = [
            // build survives, but is retuned to 2 slots / 90 min…
            ("build".to_string(), ResourcePolicy { slots: 2, max_hold_minutes: 90 }),
            // …and a brand-new resource appears.
            ("gpu".to_string(), ResourcePolicy::default()),
        ]
        .into_iter()
        .collect();
        assert!(t.sync(&declared).is_empty());
        assert_eq!(t.names(), vec!["build".to_string(), "gpu".to_string()]);
        // The live hold keeps the deadline it was GRANTED under — a config
        // edit must never retroactively move a running build's deadline.
        assert_eq!(t.get("build").unwrap().holders[0].expires_ms, 45 * MIN);
        // The widened slot count applies to the next acquire immediately.
        assert!(matches!(
            t.acquire("build", "w-2", "", MIN, 60).unwrap(),
            Acquired::Granted { .. }
        ));
        // Undeclaring a resource hands its live state back so it can be audited.
        let dropped = t.sync(&[("gpu".to_string(), ResourcePolicy::default())].into_iter().collect());
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].name, "build");
        assert_eq!(dropped[0].holders.len(), 2);
        assert!(t.get("build").is_none());
    }

    #[test]
    fn shrinking_slots_stops_new_grants_without_evicting_anyone() {
        let mut t = table(&[("gpu", 2, 45)]);
        t.acquire("gpu", "w-1", "", 0, 60).unwrap();
        t.acquire("gpu", "w-2", "", 0, 60).unwrap();
        t.sync(&[("gpu".to_string(), ResourcePolicy { slots: 1, max_hold_minutes: 45 })].into_iter().collect());
        assert_eq!(t.get("gpu").unwrap().holders.len(), 2, "no live hold is revoked");
        // Over capacity, so a release does NOT promote — the queue waits until
        // the count falls back under the new limit.
        assert_eq!(
            t.acquire("gpu", "w-3", "", 0, 60).unwrap(),
            Acquired::Queued { position: 1, expires_ms: 60 * MIN }
        );
        let Released::Held { granted } = t.release("gpu", "w-1", MIN).unwrap() else {
            panic!("w-1 held it");
        };
        assert!(granted.is_none(), "still at the new 1-slot limit");
        let Released::Held { granted } = t.release("gpu", "w-2", 2 * MIN).unwrap() else {
            panic!("w-2 held it");
        };
        assert_eq!(granted.unwrap().agent, "w-3");
    }

    #[test]
    fn extending_deadlines_across_a_pause_saves_a_hold_that_would_have_expired() {
        let mut t = table(&[("build", 1, 45)]);
        t.acquire("build", "w-1", "", 0, 60).unwrap();
        // The group was paused for an hour: without the credit, the very first
        // sweep after it resumes reclaims a lock whose holder was frozen too.
        t.extend_deadlines(60 * MIN);
        assert!(t.sweep(60 * MIN, &all_live).is_empty());
        assert_eq!(t.get("build").unwrap().holders[0].agent, "w-1");
        // And it still expires — the credit shifts the deadline, it doesn't
        // remove it.
        assert_eq!(t.sweep(105 * MIN, &all_live).reclaimed.len(), 1);
    }

    #[test]
    fn a_note_is_trimmed_and_capped() {
        let mut t = table(&[("build", 1, 45)]);
        t.acquire("build", "w-1", &format!("  {}  ", "x".repeat(500)), 0, 60).unwrap();
        assert_eq!(t.get("build").unwrap().holders[0].note.len(), MAX_NOTE_CHARS);
    }
}
