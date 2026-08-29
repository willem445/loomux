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

use loomux_lib::orchestration::e2ehold::{self, Target};

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
