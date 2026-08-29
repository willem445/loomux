//! The snapshot publisher's contract (#1608, plan #1600 §3 Phase 1).
//!
//! Must be an integration test, not a unit test (CLAUDE.md constraint 4 — the
//! Windows test exe needs build.rs's comctl32-v6 manifest). Everything here
//! drives the SHIPPED `ViewPublisher` against a real `OrchRegistry` with no
//! Tauri `AppHandle` (unavailable headless) and no real agent CLI (constraint
//! 3).
//!
//! The liveness half of Phase 1 — that a published read still returns while
//! every tracked registry lock is held — lives in `tests/liveness.rs` (L1),
//! because it needs Phase 0's `hold_lock_for_test` seam. This file pins what
//! the publisher *produces*: wire identity with the ten commands it replaces,
//! the two tiers, the write-side nudge, and the staleness rule.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use loomux_lib::orchestration::views::{group_view_payload, strip_view_payload, ViewPublisher};
use loomux_lib::orchestration::views::VIEW_USAGE_MAX_AGE;
use loomux_lib::orchestration::{mergeqview, GroupId, Guardrails, OrchRegistry, Role};

fn rails() -> Guardrails {
    Guardrails {
        max_agents: 4,
        agent_cli: "claude".into(),
        auto_ops: false,
        advanced_orchestrator: false,
        idle_kill_minutes: 0,
        max_spawns_per_hour: 0,
        watchdog_stall_minutes: 0,
        ..Guardrails::default()
    }
}

/// #464: every registry construction here goes through this, so no spawn can
/// write a generated custom-agent file into the developer's REAL
/// `~/.claude/agents` or `~/.copilot/agents`.
fn test_registry() -> (Arc<OrchRegistry>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let reg = OrchRegistry::new(dir.path().to_path_buf());
    reg.set_port(45997);
    reg.set_claude_agents_dir_override(dir.path().join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.path().join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.path().join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.path().join("copilot-hooks"));
    (Arc::new(reg), dir)
}

/// Wall-clock-derived keys, which two calls a millisecond apart legitimately
/// disagree on. Each row is a **path suffix**, blanked in BOTH sides before the
/// byte comparison below, so the comparison still sees every key, every array
/// length and every other value.
///
/// The rows are argued, not convenient:
/// - `uptime_ms` — `group_summary` computes it as `now - started_ms` per agent.
/// - `now_ms` — `lock_state` stamps the instant it answered, so the frontend
///   can age holds and queue entries against one clock.
///
/// `blank_clock_keys` counts what it blanked and
/// [`a_published_view_carries_every_payload_the_ten_commands_return`] asserts
/// every row was actually hit, so a row that stops occurring (a rename, a
/// dropped field) fails loudly instead of quietly widening the exemption.
const CLOCK_KEYS: &[&str] = &["uptime_ms", "now_ms"];

/// Replace every value under a [`CLOCK_KEYS`] key with `null`, in place,
/// recording how many times each key was hit.
fn blank_clock_keys(v: &mut Value, hits: &mut BTreeMap<&'static str, usize>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if let Some(key) = CLOCK_KEYS.iter().find(|c| **c == k.as_str()) {
                    *hits.entry(key).or_insert(0) += 1;
                    *val = Value::Null;
                } else {
                    blank_clock_keys(val, hits);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                blank_clock_keys(item, hits);
            }
        }
        _ => {}
    }
}

/// Byte-compare two payloads after blanking the clock keys, reporting the first
/// differing path rather than two blobs.
fn assert_same_payload(section: &str, published: &Value, direct: &Value, hits: &mut BTreeMap<&'static str, usize>) {
    let mut a = published.clone();
    let mut b = direct.clone();
    blank_clock_keys(&mut a, hits);
    blank_clock_keys(&mut b, &mut BTreeMap::new());
    if a != b {
        panic!(
            "section `{section}` is NOT what the command it replaces returns.\n  \
             published: {}\n  direct:    {}\n\
             The publisher must call the SAME registry function the command calls, so the wire \
             shape cannot drift; if this differs only on a clock-derived key, that key belongs in \
             CLOCK_KEYS with its own argued row.",
            serde_json::to_string(&a).unwrap_or_default(),
            serde_json::to_string(&b).unwrap_or_default(),
        );
    }
}

