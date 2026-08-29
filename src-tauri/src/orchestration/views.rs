//! The snapshot publisher for polled reads (#1608, plan #1600 §3 Phase 1).
//!
//! # What this replaces
//!
//! Two loops used to poll the registry directly: the group view's 2 s batch of
//! **ten** `orch_*` commands (`groupview.ts` `load()`), and the tab strip's 4 s
//! sweep of **two per group-bound tab** (`tabbar.ts` `pollStatusOnce()`). Each
//! of those commands acquires registry mutexes, and `lock_safe` is
//! `Mutex::lock` with poison recovery — no timeout, no try-lock. So one long
//! hold anywhere parks every poller; post-#1595 each parks a *blocking-pool*
//! thread; at 2.5-5/s tokio's default 512 is reached in minutes; and from then
//! on `write_pty` cannot be scheduled and no pane accepts input. #1600 §1.2.
//!
//! #1602/#1604 made that accumulation unreachable by single-flighting the two
//! poll sites — one outstanding call per site, never a queue. What it could not
//! do is make the *view* recover: a call that never settles leaves the panel
//! showing its last payload with no disclosure at all (#1604 review N3). This
//! module is the other half.
//!
//! **The polled reads never needed the live registry; they needed a view of
//! it.** One owned thread computes every payload on a cadence, under the
//! registry locks, and publishes an immutable snapshot into a
//! [`Published`](loomux_engine::published::Published) cell. Every polled read
//! is then a pointer clone: no registry lock, so no possible wait, so a wedged
//! registry yields a **stale panel** — visible, bounded, recoverable (INV-6
//! applied to the registry) — instead of an unbounded queue of parked threads.
//! Exactly one thread parks: this module's.
//!
//! # The two tiers, and why
//!
//! - **Strip tier** (`summary`, `usage`) — computed for **every** group, every
//!   tick. The tab strip already polls every group-bound tab, so this adds no
//!   work; it just moves it off the poll path.
//! - **View tier** (the other eight) — computed only for a group holding a
//!   *view lease*. [`orch_group_view`](super::orch_group_view) stamps
//!   `lease(group) = now`; the publisher computes the view tier while that
//!   lease is younger than [`VIEW_LEASE_MS`]. Without the lease, opening one
//!   group view would put `merge_queue.json` reads, `workflow.yml` parses and
//!   `git` default-branch spawns on **every** group forever, rather than on the
//!   one view that is open — which is exactly today's rate.
//!
//! A lapsed lease **drops** the group's view tier rather than carrying it
//! forward. Carrying it would let `orch_group_view` answer a reopened panel
//! with ten-minute-old data under a fresh-looking `age_ms`, which is the false
//! freshness this whole slice exists to remove. Dropping it means the first
//! read after a reopen honestly says `view_ready: false`, and the caller
//! re-asks on a short bounded ladder (`src/groupview.ts`).
//!
//! # Freshness is per group, never per snapshot
//!
//! [`GroupView::computed_at`] stamps each group individually, and every
//! `age_ms`/`stale` decision reads *that*, not the snapshot's own publication
//! stamp. The snapshot stamp would be a lie in two ways that both really
//! happen: [`OrchRegistry::publish_group_now`] republishes with **one** group
//! recomputed, and a group added between passes is younger than the map it
//! arrives in. [`strip_view_payload`] reports the **oldest** group's age for
//! the same reason — "nothing in this payload is older than this" is a claim
//! that survives a partial pass; an average or a snapshot stamp is not.
//!
//! # Staleness (INV-6, #1604 review N3)
//!
//! `meta.stale = age_ms > `[`VIEW_STALE_AFTER_MS`] — deliberately the same
//! 5 s as Phase 0's long-hold breadcrumb threshold, so the app has ONE
//! definition of "stuck". It is **entered on the clock and released only on
//! evidence**: nothing clears the badge but the next successful `store`, which
//! is what re-stamps `computed_at`. A timer-released badge would come down
//! while the registry was still wedged, which is the failure mode
//! `.orrerix/lessons.md` names ("release on independent evidence, not elapsed
//! time").
//!
//! `meta.partial` is reserved for Phase 2.1, where a section that hits a
//! `Busy` timeout keeps its previous value and flips it. It is always `false`
//! here, and is in the wire contract now so the frontend's staleness renderer
//! (`src/viewstale.ts`) does not have to change shape when 2.1 lands.
//!
//! # Wire identity
//!
//! Every section is produced by the **same** registry function the command it
//! replaces calls today — `group_summary`, `group_usage_live_within`,
//! `is_paused`, `notify_enabled`, `spawn_expanded`, `autonomy_state_within`,
//! `group_watches`, `workflow_status`, `merge_queue_view`, `lock_state`. The
//! payloads are therefore wire-identical by construction rather than by
//! reimplementation, and `a_published_view_carries_every_payload_the_ten_commands_return`
//! pins it. The ten commands themselves stay: `tasksview.ts` reads summary and
//! workflow status on open, and the MCP tools read their own paths.
//!
//! See `doc/design/polled-views.md` for the wire contract this file implements.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use loomux_engine::published::{Published, Stamped};

