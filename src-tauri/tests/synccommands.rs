//! Every **synchronous** `#[tauri::command]` in the orchestration module either
//! cannot reach a tracked lock, or runs inside a command-boundary frame (#1702,
//! #1713 review B1).
//!
//! # Why this class needs a guard at all
//!
//! Tauri dispatches a non-`async` command by calling it **inline, on the
//! webview/GUI thread, inside the WebView2 COM callback's own stack frame**:
//! `tauri-macros`' `body_blocking` -> `run_invoke_handler` -> `ipc/protocol.rs`
//! -> `wry`'s webview2 backend -> `webview2-com-sys`'s
//! `unsafe extern "system" fn Invoke`. Traced through the vendored sources for
//! #1713 review B1: there is **no `catch_unwind` anywhere on that path** in
//! `tauri`, `tauri-runtime-wry`, `wry`, `webview2-com` or `webview2-com-sys`,
//! and that `Invoke` thunk is a plain `extern "system"` with no `-unwind` ABI.
//! An unwind reaching it hits Rust's abort-on-unwind shim and the **process
//! aborts**.
//!
//! #1702 made `lock_safe` refuse a re-entrant acquisition by unwinding. On
//! every other thread that is the improvement — the registry is released and
//! one caller pays. On this one it would kill the app. A `read_budget` frame is
//! what contains it: the frame's own `catch_unwind` catches the typed unwind
//! `budget::unwind_to_frame` throws, so the refusal becomes the command's
//! degraded value and never reaches the boundary.
//!
//! So this is a **frame-mandatory class**, and the eighth command added to it
//! is the one that would forget. That is what this scan is for. It does not
//! decide from a binding's NAME (CLAUDE.md's source-scanning-guard rule): the
//! population is every sync command, the verdict is structural, and the one
//! exemption is a signature property rather than a list of blessed names.
//!
//! # What it cannot see, stated
//!
//! - It scans `mod.rs` only. A sync command in another orchestration file is
//!   invisible; there are none today, and the population floor below would not
//!   notice if one appeared elsewhere.
//! - It reads the body TEXT between the signature and the next column-0 `}`, so
//!   it sees a wrapper mentioned anywhere in that body. The frame is what
//!   matters and the frame is installed at this boundary, so that is the right
//!   granularity — but a body that merely *names* a wrapper in a comment would
//!   satisfy it.
//! - `async` commands are out of scope by construction: Tauri spawns their
//!   bodies onto the async runtime rather than running them in the COM frame,
//!   so an unwind there is a task failure and not an abort.

use loomux_lib::orchestration::OrchRegistry;
use std::path::Path;

/// A command exempt because its signature makes a tracked-lock acquisition
/// **impossible**, with the reason recorded per row.
///
/// Structural, not a blessing: a command that never receives an `OrchRegistry`
/// has nothing to call `lock_safe` on. If one of these ever grows a registry
/// parameter it stops matching the exemption and this scan starts requiring a
/// frame — which is what makes the row safe to keep. Each row is asserted to
/// still match a live registry-free command below, so a rename, a deletion or a
/// newly-added registry parameter fails loudly instead of silently watching
/// nothing.
const NO_REGISTRY: &[(&str, &str)] = &[
    ("agent_autopilot_flags", "takes `program: String`; calls a pure fn, no registry handle"),
    ("agent_cli_knobs", "takes `cli: String`; calls a pure fn, no registry handle"),
];

/// The wrappers that install a command-boundary frame.
///
/// `run_blocking` is deliberately NOT a row (#1713 review N8). It is an
/// `async fn`, and this population is async-excluded by construction, so a
/// synchronous command cannot call it — the row could only ever be satisfied by
/// that text turning up in a body for some other reason, which is this header's
/// own stated blind spot pointed at a case that cannot legitimately arise. A
/// row that can only fire wrongly is a row that widens the exemption silently,
/// and unlike `NO_REGISTRY` these are not re-checked against anything.
const FRAMED: &[&str] = &["read_command", "mutating_command"];

struct SyncCommand {
    name: String,
    line: usize,
    takes_registry: bool,
    body: String,
}

