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
//! Four separate properties, because the claim rests on four separate things
//! and any one of them could rot on its own:
//!
//! 1. **Nothing that can hold a lock or sleep survives a release build.** A
//!    source scan over `e2ehold.rs` that classifies by *shape* (does this
//!    function's body contain a hazard?) rather than by name, per CLAUDE.md's
//!    source-scanning-guard convention — a rename must not step over it. The
//!    scan recognises a function DEFINITION at any visibility, modifier and
//!    indentation, and cross-checks that every hazard occurrence in the file
//!    landed inside one it enumerated: both of its floors count at the MATCH
//!    site, so they say the instrument ran and never that it saw every
//!    subject. An earlier version enumerated `fn ` and `pub fn ` only, which
//!    left an ungated `pub(crate) fn` holding a registry mutex invisible with
//!    both floors green (#1606 review B3).
//! 2. **That refusal is performed, not asserted.** Two tests splice a
//!    hazardous definition into the real source — one an ungated
//!    `pub(crate) fn`, one a hazard outside every function — and require the
//!    report to name it. Everything else here is an absence assertion, and an
//!    absence assertion passes just as well when the instrument cannot see
//!    the subject.
//! 3. **The release profile really does turn `debug_assertions` off.** Gate
//!    (1) is only worth anything while that holds, and it holds today by
//!    cargo's default rather than by anything written down — so a future
//!    `[profile.release] debug-assertions = true` (a plausible thing to add
//!    while chasing a release-only panic) would silently arm the injector in
//!    a shipped binary with nothing else red.
//! 4. **The runtime opt-in accepts exactly one value, and a malformed request
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

/// One function definition: the attribute/doc block immediately above it,
/// and its body from the `fn` keyword to its matching closing brace.
struct Item {
    name: String,
    attrs: String,
    body: String,
    /// 0-based line range the body occupies, used by the attribution
    /// cross-check below to say which lines belong to no function at all.
    from: usize,
    to: usize,
}

/// Whether `line` DECLARES a function, at any indentation and under any
/// visibility or modifier.
///
/// This is deliberately not `starts_with("fn ") || starts_with("pub fn ")`,
/// which is what it used to be. That pair enumerates two spellings out of many
/// and silently skips the rest, so an ungated `pub(crate) fn` holding a mutex
/// never became an `Item`, never entered the hazard set, and never met the
/// gating assertion — while both of the instrument's own floors stayed green on
/// the six functions it did see. A guard that decides from a spelling enforces
/// nothing about the spellings it does not know (#1606 review B3).
/// Leading-whitespace width. The axis `decl_lines` uses to tell a declaration
/// the splitter should have enumerated from one nested inside a body it
/// consumed.
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn is_fn_decl(line: &str) -> bool {
    let mut rest = line.trim_start();

    // Visibility, if any: `pub`, `pub(crate)`, `pub(super)`, `pub(in ...)`.
    // Guarded on the following character so `pubescent_fn` is not a visibility.
    if let Some(after) = rest.strip_prefix("pub") {
        if after.starts_with('(') {
            match after.find(')') {
                Some(i) => rest = after[i + 1..].trim_start(),
                None => return false,
            }
        } else if after.starts_with(char::is_whitespace) {
            rest = after.trim_start();
        }
    }

    // Modifiers, in any order and any combination.
    loop {
        let before = rest;
        for kw in ["const ", "async ", "unsafe ", "default "] {
            if let Some(r) = rest.strip_prefix(kw) {
                rest = r.trim_start();
            }
        }
        if let Some(r) = rest.strip_prefix("extern ") {
            let r = r.trim_start();
            rest = match r.strip_prefix('"').and_then(|q| q.find('"').map(|i| &q[i + 1..])) {
                Some(after_abi) => after_abi.trim_start(),
                None => r,
            };
        }
        if rest == before {
            break;
        }
    }

    rest.starts_with("fn ")
}