use crate::obs::LockExt;

use super::{mergeqview, GroupId, OrchRegistry, USAGE_POLL_MAX_AGE};

/// How often the publisher recomputes. Equal to `USAGE_POLL_MAX_AGE`, so a
/// payload served to the group view's 2 s poll is never staler than the memo
/// that poll reads today — this slice changes where the work happens, not how
/// fresh the answer is.
pub const VIEW_PUBLISH_INTERVAL: Duration = Duration::from_millis(1000);

/// How long a view lease stays fresh after `orch_group_view` stamps it — five
/// poll periods, so a view whose poll ticks at 2 s keeps its lease alive with
/// four ticks of slack before the tier is dropped.
pub const VIEW_LEASE_MS: u64 = 10_000;

/// How long a STRIP lease stays fresh after `orch_strip_view` stamps it.
///
/// Same window as the view lease, and for the same reason: five poll periods
/// of the surface that stamps it (the strip polls at 4 s, so this is two and a
/// half ticks of slack — tighter than the view's four, because a tab that
/// stops being bound should stop being computed promptly).
pub const STRIP_LEASE_MS: u64 = 10_000;

/// Past this age a payload is `stale` and the frontend badges it. The same
/// number as Phase 0's long-hold breadcrumb threshold, deliberately: one
/// definition of "stuck" in the app, not two.
pub const VIEW_STALE_AFTER_MS: u64 = 5_000;

/// The usage-memo window the publisher computes `usage` and `autonomy` with —
/// the same `USAGE_POLL_MAX_AGE` the two commands pass today, re-exposed so a
/// wire-identity test can make the IDENTICAL call rather than re-guessing the
/// number (a pin that re-derives its own expectation pins nothing).
pub const VIEW_USAGE_MAX_AGE: Duration = USAGE_POLL_MAX_AGE;

/// The eight sections the group view needs and the tab strip does not — the
/// expensive half (a `merge_queue.json` read, a `workflow.yml` parse, a
/// memoised `git` default-branch resolution, a resource reconcile).
///
/// A struct rather than eight `Option`s on [`GroupView`] so "all eight or none"
/// is a fact the type system holds: there is no way to assemble a half-computed
/// view tier by accident, and `view_ready` in the wire meta is exactly
/// `Option::is_some` rather than a flag someone has to remember to set.
#[derive(Debug, Clone)]
pub struct GroupViewTier {
    pub paused: bool,
    pub notify: bool,
    pub spawn_expanded: bool,
    pub autonomy: Value,
    pub watches: Value,
    pub workflow: Value,
    pub merge_queue: Value,
    pub locks: Value,
}