/// Every synchronous `#[tauri::command]` in `src`, with its body text.
fn sync_commands(src: &str) -> Vec<SyncCommand> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.trim() != "#[tauri::command]" {
            continue;
        }
        // Walk to the signature, skipping any further attributes and docs.
        let mut j = i + 1;
        while j < lines.len() && !lines[j].contains(" fn ") {
            j += 1;
        }
        if j >= lines.len() {
            continue;
        }
        // `async fn` is a different dispatch path entirely — see the header.
        if lines[j].contains("async fn ") {
            continue;
        }
        let Some(name) = lines[j].split(" fn ").nth(1).and_then(|r| r.split('(').next()) else {
            continue;
        };
        // The signature can span lines; the body starts after the line whose
        // trailing `{` opens it, and ends at the next column-0 `}`.
        let mut k = j;
        while k < lines.len() && !lines[k].trim_end().ends_with('{') {
            k += 1;
        }
        let sig: String = lines[j..=k.min(lines.len() - 1)].join(" ");
        let mut body = String::new();
        let mut b = k + 1;
        while b < lines.len() && lines[b] != "}" {
            body.push_str(lines[b]);
            body.push('\n');
            b += 1;
        }
        out.push(SyncCommand {
            name: name.trim().to_string(),
            line: j + 1,
            takes_registry: sig.contains("OrchRegistry"),
            body,
        });
    }
    out
}

/// The verdict, as a pure function over source text, so the SAME code that
/// judges the real module can be run against a synthetic one that must fail.
/// A guard whose refusing branch has never executed is a guard nobody has
/// checked (CLAUDE.md).
fn bare_commands(src: &str) -> Vec<String> {
    let mut bare = Vec::new();
    for c in sync_commands(src) {
        if !c.takes_registry {
            // Exempt, but only if it is a row we have argued. An UNARGUED
            // registry-free command still has to be looked at: "no registry in
            // the signature" is where the reasoning starts, not where it ends.
            if !NO_REGISTRY.iter().any(|(n, _)| *n == c.name) {
                bare.push(format!(
                    "`{}` (line {}) takes no registry but has no NO_REGISTRY row — add one with \
                     the reason, or give it a frame",
                    c.name, c.line
                ));
            }
            continue;
        }
        if FRAMED.iter().any(|w| c.body.contains(w)) {
            continue;
        }
        bare.push(format!(
            "`{}` (line {}) is a SYNCHRONOUS #[tauri::command] that takes the registry and runs \
             its body with no command-boundary frame. On Windows a re-entrant `lock_safe` \
             underneath it unwinds into webview2-com-sys's `extern \"system\" Invoke` and ABORTS \
             THE PROCESS (#1702). Wrap it in `OrchRegistry::mutating_command` (or `read_command` \
             if it only reads), or make it `async` and use `run_blocking`",
            c.name, c.line
        ));
    }
    bare
}

fn module_src() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/orchestration/mod.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn every_sync_orchestration_command_is_frame_mandatory_or_cannot_lock() {
    let src = module_src();
    let cmds = sync_commands(&src);

    // POPULATION CONTROL. An empty or tiny scan reports no violation, which is
    // byte-identical to one that found nothing wrong — the vacuity shape
    // CLAUDE.md names. The floor is loose on purpose: it pins that the scan
    // SEES this class, not how big the class happens to be today.
    assert!(
        cmds.len() >= 8,
        "the scan found only {} synchronous commands — it has gone blind to the class it guards",
        cmds.len()
    );

    let bare = bare_commands(&src);
    assert!(bare.is_empty(), "{}", bare.join("\n"));
}

