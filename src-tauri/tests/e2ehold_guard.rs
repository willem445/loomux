//! The guard on the E2E soak lane's lock-hold injector (#1603, plan #1600 §3
//! Phase 4.1).
//!
//! `orchestration::e2ehold` is the one piece of this repo that deliberately
//! makes the app misbehave — it holds a registry mutex for tens of seconds so
//! the soak spec can assert the app is still alive underneath. The claim that
//! makes that acceptable is "it cannot ship enabled", and a claim is a
//! deliverable: this file is what turns it into something a reviewer can
//! check instead of read.
//!
//! Three separate properties, because the claim rests on three separate
//! things and any one of them could rot on its own:
//!
//! 1. **Nothing that can hold a lock or sleep survives a release build.** A
//!    source scan over `e2ehold.rs` that classifies by *shape* (does this
//!    function's body contain a hazard?) rather than by name, per CLAUDE.md's
//!    source-scanning-guard convention — a rename must not step over it.
//! 2. **The release profile really does turn `debug_assertions` off.** Gate
//!    (1) is only worth anything while that holds, and it holds today by
//!    cargo's default rather than by anything written down — so a future
//!    `[profile.release] debug-assertions = true` (a plausible thing to add
//!    while chasing a release-only panic) would silently arm the injector in
//!    a shipped binary with nothing else red.
//! 3. **The runtime opt-in accepts exactly one value, and a malformed request
//!    is refused loudly.** Both are behaviours, not shapes, so both are
//!    tested by calling them. The second one matters for the same reason the
//!    spec keeps an `acquired_ms` breadcrumb: a hold that silently never
//!    happens looks, from the spec's side, exactly like the app passing the
//!    assertion it was supposed to fail.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use loomux_lib::orchestration::e2ehold::{self, Target};
use loomux_lib::orchestration::OrchRegistry;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn module_source() -> String {
    read(&crate_root().join("src").join("orchestration").join("e2ehold.rs"))
}

/// One top-level `fn` item: the attribute/doc block immediately above it, and
/// the text from its `fn` keyword to the next one.
struct Item {
    name: String,
    attrs: String,
    body: String,
}

/// Splits the module into its top-level functions.
///
/// Deliberately crude — this is one 250-line module, not a parser's job — but
/// crude in a way that fails LOUD rather than silently scanning nothing: the
/// tests below assert a floor on what the split found before reading any
/// verdict from it.
///
/// Every `fn` in this module is at top level, so its keyword sits in column 0;
/// an inherent-impl method is indented. `Target::as_str`/`Target::parse` are
/// therefore excluded, which is correct — neither can hold anything.
fn items(src: &str) -> Vec<Item> {
    let lines: Vec<&str> = src.lines().collect();
    let mut starts: Vec<usize> = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        if line.starts_with("fn ") || line.starts_with("pub fn ") {
            starts.push(n);
        }
    }

    let mut out = Vec::new();
    for (i, &decl) in starts.iter().enumerate() {
        let name = lines[decl]
            .trim_start_matches("pub ")
            .trim_start_matches("fn ")
            .split(['(', '<'])
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        // The preamble is the contiguous run of attribute and doc lines
        // immediately above the `fn`. Walking back from the declaration (not
        // forward from the previous item) is what keeps a NEIGHBOUR's
        // `#[cfg(debug_assertions)]` from being read as this item's — the
        // exact mis-attribution that would make the scan below pass on an
        // ungated function sitting under a gated one.
        let mut top = decl;
        while top > 0 {
            let prev = lines[top - 1].trim_start();
            if prev.starts_with("#[") || prev.starts_with("//") {
                top -= 1;
            } else {
                break;
            }
        }
        let attrs = lines[top..decl].join("\n");

        let end = starts.get(i + 1).copied().unwrap_or(lines.len());
        let body = lines[decl..end].join("\n");

        out.push(Item { name, attrs, body });
    }
    out
}

/// Anything that, if it ran in a shipped binary, would be the defect this
/// module's whole existence is a calculated risk about. Chosen as *shapes*
/// with no other legitimate use in this file — a lock acquisition, a sleep, a
/// thread spawn, a filesystem write — never as a function name, so a rename
/// cannot step over the guard (CLAUDE.md's source-scanning-guard convention).
const HAZARDS: &[&str] =
    &["lock_safe()", "thread::sleep", "thread::spawn", "fs::write", "fs::remove_file"];