/// One group's published payloads, with the stamp every freshness claim about
/// them is made from.
#[derive(Debug, Clone)]
pub struct GroupView {
    /// `orch_group_summary`'s payload.
    pub summary: Value,
    /// `orch_group_usage`'s payload.
    pub usage: Value,
    /// The view tier, present only while the group holds a fresh view lease.
    pub view: Option<GroupViewTier>,
    /// The instant the pass that produced this group STARTED. Every
    /// `age_ms`/`stale` decision reads this, never the snapshot's own
    /// publication stamp — see the module doc.
    ///
    /// Pass-start rather than compute-end for two reasons: it is the clock the
    /// pass was given, so a test can inject one and reason about staleness
    /// without sleeping; and it is the conservative end of the interval, so a
    /// payload is never reported fresher than it is.
    pub computed_at: Instant,
    /// Wall-clock stamp for the payload a human reads. Never what staleness is
    /// decided from — and, unlike `computed_at`, not injectable: it is always
    /// the real clock, so under an injected `at` the two deliberately disagree.
    /// That is safe precisely because nothing decides anything from this field.
    pub computed_unix_ms: u64,
    /// What this group's pass cost, in ms.
    pub compute_ms: u32,
}

/// What one publish pass produced.
#[derive(Debug, Default)]
pub struct ViewSnapshot {
    /// `Arc` per group so a single-group republish clones POINTERS rather
    /// than ten `serde_json::Value` trees per group it is not touching.
    pub groups: HashMap<GroupId, Arc<GroupView>>,
    /// Reserved for Phase 2.1: a section that hit a `Busy` timeout kept its
    /// previous value. Always `false` in Phase 1 — there is no bounded
    /// acquisition yet to time out, so no section can be partial.
    pub partial: bool,
}

/// Owns the publish thread's state: the published cell, the view leases, and
/// the two compute counters the lease test reads.
#[derive(Debug)]
pub struct ViewPublisher {
    published: Published<ViewSnapshot>,
    /// `group -> when its view lease was last stamped`.
    ///
    /// **Released** in [`ViewPublisher::publish_pass_at`], which drops every
    /// entry whose group is no longer in the registry (INV-8a: an entity-keyed
    /// collection names its release site). Its critical section is a single map
    /// insert or a retain over a map bounded by the groups this session has
    /// opened a view on — nothing holds it across anything, so unlike a
    /// registry mutex it cannot be the lock a reader waits behind.
    leases: Mutex<HashMap<GroupId, Instant>>,
    /// `group -> when the tab strip last named it as bound`.
    ///
    /// The strip's tier cannot be derived from the registry alone: a tab can
    /// be bound to a RESTORED orchestration, which lives on disk and is not in
    /// `groups` at all. Publishing only registry-known groups dropped those
    /// tabs' badges entirely (#1625 review round 2) — a regression against the
    /// per-tab reads this replaced, which answered for any bound id.
    ///
    /// Released beside `leases` in [`ViewPublisher::publish_pass_at`]: an entry
    /// whose lease has aged out is dropped, so a closed tab stops being
    /// computed within one lease window.
    strip_leases: Mutex<HashMap<GroupId, Instant>>,
    /// Serializes the read-modify-write half of a publish — never the
    /// compute, which stays outside it. Two publishers exist (the thread's
    /// full pass and a mutating command's single-group nudge) and both did
    /// load -> modify -> store, which is a lost update with no lock.
    ///
    /// **No reader ever takes this.** `load()` goes to the `Published` cell
    /// directly, so nothing about the liveness property depends on it, and
    /// its own critical section is a map clone plus a pointer swap — no
    /// registry lock, no IO, nothing that can park.
    publish_lock: Mutex<()>,
    strip_tier_computes: AtomicU64,
    view_tier_computes: AtomicU64,
}

impl Default for ViewPublisher {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewPublisher {
    pub fn new() -> Self {
        Self {
            published: Published::new(ViewSnapshot::default()),
            leases: Mutex::new(HashMap::new()),
            strip_leases: Mutex::new(HashMap::new()),
            publish_lock: Mutex::new(()),
            strip_tier_computes: AtomicU64::new(0),
            view_tier_computes: AtomicU64::new(0),
        }
    }

    /// The current snapshot — a read-lock, a pointer clone, and release. This
    /// is the whole of what a polled command does with the registry: nothing.
    ///
    /// The name is load-bearing: `perf_dispatch.rs`'s L6 guard asserts that
    /// every command reached from a poll site has `views.load(` in its body,
    /// which is how "a polled read reads the published cell" stops being a
    /// claim in a doc comment and becomes something a test refuses to let drift.
    pub fn load(&self) -> Arc<Stamped<ViewSnapshot>> {
        self.published.load()
    }