#[test]
fn the_scan_really_refuses_a_bare_sync_command() {
    // The discriminating half. Every assertion above passes just as well
    // against a scan that classifies nothing, and the population floor cannot
    // tell the difference. This runs the SAME `bare_commands` over a synthetic
    // module carrying one framed command, one bare one and one async one, and
    // requires it to name exactly the bare one.
    let synthetic = concat!(
        "#[tauri::command]\n",
        "pub fn framed_one(reg: tauri::State<Arc<OrchRegistry>>, x: u32) -> Result<(), String> {\n",
        "    OrchRegistry::mutating_command(\"framed_one\", || Err(String::new()), || reg.go(x))\n",
        "}\n",
        "#[tauri::command]\n",
        "pub fn bare_one(reg: tauri::State<Arc<OrchRegistry>>, x: u32) -> Result<(), String> {\n",
        "    reg.go(x)\n",
        "}\n",
        "#[tauri::command]\n",
        "pub async fn async_one(app: AppHandle, x: u32) -> Value {\n",
        "    run_blocking(move || reg_of(&app).go(x)).await\n",
        "}\n",
    );

    let found = sync_commands(synthetic);
    assert_eq!(
        found.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["framed_one", "bare_one"],
        "the async command must not be in the population, and both sync ones must be"
    );

    let bare = bare_commands(synthetic);
    assert_eq!(bare.len(), 1, "exactly the bare command is refused: {bare:?}");
    assert!(bare[0].contains("bare_one"), "{}", bare[0]);
    assert!(!bare[0].contains("framed_one"), "the framed one must pass: {}", bare[0]);
}

#[test]
fn every_no_registry_exemption_still_names_a_live_registry_free_command() {
    // A stale allowlist row watches nothing, and this scan's whole default-deny
    // posture rests on the rows being real. Each must still name a sync command
    // that is still registry-free — a rename, a deletion, or a command that
    // GAINS a registry parameter all fail here rather than silently widening
    // the exemption.
    let src = module_src();
    let cmds = sync_commands(&src);
    for (name, reason) in NO_REGISTRY {
        let Some(c) = cmds.iter().find(|c| &c.name == name) else {
            panic!("NO_REGISTRY row `{name}` ({reason}) names no synchronous command any more");
        };
        assert!(
            !c.takes_registry,
            "NO_REGISTRY row `{name}` now TAKES a registry (mod.rs:{}) — its exemption said {reason}, \
             which is no longer true, so it needs a command-boundary frame",
            c.line
        );
    }
}

#[test]
fn the_refusal_message_is_one_paragraph() {
    // Asserted on the VALUE, not on the source text: a `\n` plus the source's
    // indentation ships that indentation to the reader, and a COLLAPSED
    // line-continuation leaves the run of spaces with no newline at all. A
    // `.contains` pin on wording sees neither form, because no asserted
    // substring straddles the break — so the SHAPE is pinned beside the content
    // (CLAUDE.md, `is_one_paragraph`).
    let msg = loomux_lib::orchestration::COMMAND_REFUSED;
    assert!(!msg.contains('\n'), "a user-facing message is one paragraph: {msg:?}");
    assert!(!msg.contains("          "), "a source indent leaked into the message: {msg:?}");
    // Content, so the shape assertions above are not satisfied by an empty or
    // placeholder string.
    assert!(msg.contains("may have partly applied"), "{msg}");
    assert!(msg.contains("breadcrumbs.log"), "{msg}");
}

#[test]
fn the_command_boundary_wrappers_really_install_a_frame() {
    // #1713 review N5. The B1 fix rests on TWO things: every sync command
    // routes through a wrapper, and the wrapper installs a frame. The scan
    // above pins the first by matching the wrapper's NAME in the body text,
    // and says nothing at all about the second.
    //
    // The mutation that stayed green without this row: delete the
    // `budget::read_budget(..)` call from `mutating_command` and call `f()`
    // directly, keeping the name and the `MutationScope`. The scan still
    // passes (the name is in every body), every lockwatch re-entrancy row
    // still passes (they build their own frames, and structurally cannot call
    // this helper — it lives in `src-tauri` and they live in the engine), and
    // all five sync mutating commands are back to aborting the process on a
    // re-entrant acquire — which is the exact defect B1 was.
    //
    // So this asks the SHIPPED helper what it installed, rather than a
    // re-implementation of it.
    use loomux_engine::budget::{budget_active_for_test, mutation_depth_for_test};

    // Discriminating half, first: outside any wrapper there is no frame and no
    // scope. Without it, "a frame is installed" below would be a statement
    // about the ambient state of the test thread rather than about the helper.
    assert!(!budget_active_for_test(), "the test thread starts with no budget frame");
    assert_eq!(mutation_depth_for_test(), 0, "and no mutation scope");

    let (framed, depth) = OrchRegistry::mutating_command(
        "frame_probe",
        || (false, 0),
        || (budget_active_for_test(), mutation_depth_for_test()),
    );
    assert!(
        framed,
        "mutating_command must install a read_budget frame — that frame's own catch_unwind is \
         the barrier keeping a re-entrant refusal off the COM boundary, and without it every \
         sync mutating command aborts the process instead of degrading"
    );
    assert!(
        depth > 0,
        "…and a MutationScope, so a budget TIMEOUT still waits unbounded (rider R1) while only \
         the re-entrant refusal unwinds"
    );

    let (framed, depth) = OrchRegistry::read_command(
        "frame_probe",
        || (false, 0),
        || (budget_active_for_test(), mutation_depth_for_test()),
    );
    assert!(framed, "read_command must install a frame too — two sync commands rely on it");
    assert_eq!(
        depth, 0,
        "a READ command must NOT enter a mutation scope; that is the whole difference between \
         the two wrappers, and a test that let both pass would not distinguish them"
    );

    // Neither may leak past its own call.
    assert!(!budget_active_for_test(), "the frame must not outlive the wrapper");
    assert_eq!(mutation_depth_for_test(), 0, "nor the scope");
}