#[test]
fn nothing_that_can_hold_a_lock_or_sleep_is_compiled_into_a_release_build() {
    let src = module_source();
    let items = items(&src);

    // Positive controls on the INSTRUMENT, read before its verdict: a split
    // that found no items, or a hazard list that stopped matching, reports
    // "all clean" in bytes identical to a genuinely clean module. Both halves
    // are asserted, because either alone can be the vacuous one.
    assert!(
        items.len() >= 5,
        "the item split found only {} top-level fns in e2ehold.rs — it has more than that, so \
         the scan below would be measuring the split rather than the module",
        items.len()
    );
    let hazardous: Vec<&Item> =
        items.iter().filter(|i| HAZARDS.iter().any(|h| i.body.contains(h))).collect();
    assert!(
        hazardous.len() >= 3,
        "only {} of e2ehold.rs's functions matched any hazard marker — the injector holds a \
         mutex, sleeps, spawns a thread and writes files, so a lower count means the markers \
         stopped matching the module rather than the module getting safer",
        hazardous.len()
    );

    for item in hazardous {
        assert!(
            item.attrs.contains("#[cfg(debug_assertions)]"),
            "`{}` in e2ehold.rs can hold a lock, sleep, spawn or write, but is not gated on \
             #[cfg(debug_assertions)] — it would be compiled into a shipped binary. The module \
             doc's claim is that a release build contains no injector at all.",
            item.name
        );
    }
}

#[test]
fn the_release_arm_of_start_is_an_empty_stub() {
    let src = module_source();
    let all = items(&src);
    let start_arms: Vec<&Item> = all.iter().filter(|i| i.name == "start").collect();
    assert_eq!(
        start_arms.len(),
        2,
        "e2ehold::start should have exactly two cfg arms (the shape voice.rs already uses for \
         its non-Windows stubs) — found {}",
        start_arms.len()
    );

    let release = start_arms
        .iter()
        .find(|i| i.attrs.contains("#[cfg(not(debug_assertions))]"))
        .expect("no #[cfg(not(debug_assertions))] arm of `start`: a release build would then \
                 either fail to link or take the debug one");
    for hazard in HAZARDS {
        assert!(
            !release.body.contains(hazard),
            "the release arm of e2ehold::start contains `{hazard}` — it must be an empty stub"
        );
    }
    assert!(
        release.body.contains("{}"),
        "the release arm of e2ehold::start is not an empty body:\n{}",
        release.body.trim()
    );
}

#[test]
fn the_release_profile_does_not_turn_debug_assertions_back_on() {
    // Workspace root: the profile moved out of src-tauri/Cargo.toml in #888
    // slice A1, so a scan of the member manifest would now be reading the
    // wrong file — silently, since an absent key looks exactly like a correct
    // one. Assert the section is where this test thinks it is first.
    let ws = crate_root().parent().expect("workspace root above src-tauri").join("Cargo.toml");
    let text = read(&ws);
    assert!(
        text.contains("[profile.release]"),
        "no [profile.release] in {} — this guard is reading the wrong manifest",
        ws.display()
    );
    let after = text.split("[profile.release]").nth(1).unwrap_or("");
    let section = after.split("\n[").next().unwrap_or(after);
    for line in section.lines().map(str::trim) {
        assert!(
            !line.starts_with("debug-assertions") && !line.starts_with("debug_assertions"),
            "[profile.release] sets `{line}`. The E2E lock-hold injector \
             (src-tauri/src/orchestration/e2ehold.rs) is kept out of shipped builds by \
             #[cfg(debug_assertions)] alone, so turning this on would compile a \
             deliberately-hanging code path into a release binary."
        );
    }
}

#[test]
fn only_the_exact_opt_in_value_arms_the_injector() {
    assert!(e2ehold::armed(Some("1")));
    // Everything else, including the values a reader would guess are truthy.
    assert!(!e2ehold::armed(None));
    assert!(!e2ehold::armed(Some("")));
    assert!(!e2ehold::armed(Some("0")));
    assert!(!e2ehold::armed(Some("true")));
    assert!(!e2ehold::armed(Some("yes")));
    assert!(!e2ehold::armed(Some(" 1")));
}

