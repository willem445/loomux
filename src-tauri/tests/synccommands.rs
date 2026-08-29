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
const FRAMED: &[&str] = &["read_command", "mutating_command", "run_blocking"];

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