/// Splits the module into its function definitions.
///
/// Bodies are delimited by BRACE DEPTH rather than by "up to the next `fn`",
/// so text between two functions — a `static`, a `const`, an `impl` header —
/// belongs to neither, which is what lets the attribution cross-check below
/// see it.
///
/// Brace counting is textual, so a mis-parse is possible in two directions and
/// each needs its own check. UNDER-spanning moves hazard occurrences OUT of
/// the items that should hold them, and `unattributed` sees that. OVER-spanning
/// moves them IN: an unbalanced `{` inside a string literal stops `depth` ever
/// returning to zero, so the item swallows the following function's lines, the
/// scan resumes past it, and that function is never an `Item` at all — while
/// its hazards read as covered and attributed to the swallowing item. If the
/// swallower is gated, an ungated hazardous function is then invisible to both
/// floors, the attribution check AND the gating loop, all green. `decl_lines`
/// below is what closes it: every line that DECLARES a function must have
/// produced one (#1606 review round 2, N2).
fn items(src: &str) -> Vec<Item> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if !is_fn_decl(lines[i]) {
            i += 1;
            continue;
        }

        let mut depth: i32 = 0;
        let mut opened = false;
        let mut j = i;
        while j < lines.len() {
            for ch in lines[j].chars() {
                if ch == '{' {
                    depth += 1;
                    opened = true;
                } else if ch == '}' {
                    depth -= 1;
                }
            }
            if opened && depth <= 0 {
                break;
            }
            j += 1;
        }
        let end = (j + 1).min(lines.len());

        let name = lines[i]
            .trim_start()
            .rsplit("fn ")
            .next()
            .unwrap_or("")
            .split(['(', '<'])
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        // The preamble is the contiguous run of attribute and doc lines
        // immediately above the `fn`. Walking BACK from the declaration (not
        // forward from the previous item) is what keeps a NEIGHBOUR's
        // `#[cfg(debug_assertions)]` from being read as this item's — the
        // exact mis-attribution that would make the scan pass on an ungated
        // function sitting under a gated one.
        let mut top = i;
        while top > 0 {
            let prev = lines[top - 1].trim_start();
            if prev.starts_with("#[") || prev.starts_with("//") {
                top -= 1;
            } else {
                break;
            }
        }

        out.push(Item {
            name,
            attrs: lines[top..i].join("\n"),
            body: lines[i..end].join("\n"),
            from: i,
            to: end,
        });
        i = end;
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

/// Everything the release-gating scan found, as data — so the assertions can
/// be made against it AND a synthetic source can be fed through the same
/// function to prove they would fire.
struct GateReport {
    items: usize,
    hazardous: usize,
    /// Functions carrying a hazard but no `#[cfg(debug_assertions)]`.
    ungated: Vec<String>,
    /// `(line number, line)` for every hazard occurrence that belongs to no
    /// function at all — the subjects the split could not see.
    unattributed: Vec<(usize, String)>,
    /// Lines that DECLARE a function the splitter is supposed to enumerate —
    /// top-level or impl-level. Must equal `items`: a declaration that produced
    /// no item is one the split lost, which is the over-spanning direction of a
    /// brace mis-parse.
    decl_lines: usize,
    /// Declarations nested inside another function's body, which `items()`
    /// never sees because it resumes past a consumed body. Reported rather than
    /// asserted: it is zero today, and counting it in `decl_lines` would
    /// false-block this guard the first time one exists (#1606 review R5).
    nested_decl_lines: usize,
}