#[test]
fn a_request_names_a_real_target_and_a_bounded_hold() {
    let (target, ms) = e2ehold::parse_request(r#"{"target":"agents","hold_ms":1500}"#)
        .expect("a well-formed request parses");
    assert_eq!(target, Target::Agents);
    assert_eq!(ms, 1500);

    // All three targets resolve — the spec picks one, and a target that
    // stopped parsing would produce a soak run with no hold in it.
    assert_eq!(
        e2ehold::parse_request(r#"{"target":"groups","hold_ms":1}"#).map(|(t, _)| t),
        Ok(Target::Groups)
    );
    assert_eq!(
        e2ehold::parse_request(r#"{"target":"by_token","hold_ms":1}"#).map(|(t, _)| t),
        Ok(Target::ByToken)
    );
}

#[test]
fn a_malformed_request_is_refused_with_a_reason_rather_than_silently_not_holding() {
    for (bad, expect) in [
        ("not json", "not JSON"),
        (r#"{"hold_ms":10}"#, "no string `target`"),
        (r#"{"target":"grups","hold_ms":10}"#, "unknown target"),
        (r#"{"target":"groups"}"#, "no integer `hold_ms`"),
        (r#"{"target":"groups","hold_ms":0}"#, "greater than zero"),
        (r#"{"target":"groups","hold_ms":300001}"#, "ceiling"),
    ] {
        let err =
            e2ehold::parse_request(bad).expect_err(&format!("{bad} should have been refused"));
        assert!(
            err.contains(expect),
            "refusing {bad} said {err:?}, which does not name the problem ({expect})"
        );
    }
}

#[test]
fn the_protocol_constants_reach_the_playwright_side_unrenamed() {
    // e2e/liveness.ts reads and writes these two files, and sets that
    // environment variable, directly off disk — the Playwright process owns
    // the data dir, so it never asks the app for any of this. There is no
    // shared header between a Rust module and a TypeScript spec, so a rename
    // on this side with no matching edit there produces a soak run whose hold
    // request is never seen: green, and meaningless. This test is that joint,
    // and it reads the OTHER file rather than restating this one's constants
    // back to itself.
    let repo_root = crate_root().parent().expect("repo root above src-tauri").to_path_buf();
    let ts = read(&repo_root.join("e2e").join("liveness.ts"));
    for literal in [e2ehold::REQUEST_FILE, e2ehold::STATE_FILE, e2ehold::ENV_SUFFIX] {
        assert!(
            ts.contains(literal),
            "e2e/liveness.ts does not mention `{literal}`. Either the Rust constant was \
             renamed without the spec, or the spec stopped using the file protocol — in \
             both cases the soak lane's class assertion would run with no lock hold behind \
             it, and pass."
        );
    }
    // The ceiling is quoted in the module doc and in doc/design/e2e-testing.md;
    // pin the number so the three cannot drift apart silently.
    assert_eq!(e2ehold::MAX_HOLD_MS, 300_000);
}

// ---------------------------------------------------------------------------
// The injector's own behaviour.
//
// This is the control the E2E spec structurally cannot carry. Its class
// assertion is marked `test.fail()`, and that marker absorbs EVERY failure
// inside its own test — so an injector that silently never took a lock would
// leave the spec's probes measuring an idle app, the spec would fail, and
// Playwright would report a healthy expected failure. "The hold really
// happened" therefore has to be proven somewhere no marker reaches.
//
// It is proven DIFFERENTIALLY rather than by a single "it blocked" reading:
// a probe that never completes is indistinguishable from a probe that could
// not have completed anyway, so the same probe is run against the same
// registry with NO hold in flight and has to finish promptly.
// ---------------------------------------------------------------------------

/// A registry read that takes the `agents` mutex, run on its own thread.
/// `OrchRegistry::agent` is public and locks `agents` and nothing else, so it
/// is the narrowest possible observer of that lock's availability.
fn probe_agents_lock(reg: Arc<OrchRegistry>) -> (std::thread::JoinHandle<()>, Arc<AtomicBool>) {
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let handle = std::thread::spawn(move || {
        let _ = reg.agent("no-such-agent");
        flag.store(true, Ordering::SeqCst);
    });
    (handle, done)
}

fn wait_for<F: Fn() -> bool>(cond: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// The one sanctioned `OrchRegistry::new` in this file (#464). Every test
/// here routes through it, and `tests/orchestration.rs`'s
/// `no_registry_construction_bypasses_the_test_agent_dir_overrides` is what
/// enforces that — a raw construction anywhere else can write a generated
/// agent file into the operator's REAL `~/.claude`/`~/.copilot` agents dir on
/// its first spawn. Nothing in this file spawns, but the guard is
/// default-deny on purpose: "this one is harmless" is exactly the reasoning
/// that let 1,111 stray files accumulate.
fn registry_at(dir: &Path) -> Arc<OrchRegistry> {
    let reg = OrchRegistry::new(dir.join("orchestration"));
    reg.set_port(45999);
    reg.set_claude_agents_dir_override(dir.join("claude-agents"));
    reg.set_copilot_agents_dir_override(dir.join("copilot-agents"));
    reg.set_compact_hook_dir_override(dir.join("compacthook"));
    reg.set_copilot_hooks_dir_override(dir.join("copilot-hooks"));
    Arc::new(reg)
}

fn state_of(root: &Path) -> serde_json::Value {
    match std::fs::read_to_string(root.join(e2ehold::STATE_FILE)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    }
}

#[test]
fn with_no_request_pending_a_registry_read_completes_promptly() {
    // The differential control for the test below. Without it, "the read did
    // not finish" would be evidence about the injector only if the read could
    // have finished — and nothing else here establishes that.
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_at(dir.path());

    assert!(
        !e2ehold::run_once(&reg, dir.path()),
        "run_once claimed to have honoured a request when none was written"
    );
    assert_eq!(state_of(dir.path()), serde_json::Value::Null, "a state file appeared with no request");

    let (handle, done) = probe_agents_lock(reg);
    assert!(
        wait_for(|| done.load(Ordering::SeqCst), Duration::from_secs(5)),
        "a registry read did not complete within 5s with NOTHING holding the lock — the probe \
         itself is broken, so the held-lock test below would prove nothing"
    );
    handle.join().expect("probe thread");
}

#[test]
fn a_request_really_takes_the_named_lock_and_really_gives_it_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_at(dir.path());

    // 1.5 s: long enough that a probe blocked on the mutex cannot finish
    // inside the 500 ms window checked below by luck, short enough that the
    // suite does not pay for it.
    std::fs::write(
        dir.path().join(e2ehold::REQUEST_FILE),
        br#"{"target":"agents","hold_ms":1500}"#,
    )
    .expect("write request");

    let injector_reg = reg.clone();
    let root = dir.path().to_path_buf();
    let injector = std::thread::spawn(move || e2ehold::run_once(&injector_reg, &root));

    assert!(
        wait_for(
            || state_of(dir.path())["acquired_ms"].as_u64().unwrap_or(0) > 0,
            Duration::from_secs(5)
        ),
        "the injector never reported acquiring the lock: {}",
        state_of(dir.path())
    );
    assert!(
        state_of(dir.path())["released_ms"].is_null(),
        "the injector reported releasing the lock before the hold could have elapsed"
    );

    // The actual claim: a registry read that takes `agents` cannot proceed.
    let (handle, done) = probe_agents_lock(reg.clone());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !done.load(Ordering::SeqCst),
        "a registry read completed while the `agents` mutex was supposedly held for 1500ms — \
         the injector is not holding anything, so every E2E result taken under it would be a \
         measurement of an idle app"
    );

    // And it is a HOLD, not a leak: the read completes once the hold elapses.
    assert!(
        wait_for(|| done.load(Ordering::SeqCst), Duration::from_secs(10)),
        "the registry read never completed even after the hold should have expired — the \
         injector leaked the guard"
    );
    handle.join().expect("probe thread");

    assert!(injector.join().expect("injector thread"), "run_once did not report honouring the request");
    let final_state = state_of(dir.path());
    assert!(
        final_state["released_ms"].as_u64().unwrap_or(0) >= final_state["acquired_ms"].as_u64().unwrap_or(0),
        "released_ms is not at or after acquired_ms: {final_state}"
    );
    assert!(
        !dir.path().join(e2ehold::REQUEST_FILE).exists(),
        "the request file survived — it would be honoured again on the next tick"
    );
}

#[test]
fn a_refused_request_says_so_in_the_state_file_and_holds_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = registry_at(dir.path());
    std::fs::write(
        dir.path().join(e2ehold::REQUEST_FILE),
        br#"{"target":"nonsense","hold_ms":1500}"#,
    )
    .expect("write request");

    assert!(e2ehold::run_once(&reg, dir.path()));
    let state = state_of(dir.path());
    assert!(
        state["error"].as_str().unwrap_or_default().contains("unknown target"),
        "a bad request did not record why it was refused: {state}"
    );
    assert!(state["acquired_ms"].is_null(), "a refused request still reported an acquisition");

    // And nothing is held afterwards, so a typo in a fixture degrades to a
    // loud spec failure rather than a wedged app.
    let (handle, done) = probe_agents_lock(reg);
    assert!(
        wait_for(|| done.load(Ordering::SeqCst), Duration::from_secs(5)),
        "a refused request left the `agents` mutex held"
    );
    handle.join().expect("probe thread");
}