    /// Stamp a group's view lease at `now`. Called by `orch_group_view` before
    /// it reads, so the tier the caller is about to ask for keeps being
    /// computed for as long as it keeps asking.
    pub fn note_view_lease_at(&self, group: &GroupId, now: Instant) {
        self.leases.lock_safe().insert(group.clone(), now);
    }

    /// [`ViewPublisher::note_view_lease_at`] at the current instant.
    pub fn note_view_lease(&self, group: &GroupId) {
        self.note_view_lease_at(group, Instant::now());
    }

    /// Stamp a group's STRIP lease at `now` — the tab strip naming a group it
    /// is bound to. Called by `orch_strip_view` for every id its caller sends,
    /// so the publisher covers a bound tab whether or not the registry knows
    /// its group.
    pub fn note_strip_lease_at(&self, group: &GroupId, now: Instant) {
        self.strip_leases.lock_safe().insert(group.clone(), now);
    }

    /// Group ids whose strip lease is younger than [`STRIP_LEASE_MS`] at `now`.
    fn fresh_strip_leases_at(&self, now: Instant) -> Vec<GroupId> {
        self.strip_leases
            .lock_safe()
            .iter()
            .filter(|(_, at)| {
                now.saturating_duration_since(**at).as_millis() as u64 <= STRIP_LEASE_MS
            })
            .map(|(g, _)| g.clone())
            .collect()
    }

    /// Whether `group` holds a view lease younger than [`VIEW_LEASE_MS`] at
    /// `now`.
    pub fn has_view_lease_at(&self, group: &GroupId, now: Instant) -> bool {
        self.leases
            .lock_safe()
            .get(group)
            .is_some_and(|at| now.saturating_duration_since(*at).as_millis() as u64 <= VIEW_LEASE_MS)
    }

    /// How many times a strip tier / view tier has been computed. The seam
    /// `the_view_tier_is_not_computed_for_an_unleased_group` reads: a counter
    /// rather than a timing observation, because "it was slower" is not
    /// evidence about which branch ran.
    pub fn strip_tier_computes(&self) -> u64 {
        self.strip_tier_computes.load(Ordering::Relaxed)
    }

    pub fn view_tier_computes(&self) -> u64 {
        self.view_tier_computes.load(Ordering::Relaxed)
    }

    /// One publish pass at `now`: recompute every group the registry knows and
    /// swap the result in.
    ///
    /// **No registry lock is held across a compute.** The group id list is
    /// cloned out from under the `groups` mutex and the guard released before
    /// the first payload is built — the payload builders take their own locks,
    /// and holding `groups` across them would reintroduce exactly the
    /// cross-group hold this file exists to remove (INV-5).
    pub fn publish_pass_at(&self, reg: &OrchRegistry, now: Instant) {
        // Every group the registry knows, PLUS every group a tab strip has
        // named as bound. The second half is not redundant: a tab can be bound
        // to a restored orchestration, which lives on disk and never enters
        // `groups`. Covering only the first half is what dropped those tabs'
        // badges (#1625 review round 2).
        let mut ids: Vec<GroupId> = {
            let guard = reg.groups.lock_safe();
            guard.keys().cloned().collect()
        };
        for leased in self.fresh_strip_leases_at(now) {
            if !ids.contains(&leased) {
                ids.push(leased);
            }
        }

        // Leases for groups the registry no longer knows are dropped here —
        // this is the release site the `leases` field's doc names.
        {
            let live: std::collections::HashSet<&GroupId> = ids.iter().collect();
            self.leases.lock_safe().retain(|g, _| live.contains(g));
            // Strip leases are released by AGE, not by registry membership —
            // the whole point is that a leased group need not be in `groups`.
            self.strip_leases.lock_safe().retain(|_, at| {
                now.saturating_duration_since(*at).as_millis() as u64 <= STRIP_LEASE_MS
            });
        }

        let started = Instant::now();
        let mut computed: HashMap<GroupId, Arc<GroupView>> = HashMap::with_capacity(ids.len());
        for id in ids {
            let leased = self.has_view_lease_at(&id, now);
            computed.insert(id.clone(), Arc::new(self.compute_group(reg, &id, leased, now)));
        }
        let compute_ms = elapsed_ms(started);

        // The swap, under `publish_lock` and with the compute already done.
        //
        // A pass stamps every group with its own START instant, so a nudge
        // that landed WHILE this pass was computing carries a strictly later
        // stamp — and it must win, because the pass's copy of that group was
        // read before the write the nudge is reporting. Storing this pass
        // wholesale would silently revert exactly the toggle the group view
        // is about to re-read, which is the one thing the nudge exists to
        // prevent. Keeping the later stamp is monotone: the NEXT pass, which
        // starts after the write, replaces it with genuinely fresher data.
        let _swap = self.publish_lock.lock_safe();
        let previous = self.published.load();
        let mut groups: HashMap<GroupId, Arc<GroupView>> = HashMap::with_capacity(computed.len());
        for (id, fresh) in computed {
            match previous.value.groups.get(&id) {
                Some(prev) if prev.computed_at > fresh.computed_at => {
                    groups.insert(id, Arc::clone(prev));
                }
                _ => {
                    groups.insert(id, fresh);
                }
            }
        }
        self.published.store(ViewSnapshot { groups, partial: false }, compute_ms);
    }