fn gate_report(src: &str) -> GateReport {
    let items = items(src);
    let hazardous: Vec<&Item> =
        items.iter().filter(|i| HAZARDS.iter().any(|h| i.body.contains(h))).collect();
    let ungated: Vec<String> = hazardous
        .iter()
        .filter(|i| !i.attrs.contains("#[cfg(debug_assertions)]"))
        .map(|i| i.name.clone())
        .collect();

    // THE CROSS-CHECK. Both floors below count at the MATCH site — they say
    // the instrument ran, never that it saw every subject. A hazard occurrence
    // that lands outside every function the split found is exactly the subject
    // it cannot enumerate, so count at the VERIFIED site too and name what was
    // skipped (CLAUDE.md: a population control counts at the verified site).
    let covered: Vec<bool> = {
        // `total` is hoisted rather than read as `v.len()` inside the loop:
        // `iter_mut()` holds the mutable borrow, so reading the length there is
        // E0502.
        let total = src.lines().count();
        let mut v = vec![false; total];
        for item in &items {
            let to = item.to.min(total);
            for slot in v.iter_mut().take(to).skip(item.from) {
                *slot = true;
            }
        }
        v
    };
    let unattributed: Vec<(usize, String)> = src
        .lines()
        .enumerate()
        .filter(|(n, line)| {
            HAZARDS.iter().any(|h| line.contains(h)) && !covered.get(*n).copied().unwrap_or(false)
        })
        .map(|(n, line)| (n + 1, line.trim().to_string()))
        .collect();

    GateReport {
        items: items.len(),
        hazardous: hazardous.len(),
        ungated,
        unattributed,
        // Declarations the SPLITTER is supposed to enumerate: top-level (`fn`
        // at column 0) and impl-level (four spaces). A `fn` nested inside
        // another function BODY is not one — `items()` resumes past a consumed
        // body and never sees it — so counting it here would FALSE-BLOCK this
        // guard the first time anyone writes a nested helper in `e2ehold.rs`
        // (#1606 review R5).
        //
        // Indentation is the axis deliberately, and it is the only one
        // available: a swallowed function and a nested one are
        // indistinguishable to a brace walk, which is the very thing under
        // test, so a depth measured that way would be derived from the
        // instrument it is meant to check. Indentation is independent of it.
        // The residual: a nested `fn` written at four spaces or less would
        // still be counted — this file is not rustfmt-enforced, so that is a
        // convention rather than a guarantee — and `nested_decl_lines` is
        // reported beside it so the split is legible rather than silent.
        decl_lines: src.lines().filter(|l| is_fn_decl(l) && indent_of(l) <= 4).count(),
        nested_decl_lines: src.lines().filter(|l| is_fn_decl(l) && indent_of(l) > 4).count(),
    }
}

#[test]
fn nothing_that_can_hold_a_lock_or_sleep_is_compiled_into_a_release_build() {
    let report = gate_report(&module_source());

    // Positive controls on the INSTRUMENT, read before its verdict: a split
    // that found no items, or a hazard list that stopped matching, reports
    // "all clean" in bytes identical to a genuinely clean module.
    assert!(
        report.items >= 5,
        "the split found only {} functions in e2ehold.rs — it has more than that, so the \
         verdict below would be about the split rather than the module",
        report.items
    );
    assert!(
        report.hazardous >= 3,
        "only {} of e2ehold.rs's functions matched any hazard marker — the injector holds a \
         mutex, sleeps, spawns a thread and writes files, so a lower count means the markers \
         stopped matching the module rather than the module getting safer",
        report.hazardous
    );

    // Every function DECLARATION produced an item. A declaration that did not
    // is one an over-spanning brace swallowed, and a swallowed function is
    // invisible to every other check here: its hazards are attributed to the
    // swallower, so `unattributed` stays empty and the gating loop judges the
    // wrong function's attributes.
    assert_eq!(
        report.items, report.decl_lines,
        "e2ehold.rs has {} top-level/impl-level function declarations but the split \
         produced {} items — one or more were swallowed, most likely by an unbalanced \
         brace inside a string literal. A swallowed function is judged by its \
         swallower's #[cfg], so the release-gating verdict below says nothing about it. \
         ({} further declarations are nested inside a body; those are deliberately not \
         counted here — see `decl_lines`.)",
        report.decl_lines, report.items, report.nested_decl_lines
    );

    // The population control: every hazard occurrence in the FILE is inside a
    // function the split enumerated. Without this, a hazard in a shape the
    // split does not recognise is invisible with both floors still green.
    assert!(
        report.unattributed.is_empty(),
        "e2ehold.rs contains hazard occurrences that belong to no function this scan \
         enumerated, so the gating verdict below says nothing about them: {:?}",
        report.unattributed
    );

    assert!(
        report.ungated.is_empty(),
        "these functions in e2ehold.rs can hold a lock, sleep, spawn or write, but are not \
         gated on #[cfg(debug_assertions)] — they would be compiled into a shipped binary, \
         against the module doc's claim that a release build contains no injector at all: \
         {:?}",
        report.ungated
    );
}