fn section<'a>(payload: &'a Value, key: &str) -> &'a Value {
    payload.get(key).unwrap_or_else(|| panic!("payload has no `{key}` section: {payload}"))
}

// ---------- wire identity ----------

#[test]
fn a_published_view_carries_every_payload_the_ten_commands_return() {
    let (reg, d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let w = reg.spawn_agent(&g.id, Role::Worker, "w", "t", false, None).unwrap();
    reg.set_pty_for_test(&w.id, 74401);

    let views = ViewPublisher::new();
    let now = Instant::now();
    views.note_view_lease_at(&g.id, now);
    views.publish_pass_at(&reg, now);

    let snap = views.load();
    let payload = group_view_payload(&snap, &g.id, now);
    assert!(payload.is_object(), "a leased, published group must answer an object, got {payload}");
    assert_eq!(
        section(&payload, "meta").get("view_ready"),
        Some(&Value::Bool(true)),
        "the group holds a fresh lease and a pass has run, so the view tier must be present"
    );

    let mut hits: BTreeMap<&'static str, usize> = BTreeMap::new();

    // The ten sections, each against the registry function its command calls.
    assert_same_payload("summary", section(&payload, "summary"), &reg.group_summary(&g.id), &mut hits);
    assert_same_payload(
        "usage",
        section(&payload, "usage"),
        &reg.group_usage_live_within(&g.id, VIEW_USAGE_MAX_AGE),
        &mut hits,
    );
    assert_same_payload("paused", section(&payload, "paused"), &Value::Bool(reg.is_paused(&g.id)), &mut hits);
    assert_same_payload("notify", section(&payload, "notify"), &Value::Bool(reg.notify_enabled(&g.id)), &mut hits);
    assert_same_payload(
        "spawn_expanded",
        section(&payload, "spawn_expanded"),
        &Value::Bool(reg.spawn_expanded(&g.id)),
        &mut hits,
    );
    assert_same_payload(
        "autonomy",
        section(&payload, "autonomy"),
        &reg.autonomy_state_within(&g.id, VIEW_USAGE_MAX_AGE),
        &mut hits,
    );
    assert_same_payload("watches", section(&payload, "watches"), &reg.group_watches(&g.id), &mut hits);
    assert_same_payload("workflow", section(&payload, "workflow"), &reg.workflow_status(&g.id), &mut hits);
    assert_same_payload(
        "merge_queue",
        section(&payload, "merge_queue"),
        // The group dir derived independently of the registry (`group_dir_at`
        // is `root.join(id)`), so this side of the comparison does not run
        // through the same private helper the publisher does.
        &mergeqview::merge_queue_view(&d.path().join(g.id.as_str())),
        &mut hits,
    );
    assert_same_payload("locks", section(&payload, "locks"), &reg.lock_state(&g.id), &mut hits);

    // Vacuity control: an all-null payload would satisfy every comparison
    // above if the direct calls were also null. The one agent this fixture
    // spawns is what makes `summary` non-trivial.
    assert_eq!(
        section(&payload, "summary").get("live_agents"),
        Some(&Value::from(1)),
        "the fixture must have a live agent, or the summary comparison is between two empty \
         payloads and pins nothing"
    );

    // Population control on the exemption list: a CLOCK_KEYS row that no
    // longer occurs is a row watching nothing, and would silently widen what
    // the comparison forgives.
    for key in CLOCK_KEYS {
        assert!(
            hits.get(key).copied().unwrap_or(0) > 0,
            "CLOCK_KEYS row `{key}` was never found in any published section — the field was \
             renamed or removed, so this row now exempts nothing and must be deleted or \
             re-pointed. Hits: {hits:?}"
        );
    }
}

// ---------- the two tiers ----------

#[test]
fn the_view_tier_is_not_computed_for_an_unleased_group() {
    let (reg, _d) = test_registry();
    let leased = reg.create_group("C:/tmp/leased", rails()).unwrap();
    let quiet = reg.create_group("C:/tmp/quiet", rails()).unwrap();

    let views = ViewPublisher::new();
    let now = Instant::now();
    views.note_view_lease_at(&leased.id, now);
    views.publish_pass_at(&reg, now);

    assert_eq!(
        views.strip_tier_computes(),
        2,
        "the strip tier is computed for EVERY group — that is what the tab strip polls"
    );
    assert_eq!(
        views.view_tier_computes(),
        1,
        "the view tier must be computed for the leased group and NOT for the other one: without \
         the lease, one open group view puts a merge_queue.json read, a workflow.yml parse and a \
         git default-branch resolution on every group in the app, every second"
    );

    let snap = views.load();
    let now_read = now;
    assert_eq!(
        section(&group_view_payload(&snap, &leased.id, now_read), "meta").get("view_ready"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        section(&group_view_payload(&snap, &quiet.id, now_read), "meta").get("view_ready"),
        Some(&Value::Bool(false)),
        "an unleased group answers view_ready:false — honestly absent, never a fabricated default"
    );
    // ...and the strip tier is there for BOTH, which is what makes the tab
    // strip's fan-out collapse to one call.
    let strip = strip_view_payload(&snap, now_read);
    let groups = section(&strip, "groups");
    assert!(groups.get(leased.id.as_str()).is_some(), "strip: {strip}");
    assert!(groups.get(quiet.id.as_str()).is_some(), "strip: {strip}");
}

#[test]
fn a_lapsed_lease_drops_the_view_tier_rather_than_carrying_it_forward() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();

    let views = ViewPublisher::new();
    let t0 = Instant::now();
    views.note_view_lease_at(&g.id, t0);
    views.publish_pass_at(&reg, t0);
    assert_eq!(
        section(&group_view_payload(&views.load(), &g.id, t0), "meta").get("view_ready"),
        Some(&Value::Bool(true))
    );

    // One millisecond past the lease. Carrying the tier forward instead would
    // let a panel reopened ten minutes later render ten-minute-old toggles
    // under a one-second `age_ms` — the false freshness this slice removes.
    let lapsed = t0 + Duration::from_millis(VIEW_LEASE_MS_FOR_TEST + 1);
    views.publish_pass_at(&reg, lapsed);
    assert_eq!(
        section(&group_view_payload(&views.load(), &g.id, lapsed), "meta").get("view_ready"),
        Some(&Value::Bool(false)),
        "a lapsed lease must DROP the view tier, not carry a stale one under a fresh stamp"
    );
    // The strip tier is unaffected: the tab strip polls every group forever.
    assert!(section(&group_view_payload(&views.load(), &g.id, lapsed), "summary").is_object());
}

// ---------- restored (disk-only) groups: the strip lease ----------

/// A group that exists on DISK but not in the registry — a restored
/// orchestration the human has not resumed. `list_recorded` finds these by
/// reading the root directory; nothing puts them in `groups`.
///
/// Built the way the app leaves one behind: a group dir with a `usage.json`
/// carrying accrued cost. That file is the whole point — it is what the tab
/// strip's badge shows for a group with no live agents (#194 P4 LOW-8).
fn restored_group_on_disk(dir: &std::path::Path, id: &str, cost: f64) -> GroupId {
    let g = GroupId::parse(id).expect("well-formed id");
    let gdir = dir.join(id);
    std::fs::create_dir_all(&gdir).expect("group dir");
    std::fs::write(
        gdir.join("usage.json"),
        serde_json::to_vec(&serde_json::json!({
            "agents": [{
                "agent_id": "w-1", "name": "w", "role": "worker",
                "cost_usd": cost, "tokens": 1234, "cost_source": "estimated"
            }]
        }))
        .expect("usage json"),
    )
    .expect("write usage.json");
    g
}

#[test]
fn a_restored_group_a_tab_is_bound_to_is_published_wire_identically() {
    // THE REGRESSION #1625 review round 2 found, pinned. The publisher covered
    // `reg.groups` only, so a tab bound to a restored orchestration got no
    // entry and lost its accrued-cost badge — a badge the per-tab reads this
    // replaced always produced, because they answered for ANY bound id.
    let (reg, d) = test_registry();
    let restored = restored_group_on_disk(d.path(), "restored-0001", 4.25);
    assert!(
        reg.group(restored.as_str()).is_none(),
        "setup: the fixture must NOT be in the registry, or this test is about a live group"
    );

    let views = ViewPublisher::new();
    let now = Instant::now();
    views.note_strip_lease_at(&restored, now);
    views.publish_pass_at(&reg, now);

    let snap = views.load();
    let payload = group_view_payload(&snap, &restored, now);
    assert!(
        payload.is_object(),
        "a strip-leased restored group must be published, not absent: {payload}"
    );

    // WIRE IDENTITY against the commands this replaced, for the two sections
    // the strip renders. Same rule as the ten-command test: the publisher must
    // call the SAME registry function, so a restored group's entry cannot
    // drift from what the per-tab reads returned for it.
    let mut hits: BTreeMap<&'static str, usize> = BTreeMap::new();
    assert_same_payload(
        "summary",
        section(&payload, "summary"),
        &reg.group_summary(&restored),
        &mut hits,
    );
    assert_same_payload(
        "usage",
        section(&payload, "usage"),
        &reg.group_usage_live_within(&restored, VIEW_USAGE_MAX_AGE),
        &mut hits,
    );

    // ...and the badge's actual number survived the trip. Without this the two
    // comparisons above hold just as well between two empty payloads, which is
    // precisely the state the regression produced.
    let usage = section(&payload, "usage");
    let cost = usage
        .get("lifetime_cost_usd")
        .or_else(|| usage.get("live_cost_usd"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    assert!(
        cost > 0.0,
        "the restored group's accrued cost did not reach the payload — the badge #194 P4 \
         LOW-8 exists for would render empty. usage: {usage}"
    );
}

#[test]
fn a_restored_group_no_tab_is_bound_to_is_not_computed() {
    // The bound half of the lease. Publishing every on-disk group would grow
    // without limit on a long-lived machine, which is why the strip names what
    // it is bound to rather than the publisher scanning the root.
    let (reg, d) = test_registry();
    let leased = restored_group_on_disk(d.path(), "restored-0002", 1.5);
    let unleased = restored_group_on_disk(d.path(), "restored-0003", 9.0);

    let views = ViewPublisher::new();
    let now = Instant::now();
    views.note_strip_lease_at(&leased, now);
    views.publish_pass_at(&reg, now);

    let snap = views.load();
    assert!(
        group_view_payload(&snap, &leased, now).is_object(),
        "the leased restored group must be published"
    );
    assert_eq!(
        group_view_payload(&snap, &unleased, now),
        Value::Null,
        "a restored group NO tab is bound to must not be computed — otherwise every group \
         ever recorded on this machine is published every second"
    );
}

#[test]
fn a_lapsed_strip_lease_stops_a_restored_group_being_computed() {
    // Released by AGE, not by registry membership — a restored group is never
    // in `groups`, so membership could not release it and the map would grow
    // for the life of the process.
    let (reg, d) = test_registry();
    let g = restored_group_on_disk(d.path(), "restored-0004", 2.0);

    let views = ViewPublisher::new();
    let t0 = Instant::now();
    views.note_strip_lease_at(&g, t0);
    views.publish_pass_at(&reg, t0);
    assert!(group_view_payload(&views.load(), &g, t0).is_object(), "setup: published while leased");

    let lapsed = t0 + Duration::from_millis(STRIP_LEASE_MS_FOR_TEST + 1);
    views.publish_pass_at(&reg, lapsed);
    assert_eq!(
        group_view_payload(&views.load(), &g, lapsed),
        Value::Null,
        "a lapsed strip lease must stop the group being computed, or a tab closed hours ago \
         is still costing a usage.json read every second"
    );
}

// ---------- the write-side nudge ----------

#[test]
fn a_toggle_is_visible_in_the_next_view_read() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();

    let views = ViewPublisher::new();
    let now = Instant::now();
    views.note_view_lease_at(&g.id, now);
    views.publish_pass_at(&reg, now);
    assert_eq!(
        section(&group_view_payload(&views.load(), &g.id, now), "notify"),
        &Value::Bool(false),
        "setup: notifications start off"
    );

    reg.set_notify(&g.id, true).expect("set_notify");

    // The negative half, and the reason the nudge exists: the WRITE alone does
    // not move the published view. Without `publish_group_now` the group view's
    // own post-toggle reload — which is immediate, not on the next tick —
    // renders the toggle snapping back for up to a full publish interval.
    assert_eq!(
        section(&group_view_payload(&views.load(), &g.id, now), "notify"),
        &Value::Bool(false),
        "the published snapshot is only moved by a publish; if this already reads true the test \
         below proves nothing about the nudge"
    );

    views.publish_group_now(&reg, &g.id);
    assert_eq!(
        section(&group_view_payload(&views.load(), &g.id, now), "notify"),
        &Value::Bool(true),
        "the toggle must be visible in the very next view read, not one tick later"
    );
}

#[test]
fn a_nudge_recomputes_one_group_and_leaves_every_other_stamp_alone() {
    let (reg, _d) = test_registry();
    let a = reg.create_group("C:/tmp/a", rails()).unwrap();
    let b = reg.create_group("C:/tmp/b", rails()).unwrap();

    let views = ViewPublisher::new();
    let t0 = Instant::now();
    views.publish_pass_at(&reg, t0);
    let before = views.load();
    let b_stamp = before.value.groups.get(&b.id).expect("b published").computed_at;

    let later = t0 + Duration::from_secs(3);
    views.publish_group_at(&reg, &a.id, later);
    let after = views.load();

    assert_eq!(
        after.value.groups.get(&b.id).expect("b survives a nudge for a").computed_at,
        b_stamp,
        "a nudge for one group must keep every other group's own stamp — a snapshot-wide stamp \
         would report B as freshly computed when only A was"
    );
    assert_eq!(
        after.value.groups.get(&a.id).expect("a republished").computed_at,
        later,
        "the nudged group is the one that gets the new stamp"
    );
    assert!(after.seq > before.seq, "a nudge publishes");
}

#[test]
fn a_nudge_is_not_lost_to_a_pass_that_was_already_computing() {
    // The lost update the two publishers had before `publish_lock` and the
    // later-stamp-wins merge. A full pass stamps every group with the instant
    // it STARTED, so a nudge that lands while that pass is still computing
    // carries a strictly later stamp — and storing the pass wholesale would
    // revert exactly the toggle the group view is about to re-read, which is
    // the one thing the nudge exists to prevent.
    //
    // Injected rather than raced: the two publishes run in the order a real
    // interleaving produces (the nudge's write lands first, the pass's older
    // snapshot arrives after it), with the stamps that interleaving gives
    // them. A sleep-and-hope race would be flaky in exactly the direction
    // that reads as a pass.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();

    let views = ViewPublisher::new();
    let pass_started = Instant::now();
    views.note_view_lease_at(&g.id, pass_started);
    views.publish_pass_at(&reg, pass_started);

    // The human toggles; the mutating command nudges. Its stamp is LATER than
    // the pass that is (in the real interleaving) still computing.
    reg.set_notify(&g.id, true).expect("set_notify");
    let nudged_at = pass_started + Duration::from_millis(40);
    views.publish_group_at(&reg, &g.id, nudged_at);
    assert_eq!(
        section(&group_view_payload(&views.load(), &g.id, nudged_at), "notify"),
        &Value::Bool(true),
        "setup: the nudge must be visible before the late pass arrives, or this test is \
         about nothing"
    );

    // Now the pass that began BEFORE the write finally publishes.
    //
    // **The assertion is on the STAMP, not on `notify`.** A test asserting
    // `notify == true` here would be a decoration: `publish_pass_at`
    // recomputes each group from the live registry, and `set_notify(true)`
    // has already landed, so it reads `true` under the merge rule AND under a
    // wholesale store. Nothing about the payload can separate the two
    // implementations, because this harness cannot make a pass carry a value
    // it read before the write.
    //
    // `computed_at` can. Under the merge rule the entry keeps the nudge's
    // stamp; a wholesale store replaces it with the pass's older one — which
    // is the lost update, and is also what would then make the payload wrong
    // in production, where the pass really is holding a pre-write copy.
    views.publish_pass_at(&reg, pass_started);
    assert_eq!(
        views.load().value.groups.get(&g.id).expect("still published").computed_at,
        nudged_at,
        "a pass that was already computing when the nudge landed must NOT overwrite it: its \
         copy of this group was read before the write, and the group view re-reads \
         immediately after the toggle rather than on the next tick"
    );
    assert_ne!(
        views.load().value.groups.get(&g.id).expect("still published").computed_at,
        pass_started,
        "the two candidate outcomes must DIVERGE, or the assertion above holds under either \
         implementation"
    );

    // ...and the pass is not simply ignored: a LATER pass replaces the nudge
    // with genuinely fresher data, so the merge is monotone rather than a
    // latch that would pin this group at its nudged stamp forever.
    let later_pass = nudged_at + Duration::from_secs(1);
    views.publish_pass_at(&reg, later_pass);
    assert_eq!(
        views.load().value.groups.get(&g.id).expect("republished").computed_at,
        later_pass,
        "a pass that started AFTER the write must win — keeping the later stamp is a merge \
         rule, not a latch"
    );
}

#[test]
fn a_slow_nudge_does_not_overwrite_a_faster_one_that_landed_first() {
    // The OTHER swap site. `publish_pass_at` merged by stamp from the start;
    // `publish_group_at` inserted unconditionally, so the same lost update
    // was still live nudge -> nudge — the direction that reverts a toggle a
    // human just clicked (#1625 review B2).
    //
    // Two nudges overlap whenever a human clicks twice. The first can be
    // computing a leased group's view tier (a merge_queue.json read, a
    // workflow.yml parse, a cold default-branch resolution) while the
    // second's compute is warm and lands first. Injected rather than raced:
    // the publishes run in the order the real interleaving produces, with
    // the stamps that interleaving gives them.
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();

    let views = ViewPublisher::new();
    let slow_started = Instant::now();
    views.note_view_lease_at(&g.id, slow_started);
    views.publish_pass_at(&reg, slow_started);

    // The SECOND click's nudge: started later, computed faster, lands first.
    let fast = slow_started + Duration::from_millis(50);
    views.publish_group_at(&reg, &g.id, fast);
    assert_eq!(
        views.load().value.groups.get(&g.id).expect("published").computed_at,
        fast,
        "setup: the faster nudge must be the published one before the slow one lands, or \
         this test is about nothing"
    );

    // The FIRST click's nudge finally lands, carrying sections it read before
    // the second write. It must NOT overwrite.
    views.publish_group_at(&reg, &g.id, slow_started);
    assert_eq!(
        views.load().value.groups.get(&g.id).expect("published").computed_at,
        fast,
        "a slow nudge landing after a faster one must not overwrite it: its sections were \
         read before the second write, so publishing them reverts the toggle the human just \
         clicked and drags computed_at backwards"
    );
    assert_ne!(
        views.load().value.groups.get(&g.id).expect("published").computed_at,
        slow_started,
        "the two candidate outcomes must DIVERGE, or the assertion above holds under either \
         implementation"
    );

    // ...and it is a merge rule, not a latch: a LATER nudge still wins.
    let later = fast + Duration::from_secs(1);
    views.publish_group_at(&reg, &g.id, later);
    assert_eq!(
        views.load().value.groups.get(&g.id).expect("published").computed_at,
        later,
        "a nudge stamped after the published one must win — otherwise the first click would \
         pin this group forever"
    );
}

#[test]
fn a_nudge_for_a_group_the_registry_does_not_know_publishes_nothing() {
    // #1625 review N4. `publish_group_now` is deliberately unconditional
    // after a REJECTED write, which is right — a refusal is when the panel
    // most needs to re-sync. But a well-formed id the registry never had must
    // not become an entry the tab strip lists and ages.
    let (reg, _d) = test_registry();
    let known = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let views = ViewPublisher::new();
    let now = Instant::now();
    views.publish_pass_at(&reg, now);
    let before = views.load().value.groups.len();

    let ghost = loomux_engine::groupid::GroupId::parse("never-created").expect("well-formed id");
    views.publish_group_at(&reg, &ghost, now);
    assert_eq!(
        views.load().value.groups.len(),
        before,
        "a nudge for an unknown group must add no entry"
    );
    assert_eq!(
        group_view_payload(&views.load(), &ghost, now),
        Value::Null,
        "and it must still read as absent"
    );
    // Non-vacuity: the publisher is working at all.
    assert!(
        group_view_payload(&views.load(), &known.id, now).is_object(),
        "the known group must still be published, or the assertions above pass because \
         nothing is published at all"
    );
}

// ---------- staleness (INV-6, #1604 review N3) ----------

#[test]
fn stale_is_entered_on_the_clock_and_released_only_by_a_publish() {
    let (reg, _d) = test_registry();
    let g = reg.create_group("C:/tmp/repo", rails()).unwrap();

    let views = ViewPublisher::new();
    let t0 = Instant::now();
    views.note_view_lease_at(&g.id, t0);
    views.publish_pass_at(&reg, t0);
    let snap = views.load();

    let stale_at = |now: Instant| -> bool {
        section(&group_view_payload(&snap, &g.id, now), "meta")
            .get("stale")
            .and_then(Value::as_bool)
            .expect("meta.stale is a bool")
    };
    let age_at = |now: Instant| -> u64 {
        section(&group_view_payload(&snap, &g.id, now), "meta")
            .get("age_ms")
            .and_then(Value::as_u64)
            .expect("meta.age_ms is a number")
    };

    // The threshold is a `>`: exactly at it is NOT stale. The edge is pinned
    // because a badge that appears one millisecond early on every healthy tick
    // is a badge nobody believes.
    //
    // `age_at(t0)` is not asserted to be 0: `computed_at` is stamped when the
    // group's own compute FINISHED, which is a hair after `t0`. Saturating
    // subtraction makes that 0 rather than negative, and the assertions below
    // are all bracketed well clear of it.
    assert!(!stale_at(t0), "a payload just published is not stale");
    assert!(!stale_at(t0 + Duration::from_millis(VIEW_STALE_AFTER_MS_FOR_TEST)), "at the threshold, not past it");
    let past = t0 + Duration::from_millis(VIEW_STALE_AFTER_MS_FOR_TEST + 500);
    assert!(stale_at(past), "past the threshold the payload is stale");
    assert!(age_at(past) >= VIEW_STALE_AFTER_MS_FOR_TEST, "age is reported, not just the flag");

    // RELEASED ONLY ON EVIDENCE. Nothing about waiting longer clears it — only
    // a publish that actually re-stamps `computed_at` does. This is the
    // difference between "the registry recovered" and "the timer expired",
    // and lessons.md says only the first may take a badge down.
    assert!(stale_at(past + Duration::from_secs(60)), "waiting longer never clears stale");
    views.publish_pass_at(&reg, past);
    let fresh = views.load();
    assert!(
        !section(&group_view_payload(&fresh, &g.id, past), "meta")
            .get("stale")
            .and_then(Value::as_bool)
            .expect("meta.stale is a bool"),
        "one successful publish is the evidence that clears the badge"
    );
}

#[test]
fn the_strip_meta_reports_the_oldest_group_not_the_newest() {
    let (reg, _d) = test_registry();
    // `left_behind` is never recomputed after the pass; `fresher` is.
    let _left_behind = reg.create_group("C:/tmp/left-behind", rails()).unwrap();
    let fresher = reg.create_group("C:/tmp/fresher", rails()).unwrap();

    let views = ViewPublisher::new();

    // THREE candidate implementations have to give three different answers
    // here, or this test only rules out whichever one it happens to differ
    // from. Stamping the pass in the PAST relative to the real clock is what
    // separates "the oldest group" from "the snapshot's own publication",
    // which an earlier version of this fixture could not do: it stamped the
    // pass at `Instant::now()`, so the snapshot stamp and the oldest group's
    // stamp were the same instant and both read as stale.
    //
    //   oldest group   -> ~600_000 ms  (stale)      <- the contract
    //   newest group   -> ~0 ms                (fresh)
    //   snapshot stamp -> ~0 ms                (fresh)      <- the store really did happen now
    let long_ago = Instant::now()
        .checked_sub(Duration::from_secs(600))
        .expect("the test host has more than 600s of uptime");
    views.publish_pass_at(&reg, long_ago);

    // One tab moves; every other tab is still 600 s behind.
    let read_at = Instant::now();
    views.publish_group_at(&reg, &fresher.id, read_at);

    let snap = views.load();
    let meta = strip_view_payload(&snap, read_at);
    let meta = section(&meta, "meta");
    let age = meta.get("age_ms").and_then(Value::as_u64).expect("meta.age_ms is a number");

    // The AGE, not just the flag: the flag alone cannot say WHICH of the three
    // candidates produced it.
    assert!(
        age >= 600_000,
        "the strip must age itself against the group left behind (~600s), not against the \
         one just recomputed and not against the snapshot's own publication (both ~0ms). \
         Got age_ms={age}: {meta}"
    );
    assert_eq!(
        meta.get("stale"),
        Some(&Value::Bool(true)),
        "...and it is therefore stale, even though a tab was just recomputed: {meta}"
    );

    // The fixture provably contains BOTH a stale group and a fresh one, so
    // the assertions above are a choice between them rather than a reading of
    // a population that only has one member.
    let fresh_age = section(&group_view_payload(&snap, &fresher.id, read_at), "meta")
        .get("age_ms")
        .and_then(Value::as_u64)
        .expect("meta.age_ms is a number");
    assert!(
        fresh_age < 1_000,
        "setup: the recomputed group must really be fresh, or the strip has nothing to \
         choose between. Got {fresh_age}ms"
    );
}

// ---------- degrades ----------

#[test]
fn a_group_the_snapshot_does_not_carry_answers_null() {
    let (reg, _d) = test_registry();
    let known = reg.create_group("C:/tmp/repo", rails()).unwrap();
    let views = ViewPublisher::new();
    let now = Instant::now();
    views.publish_pass_at(&reg, now);

    // A group created since the last pass. `command_group` has already decided
    // the id is well-formed by this point, so this is the "not published yet"
    // case, not a refusal — and both answer the same way, because the caller's
    // response to both is identical: keep the previous render, ask again.
    let later = reg.create_group("C:/tmp/other", rails()).unwrap();
    let snap = views.load();
    assert_eq!(
        group_view_payload(&snap, &later.id, now),
        Value::Null,
        "a group absent from the snapshot answers null — the same no-error-channel degrade the \
         ten commands it replaces use"
    );
    assert!(
        group_view_payload(&snap, &known.id, now).is_object(),
        "non-vacuity: a group that IS in the snapshot must answer an object, or the null above \
         is what this publisher answers for everything"
    );
}

#[test]
fn an_empty_registry_still_publishes_a_readable_strip() {
    let (reg, _d) = test_registry();
    let views = ViewPublisher::new();
    let now = Instant::now();
    views.publish_pass_at(&reg, now);
    let snap = views.load();
    let strip = strip_view_payload(&snap, now);
    assert_eq!(
        section(&strip, "groups"),
        &Value::Object(serde_json::Map::new()),
        "no groups is an empty map, never an error: {strip}"
    );
    assert_eq!(
        section(&strip, "meta").get("stale"),
        Some(&Value::Bool(false)),
        "an app with no groups is not a wedged app"
    );
}

// Local mirrors of the publisher's constants. Deliberately re-declared rather
// than imported, so a change to either value fails a test that has to be read
// and re-argued instead of silently re-deriving its own expectations from the
// value it is checking (the repo's "a pin must not build its expectation from
// the code under test" rule).
const VIEW_LEASE_MS_FOR_TEST: u64 = 10_000;
const STRIP_LEASE_MS_FOR_TEST: u64 = 10_000;
const VIEW_STALE_AFTER_MS_FOR_TEST: u64 = 5_000;

#[test]
fn the_published_constants_are_the_ones_these_tests_were_written_against() {
    assert_eq!(
        loomux_lib::orchestration::views::STRIP_LEASE_MS,
        STRIP_LEASE_MS_FOR_TEST,
        "the strip lease moved; the lapse test's arithmetic was written against the old value"
    );
    assert_eq!(
        loomux_lib::orchestration::views::VIEW_LEASE_MS,
        VIEW_LEASE_MS_FOR_TEST,
        "the view lease moved; the lapse test's arithmetic was written against the old value"
    );
    assert_eq!(
        loomux_lib::orchestration::views::VIEW_STALE_AFTER_MS,
        VIEW_STALE_AFTER_MS_FOR_TEST,
        "the staleness threshold moved; the edge test above was written against the old value"
    );
    assert_eq!(
        loomux_lib::orchestration::views::VIEW_PUBLISH_INTERVAL,
        Duration::from_millis(1000),
        "the publish cadence must stay equal to USAGE_POLL_MAX_AGE, so a served payload is never \
         staler than the usage memo the same poll reads today"
    );
}