    /// [`ViewPublisher::publish_pass_at`] at the current instant.
    pub fn publish_pass(&self, reg: &OrchRegistry) {
        self.publish_pass_at(reg, Instant::now());
    }

    /// Recompute exactly ONE group and republish, keeping every other group's
    /// entry (and its own stamp) untouched.
    ///
    /// This is the write-side nudge: a mutating command calls it after its own
    /// write lands, on its own pool thread, so the view's follow-up `load()`
    /// cannot read the pre-toggle state. It is `usage_memo`'s "invalidate where
    /// being late is wrong" rule applied to writes rather than to a memo — the
    /// group view re-reads immediately after every toggle, and a toggle that
    /// visibly snaps back for a tick is a bug report, not a stale panel.
    ///
    /// The group keeps whatever tier it is entitled to: nudging does not grant
    /// a view lease, so a mutation from the MCP side on an unviewed group stays
    /// a strip-tier recompute.
    pub fn publish_group_at(&self, reg: &OrchRegistry, group: &GroupId, now: Instant) {
        // A well-formed id the registry does not know produces no entry
        // (#1625 review N4). `publish_pass_at` only ever inserts registry-known
        // ids, so without this a nudge after a refused write on an unknown
        // group would add a phantom that `orch_strip_view` lists and whose
        // stamp its oldest-age `min_by_key` weighs, until the next full pass
        // dropped it. Self-healing within a tick, but it is a group the app
        // does not have, so it should never be published at all.
        // ...unless a tab strip has named it: a restored orchestration is not
        // in `groups` and is still a group the UI is entitled to see.
        if reg.group(group.as_str()).is_none()
            && !self
                .strip_leases
                .lock_safe()
                .get(group)
                .is_some_and(|at| {
                    now.saturating_duration_since(*at).as_millis() as u64 <= STRIP_LEASE_MS
                })
        {
            return;
        }
        let leased = self.has_view_lease_at(group, now);
        let started = Instant::now();
        let view = Arc::new(self.compute_group(reg, group, leased, now));
        let compute_ms = elapsed_ms(started);

        // Under the same lock as a full pass, taken AFTER the compute, and
        // obeying THE SAME later-stamp-wins rule.
        //
        // Inserting unconditionally here was a real lost update, not a
        // cosmetic asymmetry (#1625 review B2). Two nudges overlap whenever a
        // human clicks twice: the first may be computing a leased group's
        // view tier — a `merge_queue.json` read, a `workflow.yml` parse and a
        // cold default-branch resolution, which `orch_workflow_status`'s own
        // doc puts at 2-4 blocking `git` spawns — while the second's compute
        // is warm and finishes first. The slow one then landed last and
        // published sections it had read BEFORE the second write, reverting
        // the toggle the human just clicked and dragging `computed_at`
        // backwards with it. That is precisely what this nudge exists to
        // prevent the group view from re-reading.
        //
        // Keeping a later entry is strictly safe: a nudge runs only AFTER its
        // own write has returned, so any entry stamped later than this one was
        // computed after that write landed and already contains it.
        let _swap = self.publish_lock.lock_safe();
        let previous = self.published.load();
        if previous
            .value
            .groups
            .get(group)
            .is_some_and(|prev| prev.computed_at > view.computed_at)
        {
            // Nothing to publish, and deliberately no store: an identical
            // republish would bump `seq`, and a reader comparing `seq` would
            // read that as a new publication when nothing moved.
            return;
        }
        // Shallow: every other group is an `Arc` clone, not a payload copy.
        let mut groups: HashMap<GroupId, Arc<GroupView>> = previous.value.groups.clone();
        groups.insert(group.clone(), view);
        self.published.store(ViewSnapshot { groups, partial: previous.value.partial }, compute_ms);
    }