#[test]
fn the_gate_report_really_refuses_an_ungated_definition() {
    // The counterfactual. Everything above is an absence assertion over the
    // real module, and an absence assertion passes just as well when the
    // instrument cannot see the subject — which is precisely how the previous
    // version of this scan was green while blind to `pub(crate) fn`. So
    // perform the edit rather than reasoning about it: splice the exact
    // function #1606's review B3 describes into the real source and require
    // the report to name it.
    let src = module_source();
    let clean = gate_report(&src);
    assert!(clean.ungated.is_empty(), "control: the real module is supposed to be clean");

    let injected = format!(
        "{src}\n\npub(crate) fn hold_forever(reg: &OrchRegistry) {{\n    \
         let _g = reg.groups.lock_safe();\n    \
         std::thread::sleep(std::time::Duration::from_secs(600));\n}}\n"
    );
    let report = gate_report(&injected);
    assert!(
        report.ungated.iter().any(|n| n == "hold_forever"),
        "an ungated `pub(crate) fn` that takes a registry mutex and sleeps for ten minutes \
         did not reach the gating verdict — the scan is blind to it, and a release build \
         would carry it. Report: ungated={:?} unattributed={:?} items={}",
        report.ungated,
        report.unattributed,
        report.items
    );
}

#[test]
fn an_over_spanning_brace_cannot_swallow_a_function() {
    // The counterfactual for the check above, because this file's own doctrine
    // is that a refusal is PERFORMED rather than asserted.
    //
    // The splice is the smallest edit that defeats every OTHER check here: a
    // GATED function whose string literal carries one unmatched brace, followed
    // by an ungated one that holds a registry mutex. `depth` never returns to
    // zero, so the gated item swallows the ungated function to EOF; the hazard
    // is then attributed to a `#[cfg(debug_assertions)]` item, `unattributed`
    // is empty, both floors are met, and the gating loop finds nothing.
    let src = module_source();
    let clean = gate_report(&src);
    assert_eq!(clean.items, clean.decl_lines, "control: the real module is supposed to balance");

    let injected = format!(
        "{src}\n\n#[cfg(debug_assertions)]\nfn brace_in_a_string() {{\n    \
         let _s = \"{{\";\n}}\n\n\
         pub(crate) fn swallowed_hazard(reg: &OrchRegistry) {{\n    \
         let _g = reg.groups.lock_safe();\n}}\n"
    );
    let report = gate_report(&injected);

    // Every other check is satisfied — which is the point of the test.
    assert!(report.ungated.is_empty(), "precondition: the swallowed hazard is not caught by gating");
    assert!(
        report.unattributed.is_empty(),
        "precondition: the swallowed hazard is not caught by attribution"
    );
    // …and this is the one that has to notice.
    assert!(
        report.decl_lines > report.items,
        "an over-spanning brace swallowed a function and nothing noticed: {} declarations, \
         {} items, ungated={:?}",
        report.decl_lines,
        report.items,
        report.ungated
    );
}

#[test]
fn a_hazard_outside_every_function_is_reported_as_unattributed() {
    // The other half of B3: a hazard that is not in a function at all. The
    // gating loop cannot judge it, so the scan has to SAY so rather than
    // report a clean module.
    let injected = format!(
        "{}\n\nstatic LEAK: () = {{ let _ = std::fs::write(\"x\", b\"y\"); }};\n",
        module_source()
    );
    let report = gate_report(&injected);
    assert!(
        report.unattributed.iter().any(|(_, line)| line.contains("fs::write")),
        "a hazard sitting outside every function was not reported: {:?}",
        report.unattributed
    );
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

    // Pinned to the whole body, not to `contains("{}")` (#1606 review N2): a
    // release arm of `eprintln!("{}", x)` satisfies a contains-check and every
    // hazard negative while doing something. Signature included, so widening
    // the stub's parameters is a deliberate re-bless rather than a silent one.
    let body: String = release.body.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        body,
        "pub fn start(_reg: Arc<OrchRegistry>) {}",
        "the release arm of e2ehold::start is not exactly an empty stub"
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