/// Every `name: "…"` row in `tests/perf_dispatch.rs`'s `SYNC_COMMANDS` manifest.
///
/// Read out of that file's SOURCE because test binaries do not share code. That
/// is the honest cost of the cross-check below, and it is why the check is a
/// containment rather than an equality: this list carries the whole app's sync
/// commands (pty, filemgr, gitwatch, obs) plus that file's own fixture names.
fn perf_dispatch_manifest() -> Vec<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/perf_dispatch.rs");
    let src = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    src.lines()
        .filter_map(|l| l.trim().strip_prefix("name: \""))
        .filter_map(|r| r.split('"').next())
        .map(str::to_string)
        .collect()
}

#[test]
fn this_scan_and_perf_dispatch_agree_on_which_orchestration_commands_are_sync() {
    // **This file duplicates a mechanism the repo already has, deliberately.**
    // `tests/perf_dispatch.rs` also enumerates every `#[tauri::command]` and
    // default-denies an unargued sync one — for a DIFFERENT property (#743
    // INV-1: what a sync command costs on the webview thread). This scan asks
    // whether it has a command-boundary frame (#1702). Same population, two
    // questions, and folding either into the other would make one file answer
    // two unrelated things.
    //
    // What that costs is a second parse of the same surface, and theirs is the
    // better one: `code_only` strips comments and handles `cfg` arms, where
    // this scan's header admits a body that merely NAMES a wrapper in a comment
    // would satisfy it. So the two are pinned against each other here. Two
    // independent scanners that agree are stronger than one; two that silently
    // disagree mean one has gone blind, and the population floor above cannot
    // tell which.
    //
    // Containment, not equality, in BOTH directions and for different reasons.
    let mine: Vec<String> = sync_commands(&module_src()).into_iter().map(|c| c.name).collect();
    let manifest = perf_dispatch_manifest();
    assert!(manifest.len() >= 20, "the manifest read found {} rows — the extraction is blind", manifest.len());

    // Direction 1: anything this scan calls a sync command must be argued over
    // there. A miss here means their manifest is stale or this scan
    // over-matched — either way somebody has to look.
    let missing: Vec<&String> = mine.iter().filter(|n| !manifest.contains(n)).collect();
    assert!(
        missing.is_empty(),
        "sync commands this scan found that perf_dispatch's SYNC_COMMANDS does not argue: {missing:?}"
    );

    // Direction 2 is the one that guards THIS file. A manifest row whose `fn`
    // lives in `mod.rs` must have been found by the scan above; if it was not,
    // this scan is blind to a command it is supposed to be guarding, and the
    // frame check silently covers a smaller set than it claims. Matched by a
    // different path than the scan uses — name-to-file rather than
    // attribute-to-fn — so the two are not blind in the same way.
    let src = module_src();
    let blind: Vec<&String> = manifest
        .iter()
        .filter(|n| src.contains(&format!("pub fn {n}(")) && !mine.contains(n))
        .collect();
    assert!(
        blind.is_empty(),
        "perf_dispatch argues these as sync commands in mod.rs and this scan did not see them — \
         the frame guard is covering less than it claims: {blind:?}"
    );
}