    /// [`ViewPublisher::publish_group_at`] at the current instant — the call
    /// every mutating command appends after its own write returns, on its own
    /// pool thread, so no registry guard from that write is still held when the
    /// recompute takes its own locks.
    pub fn publish_group_now(&self, reg: &OrchRegistry, group: &GroupId) {
        self.publish_group_at(reg, group, Instant::now());
    }

    /// Compute one group's payloads, view tier included iff `leased`.
    ///
    /// Every section is the SAME registry call its command makes today — the
    /// wire-identity property, held by construction rather than by a second
    /// implementation that has to be kept in step.
    fn compute_group(
        &self,
        reg: &OrchRegistry,
        group: &GroupId,
        leased: bool,
        at: Instant,
    ) -> GroupView {
        let started = Instant::now();
        let summary = reg.group_summary(group);
        let usage = reg.group_usage_live_within(group, VIEW_USAGE_MAX_AGE);
        self.strip_tier_computes.fetch_add(1, Ordering::Relaxed);

        let view = if leased {
            self.view_tier_computes.fetch_add(1, Ordering::Relaxed);
            Some(GroupViewTier {
                paused: reg.is_paused(group),
                notify: reg.notify_enabled(group),
                spawn_expanded: reg.spawn_expanded(group),
                autonomy: reg.autonomy_state_within(group, VIEW_USAGE_MAX_AGE),
                watches: reg.group_watches(group),
                workflow: reg.workflow_status(group),
                merge_queue: mergeqview::merge_queue_view(&reg.group_dir(group)),
                locks: reg.lock_state(group),
            })
        } else {
            None
        };

        GroupView {
            summary,
            usage,
            view,
            computed_at: at,
            computed_unix_ms: super::now_ms(),
            compute_ms: elapsed_ms(started),
        }
    }
}

/// The `meta` block every published payload carries.
///
/// `view_ready` is present only on [`group_view_payload`]: the strip payload
/// has no view tier, so the question is a category error there rather than a
/// `true` worth writing down.
fn meta(
    seq: u64,
    computed_at: Instant,
    computed_unix_ms: u64,
    compute_ms: u32,
    partial: bool,
    now: Instant,
) -> Value {
    let age_ms = now.saturating_duration_since(computed_at).as_millis() as u64;
    json!({
        "seq": seq,
        "published_at_ms": computed_unix_ms,
        "age_ms": age_ms,
        "compute_ms": compute_ms,
        // Entered on the clock; released ONLY by the next successful store,
        // which is what moves `computed_at`. See the module doc.
        "stale": age_ms > VIEW_STALE_AFTER_MS,
        "partial": partial,
    })
}

/// `orch_group_view`'s payload, assembled from a snapshot the caller already
/// holds. Pure over `(snapshot, group, now)` — it takes no lock at all, which
/// is what makes the command it serves unable to park.
///
/// A group the snapshot does not carry answers `Value::Null`, the same
/// no-error-channel degrade the ten commands it replaces use (`command_group`'s
/// doc): a refused id, and a group created since the last pass, are one case to
/// the caller — keep the previous render, ask again shortly.
pub fn group_view_payload(
    snapshot: &Stamped<ViewSnapshot>,
    group: &GroupId,
    now: Instant,
) -> Value {
    let Some(g) = snapshot.value.groups.get(group) else { return Value::Null };
    let mut meta = meta(
        snapshot.seq,
        g.computed_at,
        g.computed_unix_ms,
        g.compute_ms,
        snapshot.value.partial,
        now,
    );
    if let Some(obj) = meta.as_object_mut() {
        obj.insert("view_ready".into(), Value::Bool(g.view.is_some()));
    }

    match &g.view {
        Some(v) => json!({
            "meta": meta,
            "summary": g.summary,
            "usage": g.usage,
            "paused": v.paused,
            "notify": v.notify,
            "spawn_expanded": v.spawn_expanded,
            "autonomy": v.autonomy,
            "watches": v.watches,
            "workflow": v.workflow,
            "merge_queue": v.merge_queue,
            "locks": v.locks,
        }),
        // The lease has not been picked up yet (a first open, or a reopen after
        // the tier was dropped). The two strip sections are real and go out;
        // the eight are honestly absent rather than defaulted, because a
        // fabricated `paused: false` is a wrong answer rendered as a right one.
        None => json!({
            "meta": meta,
            "summary": g.summary,
            "usage": g.usage,
            "paused": Value::Null,
            "notify": Value::Null,
            "spawn_expanded": Value::Null,
            "autonomy": Value::Null,
            "watches": Value::Null,
            "workflow": Value::Null,
            "merge_queue": Value::Null,
            "locks": Value::Null,
        }),
    }
}

/// `orch_strip_view`'s payload: one entry per group, each carrying the two
/// sections the tab strip renders. Pure, like [`group_view_payload`].
///
/// The `meta` reports the **oldest** group's age, so `age_ms`/`stale` mean
/// "nothing in this payload is older than this". An average or the snapshot's
/// own stamp would each report a payload as fresher than its worst member, and
/// the tab strip's whole job is to be right about the tab that is in trouble.
/// An empty map reports the snapshot's own publication stamp — there is no
/// group to be stale about, and reporting age 0 forever would make an app with
/// no groups indistinguishable from a wedged one.
pub fn strip_view_payload(snapshot: &Stamped<ViewSnapshot>, now: Instant) -> Value {
    let oldest = snapshot.value.groups.values().min_by_key(|g| g.computed_at);
    let (computed_at, computed_unix_ms, compute_ms) = match oldest {
        Some(g) => (g.computed_at, g.computed_unix_ms, g.compute_ms),
        None => (snapshot.published_at, snapshot.published_unix_ms, snapshot.compute_ms),
    };

    let mut groups = serde_json::Map::new();
    for (id, g) in &snapshot.value.groups {
        groups.insert(
            id.as_str().to_string(),
            json!({ "summary": g.summary, "usage": g.usage }),
        );
    }

    json!({
        "meta": meta(
            snapshot.seq,
            computed_at,
            computed_unix_ms,
            compute_ms,
            snapshot.value.partial,
            now,
        ),
        "groups": Value::Object(groups),
    })
}

/// Elapsed ms since `started`, saturating at `u32::MAX` (~49 days).
fn elapsed_ms(started: Instant) -> u32 {
    started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
}

impl OrchRegistry {
    /// Recompute and republish ONE group's view immediately — the write-side
    /// nudge appended to every mutation the group view re-reads after.
    ///
    /// Called from the mutating `#[tauri::command]`'s own blocking body, AFTER
    /// the write returns, so no registry guard from that write is still held
    /// when the recompute takes its own locks.
    ///
    /// **Unconditional, including after a REJECTED write.** A refusal (an
    /// out-of-range cap, a toggle that needs autonomous on) is exactly when the
    /// panel most needs to re-sync to the state that really holds, and the
    /// group view reloads after an error too. Republishing after a failure
    /// costs one recompute and cannot write anything.
    pub fn publish_group_now(&self, group: &GroupId) {
        self.views.publish_group_now(self, group);
    }
}
