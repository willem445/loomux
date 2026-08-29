//! The backend half of #743's enforcement — **E1** in `doc/design/performance.md`.
//!
//! THE INVARIANTS. Two of the six in that note are properties of the command
//! surface as a *set*, which no test of any single module can see:
//!
//!   INV-1 (§3) — every `#[tauri::command]` either delegates its whole body to
//!   a blocking pool (§2 P1), or is sync and enumerated below with an argued
//!   class. Tauri polls an `async` command's future on the webview thread, so
//!   `async fn` alone moves nothing (§1, the #724 review finding) — the
//!   delegation is the property.
//!
//!   INV-2 (§3) — no process spawn and no network round trip on the webview
//!   thread, ever. No class permits one: a `cheap` body additionally must carry
//!   no `Command::new` / `ShellExecuteW` / `.output(` / `fs::` marker, and only
//!   a `debt` row may name one, each pointing at its conversion issue.
//!
//! `test/perfpolicy.test.ts` (E2) is the same mechanism on the frontend's
//! listener and timer surface — scan plus a test-side manifest — and the two
//! are meant to read as one idea.
//!
//! WHY A MANIFEST AND NOT A LINT. The property is not "no blocking sync
//! commands exist": 0 do today (66 when this file landed, 1 until #1592
//! converted the last one), enumerated in
//! #743's census (planning comments parts 1-2, which `performance.md` §5 names
//! as the one source of truth) and owned by the issues in `DEBT_OWNERS`. An
//! EMPTY debt tier is the manifest working, not the manifest expiring: the
//! forwards half of the equality is what refuses the next unargued sync
//! command, and it bites exactly as hard against zero rows as against one.
//! Nor does an empty tier mean every owning issue is finished — #749's scope
//! was "an index or a live-groups filter, not just a thread hop", and #1592
//! delivered the thread hop and the audit-slurp half; `performance.md` §5
//! keeps the remainder. The property is that **one cannot be
//! added silently**. A new sync command fails this test until somebody writes
//! down what it does on the webview thread and who owns moving it off, and that
//! sentence is a review-visible diff. The debt tier is the census made
//! executable: deleting a row is the roadmap, adding one has to argue itself.
//!
//! THE BOUND OF THE CLAIM, stated rather than implied. **A source scan cannot
//! follow call chains.** A sync command whose *helper* spawns is not caught
//! mechanically, and the shipped tree still has one:
//! `orch_confirm_solo_copilot_autopilot`'s one-line body hands work to a raw
//! thread it never names. Two more were live examples until their conversions
//! took them out of the sync tier — `orch_open_ref` (#762, a scan-invisible
//! pair of `git` spawns) and `fm_open`/`fm_open_with` (#746, a one-line body
//! over a blocking `ShellExecuteW` in `filemgr.rs`'s helper). Both were found
//! by reading rather than by failing, which is the residue this paragraph is
//! about, and neither being here any more is why it names them anyway: the
//! bound did not narrow, the tier did. This is the same
//! bound `gh.rs`'s in-module enumeration test (the seed this generalizes)
//! already accepts. The scan pins the shape — which commands are sync, and that
//! each one's cost is written down — and the manifest reason plus review carry
//! the residue. For the same reason the `cheap` marker check is belt-and-braces
//! on top of the review, not a substitute for it.
//!
//! Nor can a scan see "nothing before the first await": it requires the
//! delegation call to be *in* the command's own body, which is what makes an
//! inline-work regression (#724's mutation recipe) fail, but a body that did
//! real work *and then* delegated would still pass. Review owns that half.
//!
//! WHAT MAKES THE WALK EXHAUSTIVE. The discovered command-name set must
//! **equal** `command_manifest::APP_COMMANDS`, in both directions. A file the
//! walk never opened, an attribute marker that stopped matching, a command
//! registered but not found, or one found but not registered — each fails
//! loudly rather than letting the per-command assertions pass over an empty
//! set. The sync half is pinned the same way: `SYNC_COMMANDS` must equal the
//! discovered sync set, so a conversion that lands without deleting its row is
//! as red as a new sync command that lands without adding one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// ---------- the manifest ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    /// In-memory only, under briefly-held locks. No IO, no spawn.
    ///
    /// **"Briefly held" is a fact about the HOLDER, and what a sync command
    /// pays is the ACQUISITION** (#1595). If any other thread can hold the same
    /// lock, this class says nothing about how long the webview thread waits —
    /// `lock_safe` is `Mutex::lock` with poison recovery: no timeout, no
    /// try-lock, no bound. #1595's five rows were all correctly classified by
    /// the rule above (genuinely in-memory, no INV-2 marker to find) and all
    /// five froze the app, because they were on a fixed-cadence poll and the
    /// registry mutex they take is shared with the idle reaper, the watchdog,
    /// the gh poller and `note_agent_activity` on the pty output path.
    ///
    /// So the question this class does NOT answer, and a classifier has to ask
    /// separately: **can a background thread hold this lock, and is this
    /// command polled?** Both yes is not `cheap` — it is async, whatever the
    /// body costs. The scan cannot see either property (one is a call-chain
    /// fact, the other lives in the frontend), so it is review's, and it is
    /// written here because this is where the next classifier looks.
    Cheap,
    /// Deliberate and staying — argued in code at the cite and in
    /// `performance.md` §4, which the `reason` must cite by id.
    Exception,
    /// An existing offender from the census. Names the issue that owns
    /// converting it; the `reason` says what it costs on the webview thread.
    Debt,
}

struct Row {
    name: &'static str,
    class: Class,
    /// What this command does on the webview thread, in a sentence. For an
    /// `exception`, the argument plus its `performance.md` §4 id.
    reason: &'static str,
    /// The issue that owns closing this row. Required for `debt`; optional
    /// elsewhere (a `cheap` row may still point at a lock-ordering owner).
    /// Must be one of `DEBT_OWNERS`.
    issue: Option<&'static str>,
}

/// Every issue a row may name, with the scope it actually accepted. A closed
/// set on purpose: "who owns this" is then a declaration a reviewer can check
/// against the issue, not free text that quietly names anything.
const DEBT_OWNERS: &[(&str, &str)] = &[
    // EMPTY as of #1592, which converted the last debt row
    // (`orch_session_roles`). `every_declared_owner_actually_owns_something`
    // is what forces this to empty with it: an owner nobody names any more is
    // a pointer to finished work. The table and its checks stay — they are
    // what the NEXT debt row has to satisfy, and an empty closed set still
    // refuses a row that names an owner nobody declared.
];

/// The 21 synchronous `#[tauri::command]`s at this commit, seeded verbatim from
/// #743's census (planning comments parts 1-2, reconciled against
/// `APP_COMMANDS`) with #726's 16 git conversions, #752's 8 polled
/// orchestration conversions, #762's 40 orchestration mutation and lifecycle
/// conversions, #746's 25 gesture conversions, #1592's 1 and #1595's 5 already
/// removed, and #1601's 1 added. This is today's truth, not the target state.
///
/// Reconciliation against the census's own totals, so a reader can check this
/// list rather than trust it: census A=20, T=4, C=20, B=91 of 135. Here
/// 115 async = 20 A + #726's 16 + #752's 8 + #762's 40 + #746's 25 + #1592's 1
/// + #1595's 5; 15 `cheap` = the 20 C − #1595's 5; 6 `exception` = the 4 T plus
/// `resize_pty` (census B, but §4 X1 argues it stays sync) plus `liveness_stamp`
/// (#1601, born argued — §4 X7); 0 `debt` = 91 B
/// − 16 (#726) − 8 (#752) − 40 (#762) − 25 (#746) − 1 (`resize_pty`) − 1 (#1592).
///
/// **#1595 moved five `cheap` rows, and the reason matters more than the
/// count.** They were correctly classified — genuinely in-memory, no marker
/// INV-2 would catch — and they still froze the app, because `cheap` describes
/// the CRITICAL SECTION and what a poll-path sync command actually risks is the
/// ACQUISITION. See the note above `Class::Cheap`.
///
/// CITE CONVENTION. A `reason` that points at code names the **symbol** and
/// carries the line only as a parenthetical hint (`… in `PtyManager::kill`
/// (pty.rs:507 at this commit)`). Raw line numbers rot the moment anything
/// above them moves, and they rot *silently* — a rebase carries the old number
/// across and nothing marks it stale. That is not hypothetical here: #752
/// inserted ~101 lines into `orchestration/mod.rs` and invalidated a cite in
/// this file, caught only in review. The symbol survives the move; the number
/// is a convenience that is allowed to be approximate — which #762 then proved
/// by moving several hundred lines of `mod.rs` under this manifest across three
/// slices without invalidating a single surviving cite.
const SYNC_COMMANDS: &[Row] = &[
    // ---------------------------------------------------------------------
    // exception — deliberate, argued in code and in performance.md §4.
    // ---------------------------------------------------------------------
    Row {
        name: "resize_pty",
        class: Class::Exception,
        reason: "Stays sync to inherit arrival ordering from main-thread dispatch: two different \
                 sizes can be outstanding and off-thread dispatch could land them in either \
                 order, leaving ConPTY at the older geometry with no event to correct it. See \
                 performance.md §4 X1; the argument and its named falsifier are the doc comment \
                 on `resize_pty` itself (pty.rs:1662-1692 at this commit).",
        issue: None,
    },
    Row {
        name: "fm_delete_start",
        class: Class::Exception,
        reason: "Hands the delete to a dedicated OS thread that enters its own STA, because \
                 SHFileOperationW is a Shell/COM API and a generic async pool has no defined \
                 apartment state. See performance.md §4 X2; argued in the doc comment on \
                 `fm_delete_start` itself (filemgr.rs:903-944 at this commit).",
        issue: None,
    },
    Row {
        name: "ft_search_start",
        class: Class::Exception,
        reason: "Starts a cancellable streaming search walk on a raw thread and returns \
                 immediately; results arrive as bounded batch events that P5 gates handler-side. \
                 The shared cancel registry is why it is a thread with a flag rather than an \
                 opaque pool task. See performance.md §4 X3.",
        issue: None,
    },
    Row {
        name: "ft_files_start",
        class: Class::Exception,
        reason: "The file-tree half of the same shape as ft_search_start: a cancellable walk on a \
                 raw thread, returning at once, streaming ft-files batches that fileexplorer.ts \
                 renders behind one requestAnimationFrame. See performance.md §4 X3.",
        issue: None,
    },
    Row {
        name: "fm_hash_start",
        class: Class::Exception,
        reason: "Starts the cancellable hashing walk on a raw thread and returns; it streams \
                 fm-hash batches whose handler-side cost is E2's own debt row, not this one. See \
                 performance.md §4 X3.",
        issue: None,
    },
    Row {
        name: "liveness_stamp",
        class: Class::Exception,
        reason: "Stays sync because the blocking pool is one of the two things it MEASURES \
                 (#1601 Phase 0.4): delegated, it would stop running at exactly the moment \
                 the pool is exhausted, and the heartbeat would then report the webview \
                 thread stuck on the one occasion it is the only healthy half left. The \
                 body is six relaxed atomic stores and a monotonic clock read — no \
                 allocation, no IO, and no `Mutex` at all, so there is no ACQUISITION to \
                 park on either (the #1595 half of `Class::Cheap`'s note). Filed \
                 `exception` rather than `cheap` because the synchrony is a REQUIREMENT, \
                 and a future sweep that mechanically converts the cheap rows must not take \
                 it. See performance.md §4 X7 and the doc on `liveness_stamp` itself.",
        issue: None,
    },
    // ---------------------------------------------------------------------
    // cheap — in-memory only. The marker check below is belt and braces.
    // ---------------------------------------------------------------------
    Row {
        name: "pty_backend_info",
        class: Class::Cheap,
        reason: "One Path::is_file() stat of the sideloaded conpty.dll next to the exe, to pick \
                 the reported conhost build. A single stat of a local path, read once at launcher \
                 time — named here rather than hidden, because the marker check below cannot see \
                 a stat (#769) and this row is why that gap is not theoretical.",
        issue: None,
    },
    Row {
        name: "kill_pty",
        class: Class::Cheap,
        reason: "Removes the handle from the map, then signals the child; killer.kill() is a \
                 non-waiting syscall. It already follows P3: `PtyManager::kill` takes the map lock \
                 inside a `let` initializer, so the guard is a temporary dropped at that \
                 statement's semicolon and the kill runs with the map lock RELEASED. The census \
                 (part 1) read it as a lock-held exception; #763 resolved that row as wrong with \
                 no code change and made the shape self-documenting at the fn.",
        issue: None,
    },
    Row {
        name: "ft_search_cancel",
        class: Class::Cheap,
        reason: "Sets the cancel flag for a running search in the shared registry. One lock, one \
                 store, no IO — the counterpart to ft_search_start's thread, and the reason that \
                 thread can be interrupted at all.",
        issue: None,
    },
    Row {
        name: "fm_capabilities",
        class: Class::Cheap,
        reason: "Returns the compile-time capability flags for the file manager surface. A \
                 constant-shaped struct with no state read and no IO whatsoever.",
        issue: None,
    },
    Row {
        name: "git_unwatch",
        class: Class::Cheap,
        reason: "Removes one entry from the watches map so the poll loop stops servicing it, and \
                 claims a dispatch ticket in the intents map so an in-flight git_watch cannot \
                 reinstall the watch it just removed (#746). Two map mutations under briefly-held \
                 leaf locks; unlike git_watch it reads nothing from disk, and its claim has to run \
                 on the webview thread — being sync is what puts it in arrival order.",
        issue: None,
    },
    Row {
        name: "take_startup_notice",
        class: Class::Cheap,
        reason: "Takes the one-shot startup notice out of its in-memory slot so the frontend can \
                 show it once. A single Option::take under a lock, called once per app launch.",
        issue: None,
    },
    Row {
        name: "agent_autopilot_flags",
        class: Class::Cheap,
        reason: "Matches the requested CLI against a static table of autopilot flags and returns \
                 the row. No registry state, no locks held across anything, no IO.",
        issue: None,
    },
    Row {
        name: "agent_cli_knobs",
        class: Class::Cheap,
        reason: "A static cli_caps() lookup describing which knobs a given agent CLI supports. \
                 Pure table read, like agent_autopilot_flags beside it.",
        issue: None,
    },
    Row {
        name: "orch_ack_attention",
        class: Class::Cheap,
        reason: "Clears the attention flag for one agent in the in-memory registry. A gesture-rate \
                 map mutation with no persistence and no audit append.",
        issue: None,
    },
    Row {
        name: "orch_ack_attention_pty",
        class: Class::Cheap,
        reason: "The pane-keyed twin of orch_ack_attention: resolves a pty id to its agent and \
                 clears the same in-memory flag. No IO on either path.",
        issue: None,
    },
    Row {
        name: "orch_channel_list",
        class: Class::Cheap,
        reason: "Lists the in-memory channel connections for a group. The channel state lives in \
                 the registry; only the mutating channel commands touch disk.",
        issue: None,
    },
    Row {
        name: "orch_channel_for_pane",
        class: Class::Cheap,
        reason: "Resolves one pane id to its channel membership through the in-memory index. A \
                 single map lookup, called when a pane's chrome is rebuilt.",
        issue: None,
    },
    Row {
        name: "orch_solo_bind",
        class: Class::Cheap,
        reason: "Two map inserts binding a solo pane to its group identity. Nothing is persisted \
                 here; the durable half is orch_solo_prepare's MCP config write.",
        issue: None,
    },
    Row {
        name: "orch_confirm_solo_copilot_autopilot",
        class: Class::Cheap,
        reason: "The body is a one-line delegate to `OrchRegistry::confirm_solo_copilot_autopilot`, \
                 which starts the consent watcher on a raw std::thread::spawn and returns \
                 (mod.rs:23999 at this commit) — off the webview thread, but out of this scan's \
                 reach, which is the call-chain bound this file states up front. The census labels \
                 it C/T for exactly that reason.",
        issue: None,
    },
    Row {
        name: "bind_agent",
        class: Class::Cheap,
        reason: "Takes the registry lock to record the agent binding and sends one mpsc message. \
                 The receiving side does the work; this command neither writes nor audits.",
        issue: None,
    },
    // ---------------------------------------------------------------------
    // debt — EMPTY. #1592 converted the last row (`orch_session_roles`, owned
    // by #749) after #746 (F1) drained 25 and #726/#752/#762 the 64 before
    // them. The tier stays in the type: the next sync command that lands has
    // to argue itself into one of these three classes, and `debt` is where an
    // honest "yes, and here is who owns moving it" goes.
    // ---------------------------------------------------------------------
];

// ---------- the scanner ----------

/// Kept split so the marker never appears as a whole line in this file. The
/// walk reads `src/` only, so this file cannot be its own specimen today —
/// splitting it is what keeps that true if the walk is ever widened.
const ATTR: &str = concat!("#[tauri::", "command]");

/// The INV-2 markers. A `cheap` body may contain none of them.
const HAZARD_MARKERS: &[&str] = &["Command::new", "ShellExecuteW", ".output(", "fs::"];

/// Every shape of the delegation INV-1 requires in an async command's own body.
/// Two patterns and five spellings: this list is the UNION of #1601's and
/// #1607's, which landed in the same batch and each added its own.
///
/// **P1 — hand the whole body to the shared blocking pool.** `run_blocking` is
/// the crate's thin wrapper (git.rs, gh.rs); `spawn_counted` is the one counted
/// door that wrapper and the unwrapped sites (`sessions.rs` `list_sessions`,
/// `voice.rs` `voice_stop`) hand off through (#1601 Phase 0.3); and
/// `spawn_blocking` is the runtime call `spawn_counted` itself makes.
///
/// `spawn_blocking(` stays on the list even though #1601 left exactly one such
/// call in the crate (`blocking::spawn_counted`, which is not a command). It is
/// the shape a NEW conversion written from the old precedent will use, and this
/// scan's job is to accept a correct delegation, not to insist on today's
/// spelling of it — a scan that rejected the runtime call outright would fail a
/// command that delegates perfectly well and merely skipped the counter, which
/// is `src-tauri/tests/selfwatch.rs`'s finding to report, with its own message.
///
/// (#1601's version of this doc also listed `pty.rs` `write_pty`/`change_dir`
/// among the sites handing off through `spawn_counted`. True when it was
/// written; false as of #1607, which is what took them off the pool entirely.
/// Corrected here rather than inherited through the rebase.)
///
/// **P8-writer — hand the whole body to a DEDICATED OWNER THREAD with a
/// body to a DEDICATED OWNER THREAD with a completion reply. `write_pty` and
/// `change_dir` post to the pane's own writer thread and await its answer, so
/// they satisfy INV-1's actual property — nothing of the body runs on the
/// webview thread, and nothing before the first await can block — while
/// deliberately NOT using the pool. That is the point of 2.3: the shared pool is
/// a bounded resource (tokio's default 512, unconfigured), and beta6 exhausted
/// it with parked poll ticks until the app's most latency-critical path could no
/// longer be scheduled at all (#1600 §1.2). A destination the input path shares
/// with orchestration polling is the defect, not the fix.
///
/// Adding a name here WIDENS what counts as delegation, so it is a decision, not
/// a list: the bar is that the command's own body ends at an await on work
/// running somewhere that is not the webview thread. See performance.md §2.
const DELEGATION: &[&str] = &[
    "run_blocking(",
    "spawn_counted(",
    "spawn_blocking(",
    "enqueue_frontend_write(",
    "enqueue_cd(",
];

#[derive(Debug)]
struct Site {
    name: String,
    /// Repo-relative, `/`-separated, e.g. `src-tauri/src/pty.rs`.
    file: String,
    /// 1-based line of the signature.
    line: usize,
    is_async: bool,
    /// The command's own body, comments and string contents blanked out — so a
    /// marker in prose is not mistaken for code, in either direction.
    code: String,
    /// The `#[cfg(..)]` attributes attached to this site, verbatim.
    cfgs: Vec<String>,
}

/// Blank out every comment and every string/char literal, replacing each byte
/// with a space (newlines preserved) so byte offsets and line numbers still
/// line up with the original. Everything downstream — brace matching, the
/// attribute scan, the marker checks — then reads code and only code.
///
/// The alternative, scanning raw text, is how a scan reports on prose: `git.rs`
/// has the attribute marker inside a doc comment and every module here mentions
/// `Command::new` in comments explaining why it is not called on this thread.
fn code_only(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for i in from..to.min(out.len()) {
            if out[i] != b'\n' {
                out[i] = b' ';
            }
        }
    };
    let mut i = 0usize;
    while i < b.len() {
        // line comment
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            let start = i;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            blank(&mut out, start, i);
            continue;
        }
        // block comment (Rust nests them)
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            let start = i;
            let mut depth = 1usize;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            blank(&mut out, start, i);
            continue;
        }
        // raw string: r"..", r#".."#, br#".."#. The preceding-byte check keeps
        // an identifier that merely ENDS in `r` from opening one, and the
        // `b[j] != b'"'` fallthrough leaves raw identifiers (`r#type`) alone.
        let prefix_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if prefix_ok && (b[i] == b'r' || (b[i] == b'b' && i + 1 < b.len() && b[i + 1] == b'r')) {
            let mut j = if b[i] == b'b' { i + 2 } else { i + 1 };
            let mut hashes = 0usize;
            while j < b.len() && b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < b.len() && b[j] == b'"' {
                let start = i;
                j += 1;
                loop {
                    if j >= b.len() {
                        break;
                    }
                    if b[j] == b'"' {
                        let mut k = j + 1;
                        let mut seen = 0usize;
                        while k < b.len() && b[k] == b'#' && seen < hashes {
                            seen += 1;
                            k += 1;
                        }
                        if seen == hashes {
                            j = k;
                            break;
                        }
                    }
                    j += 1;
                }
                blank(&mut out, start, j);
                i = j;
                continue;
            }
        }
        // ordinary string
        if b[i] == b'"' {
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                } else if b[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            blank(&mut out, start, i);
            continue;
        }
        // char literal vs lifetime: `'a'` and `'\n'` are literals, `'a>` and
        // `'static` are not. Getting this wrong eats real code.
        if b[i] == b'\'' {
            if i + 1 < b.len() && b[i + 1] == b'\\' {
                let start = i;
                i += 2;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
                i += 1;
                blank(&mut out, start, i);
                continue;
            }
            if i + 2 < b.len() && b[i + 2] == b'\'' {
                blank(&mut out, i, i + 3);
                i += 3;
                continue;
            }
            i += 1; // a lifetime
            continue;
        }
        i += 1;
    }
    String::from_utf8(out).expect("blanking only ever writes ASCII over whole literals")
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

/// End offset (exclusive) of the block that opens at or after `from`. `None`
/// when the braces never balance — a failure, never a silently skipped command.
fn block_end(code: &str, from: usize) -> Option<usize> {
    let b = code.as_bytes();
    let mut depth = 0usize;
    let mut opened = false;
    for i in from..b.len() {
        match b[i] {
            b'{' => {
                depth += 1;
                opened = true;
            }
            b'}' => {
                depth = depth.checked_sub(1)?;
                if opened && depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every `#[tauri::command]` site in one file.
fn sites_in(rel: &str, text: &str) -> Vec<Site> {
    let code = code_only(text);
    let raw_lines: Vec<&str> = text.lines().collect();
    let code_lines: Vec<&str> = code.lines().collect();
    let starts = line_starts(&code);
    let mut out = Vec::new();

    for i in 0..code_lines.len() {
        if code_lines[i].trim() != ATTR {
            continue;
        }
        // Attributes above the marker (`#[cfg(windows)]` sits there in
        // voice.rs) and below it, up to the signature. Read from the RAW text:
        // `#[cfg(target_os = "windows")]` loses its string in `code`.
        let mut cfgs: Vec<String> = Vec::new();
        let mut up = i;
        while up > 0 {
            let t = code_lines[up - 1].trim();
            if t.is_empty() || t.starts_with("#[") {
                let raw = raw_lines[up - 1].trim();
                if raw.starts_with("#[cfg(") {
                    cfgs.push(raw.to_string());
                }
                up -= 1;
            } else {
                break;
            }
        }
        let mut j = i + 1;
        while j < code_lines.len() {
            let t = code_lines[j].trim();
            if t.is_empty() {
                j += 1;
            } else if t.starts_with('#') {
                let raw = raw_lines[j].trim();
                if raw.starts_with("#[cfg(") {
                    cfgs.push(raw.to_string());
                }
                j += 1;
            } else {
                break;
            }
        }
        assert!(
            j < code_lines.len(),
            "{rel}:{}: {ATTR} has no function after it",
            i + 1
        );
        let sig = code_lines[j].trim();
        let fn_at = sig.find("fn ").unwrap_or_else(|| {
            panic!(
                "{rel}:{}: {ATTR} is attached to `{sig}`, which is not a function — the scan \
                 cannot classify it, so it fails rather than skipping it",
                j + 1
            )
        });
        let is_async = sig[..fn_at].split_whitespace().any(|t| t == "async");
        let name: String = sig[fn_at + 3..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        assert!(
            !name.is_empty(),
            "{rel}:{}: could not read a command name out of `{sig}`",
            j + 1
        );
        let end = block_end(&code, starts[j]).unwrap_or_else(|| {
            panic!(
                "{rel}:{}: braces never balance after `{sig}` — the body extractor lost the \
                 thread, so every assertion about this command would be about the wrong text",
                j + 1
            )
        });
        out.push(Site {
            name,
            file: rel.to_string(),
            line: j + 1,
            is_async,
            code: code[starts[j]..end].to_string(),
            cfgs,
        });
    }
    out
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every command site under `src-tauri/src`, at any depth. Depth is not
/// incidental: 63 of the 135 live in `src/orchestration/mod.rs`, so a walk that
/// stopped at the top level would miss nearly half the surface — and would say
/// nothing about it, which is the one failure mode this file exists to prevent.
/// The `APP_COMMANDS` equality below is what turns that into a red test.
fn all_sites() -> Vec<Site> {
    let src = crate_root().join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    let mut out = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(crate_root())
            .expect("under the crate root")
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        out.extend(sites_in(&format!("src-tauri/{rel}"), &text));
    }
    out
}

fn has_any(code: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| code.contains(n))
}

fn markers_in(code: &str) -> Vec<&'static str> {
    HAZARD_MARKERS
        .iter()
        .copied()
        .filter(|m| code.contains(m))
        .collect()
}

fn is_windows_arm(cfg: &str) -> bool {
    cfg.contains("windows") && !cfg.contains("not(")
}

fn is_non_windows_arm(cfg: &str) -> bool {
    cfg.contains("windows") && cfg.contains("not(")
}

/// One site per command name. A name with several sites is a `#[cfg]` split
/// (`voice.rs`'s three pairs today), and the **Windows arm is the one that gets
/// classified** — Windows is the shipped platform, so it is the arm that
/// decides what a user actually pays.
///
/// That would be a hole if the other arm were a second implementation, so it is
/// not taken on trust: every non-Windows arm must be a stub — no INV-2 marker,
/// no raw thread, and short. `voice.rs`'s non-Windows `voice_stop` is exactly
/// why the check is worth having: it is an `async fn` that returns an error
/// without delegating, which is harmless in a stub and would not be in an
/// implementation.
fn dedupe_cfg_arms(sites: Vec<Site>) -> Vec<Site> {
    let mut by_name: BTreeMap<String, Vec<Site>> = BTreeMap::new();
    for s in sites {
        by_name.entry(s.name.clone()).or_default().push(s);
    }
    let mut out = Vec::new();
    for (name, group) in by_name {
        if group.len() == 1 {
            out.extend(group);
            continue;
        }
        let where_ = group
            .iter()
            .map(|s| format!("{}:{}", s.file, s.line))
            .collect::<Vec<_>>()
            .join(", ");
        let windows: Vec<usize> = (0..group.len())
            .filter(|&i| group[i].cfgs.iter().any(|c| is_windows_arm(c)))
            .collect();
        assert_eq!(
            windows.len(),
            1,
            "`{name}` has {} sites ({where_}) but not exactly one #[cfg(windows)] arm — a \
             duplicated command name is only readable as a cfg split, and this test refuses to \
             guess which one ships",
            group.len()
        );
        for (i, s) in group.iter().enumerate() {
            if i == windows[0] {
                continue;
            }
            assert!(
                s.cfgs.iter().any(|c| is_non_windows_arm(c)),
                "{}:{}: `{name}` duplicates the #[cfg(windows)] arm but is not gated \
                 #[cfg(not(windows))] — the scan cannot tell which one ships",
                s.file,
                s.line
            );
            let markers = markers_in(&s.code);
            assert!(
                markers.is_empty() && !s.code.contains("thread::spawn"),
                "{}:{}: the non-Windows arm of `{name}` is not a stub — it carries {:?}. Only the \
                 Windows arm is classified by the manifest, which is sound only while the other \
                 arm does no real work; this one does, so it needs its own row and its own \
                 argument",
                s.file,
                s.line,
                markers
            );
            assert!(
                s.code.lines().count() <= 8,
                "{}:{}: the non-Windows arm of `{name}` is {} lines — long enough to be an \
                 implementation rather than a stub, and the manifest only classifies the Windows \
                 arm",
                s.file,
                s.line,
                s.code.lines().count()
            );
        }
        out.push(group.into_iter().nth(windows[0]).expect("index checked"));
    }
    out
}

fn commands() -> Vec<Site> {
    dedupe_cfg_arms(all_sites())
}

fn names(sites: &[Site]) -> BTreeSet<String> {
    sites.iter().map(|s| s.name.clone()).collect()
}

// ---------- the extractor, pinned on synthetic sources ----------
//
// Every manifest assertion below is only as good as `code_only` and
// `block_end`, and an extractor that has drifted reports green about code it no
// longer reads. These pin the shapes the crate actually contains plus the ones
// that would silently break them.

#[test]
fn the_body_extractor_survives_nested_braces_strings_and_comments() {
    // The specimen for why this file does not reuse gh.rs's "first line that is
    // exactly `}`" heuristic: that works for ten one-expression wrappers and
    // ends the body two lines into the first `if` block anywhere else. Close to
    // half the commands have nested blocks — a ratio deliberately not written as
    // a literal here, since nothing pins it and the point is only that the naive
    // heuristic fails on a large fraction, not on a stated count (#1018).
    let src = format!(
        r##"
{ATTR}
pub fn nested(x: u32) -> String {{
    if x > 0 {{
        let s = "a }} in a string";
        let c = '}}';
        /* a }} in a block comment */
        // a }} in a line comment
        return format!("{{s}}{{c}}");
    }}
    let r = r#"a }} in a raw string"#;
    r.to_string()
}}

{ATTR}
pub async fn after_it<'a>(s: &'a str) -> Result<(), String> {{
    run_blocking(move || Ok(())).await
}}
"##
    );
    let sites = sites_in("src-tauri/src/fake.rs", &src);
    assert_eq!(
        sites.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["nested", "after_it"],
        "the scan lost a command after one with nested braces — every later site in the file goes \
         with it"
    );
    assert!(!sites[0].is_async);
    assert!(
        sites[0].code.contains("r.to_string()"),
        "the body stopped early, so assertions about `nested` would be about a fragment: {}",
        sites[0].code
    );
    // ...and the blanking is what makes that possible: the braces inside the
    // string, the char literal and both comment shapes must not be counted.
    assert!(
        !sites[0].code.contains("in a string")
            && !sites[0].code.contains("in a block comment")
            && !sites[0].code.contains("in a line comment")
            && !sites[0].code.contains("in a raw string"),
        "code_only left literal or comment text in the body, so a marker in prose would read as \
         code: {}",
        sites[0].code
    );
    // The lifetime `'a` must not be eaten as a char literal — doing so would
    // swallow the rest of the signature and the delegation with it.
    assert!(sites[1].is_async);
    assert!(has_any(&sites[1].code, DELEGATION));
}

#[test]
fn the_scan_reads_attributes_docs_and_cfgs_around_the_marker() {
    let src = format!(
        r#"
/// A doc comment, which is not an attribute.
#[cfg(windows)]
{ATTR}
#[allow(clippy::too_many_arguments)]
pub fn gated() -> u32 {{
    1
}}

#[cfg(not(windows))]
{ATTR}
pub fn gated() -> u32 {{
    0
}}
"#
    );
    let sites = sites_in("src-tauri/src/fake.rs", &src);
    assert_eq!(sites.len(), 2, "both cfg arms must be discovered");
    assert!(
        sites[0].cfgs.iter().any(|c| is_windows_arm(c)),
        "the #[cfg(windows)] above the marker was not attached to the site: {:?}",
        sites[0].cfgs
    );
    assert!(sites[1].cfgs.iter().any(|c| is_non_windows_arm(c)));
    let kept = dedupe_cfg_arms(sites);
    assert_eq!(kept.len(), 1);
    assert!(
        kept[0].code.contains('1'),
        "dedupe kept the non-Windows arm; the shipped platform decides the class"
    );
}

#[test]
fn a_commented_out_or_quoted_marker_is_not_a_command() {
    // `git.rs`'s module doc says "#[tauri::command] by calling it directly",
    // and several modules quote the marker in prose. A raw-text scan counts
    // those; this one must not, in either direction.
    let src = format!(
        r#"
/// Prose about {ATTR} in a doc comment.
// {ATTR}
const SAMPLE: &str = "{ATTR}";

{ATTR}
pub fn real() -> u32 {{
    0
}}
"#
    );
    let sites = sites_in("src-tauri/src/fake.rs", &src);
    assert_eq!(
        sites.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["real"],
        "prose was counted as a command site"
    );
}

#[test]
fn an_unbalanced_body_fails_instead_of_being_skipped() {
    let src = format!("{ATTR}\npub fn broken() -> u32 {{\n    0\n");
    // The panic message the default hook prints here is expected output, not a
    // failure; the hook is deliberately left in place because replacing it is
    // process-global and would swallow a real panic from a concurrent test.
    let panicked = std::panic::catch_unwind(|| sites_in("src-tauri/src/fake.rs", &src)).is_err();
    assert!(
        panicked,
        "a command whose braces never balance was silently dropped — a scan that drops what it \
         cannot parse under-reports, which is exactly how a manifest goes stale quietly"
    );
}

// ---------- the real tree ----------

#[test]
fn the_discovered_command_set_equals_the_registered_one() {
    // The vacuity guard, both directions, and what makes the walk *provably*
    // exhaustive rather than sampled. A file the walk never opened, a marker
    // that stopped matching, a command registered but never found, or one found
    // but never registered — each lands here instead of letting every
    // assertion below pass over an empty set.
    let found = names(&commands());
    let registered: BTreeSet<String> = loomux_lib::command_manifest::APP_COMMANDS
        .iter()
        .map(|s| s.to_string())
        .collect();

    let missing: Vec<&String> = registered.difference(&found).collect();
    assert!(
        missing.is_empty(),
        "APP_COMMANDS names commands this scan never found: {missing:?}. Either the walk missed a \
         file, or the {ATTR} marker stopped matching — until this is fixed every dispatch \
         assertion in this file is passing over less code than it claims"
    );
    let extra: Vec<&String> = found.difference(&registered).collect();
    assert!(
        extra.is_empty(),
        "these {ATTR}s are not in command_manifest::APP_COMMANDS: {extra:?}. A command missing \
         from the manifest is unreachable for every window under the #363 ACL flip — add it there \
         (and grant it) as well as here"
    );
}

#[test]
fn the_scan_still_tells_async_from_sync() {
    // Anti-vacuity for the classification itself, not just the name set. If
    // `is_async` jammed on either value, the manifest equality below would
    // catch it wholesale — but this says which half broke, and it pins the two
    // specimens the invariant was written around.
    let sites = commands();
    let git_status = sites
        .iter()
        .find(|s| s.name == "git_status")
        .expect("git_status is a command");
    assert!(
        git_status.is_async && has_any(&git_status.code, DELEGATION),
        "git_status is not read as async-delegating — it is #726's converted shape and P1's \
         largest instance, so a scan that cannot see it is not reading Rust"
    );
    let resize = sites
        .iter()
        .find(|s| s.name == "resize_pty")
        .expect("resize_pty is a command");
    assert!(
        !resize.is_async,
        "resize_pty is read as async — it is performance.md §4 X1, the argued sync exception, so \
         either the exception was silently converted (delete its row and say so) or the sync \
         detection has drifted"
    );
    // Both figures are DERIVED, not retyped (rev round 1 NB1). The previous
    // wording carried a hand-maintained pair that had drifted from every census
    // in the tree, and bumping a stale number by one only makes it stale
    // differently — so the claim now counts what it is talking about.
    let in_orchestration = sites.iter().filter(|s| s.file.contains("orchestration/")).count();
    let total = loomux_lib::command_manifest::APP_COMMANDS.len();
    assert!(
        in_orchestration > 0,
        "the walk did not descend into src/orchestration/ — {in_orchestration} of the {total} \
         commands were found there, so a non-recursive walk would leave nearly half the surface \
         undeclared rather than red"
    );
}

#[test]
fn every_async_command_hands_its_whole_body_off_the_webview_thread() {
    // Named for the property, not for one destination: since #1607 two of the
    // commands this passes (`write_pty`, `change_dir`) satisfy it precisely by
    // NOT handing their body to a blocking pool — they hand it to the pane's
    // own writer thread (P8-writer). The old name said "to a blocking pool",
    // which a CI log reader would have had to disbelieve.
    //
    // INV-1's delegation half. `async` alone is NOT the property: Tauri polls a
    // command's future on the webview thread, so an async command that runs its
    // work inline before the first await freezes the GUI exactly as a sync one
    // does (#724's review finding, performance.md §1). Requiring the call in
    // the command's OWN body is what makes #724's mutation recipe — inline the
    // sync body into the async command — fail here.
    let offenders: Vec<String> = commands()
        .iter()
        .filter(|s| s.is_async && !has_any(&s.code, DELEGATION))
        .map(|s| format!("{}:{} {}", s.file, s.line, s.name))
        .collect();
    assert!(
        offenders.is_empty(),
        "these async commands never call any of {DELEGATION:?} in their own body: \
         {offenders:?}. An async command that does its work inline is polled on the webview \
         thread and blocks it just as a sync one would (performance.md §1, §2 P1/P8-writer) — \
         hand the WHOLE body over, with nothing before the first await"
    );
}

#[test]
fn every_sync_command_is_declared_and_every_declaration_is_still_sync() {
    // INV-1's manifest half, as an equality so it bites in both directions.
    // Forwards: a new sync command cannot land without somebody writing down
    // what it does on the webview thread and who owns moving it off. Backwards:
    // a conversion that lands without deleting its row is just as red — which
    // is how the debt tier stays the executable census instead of drifting into
    // a list of things that used to be true.
    let sites = commands();
    let sync: BTreeSet<String> = sites
        .iter()
        .filter(|s| !s.is_async)
        .map(|s| s.name.clone())
        .collect();
    let declared: BTreeSet<String> = SYNC_COMMANDS.iter().map(|r| r.name.to_string()).collect();

    let undeclared: Vec<String> = sync
        .difference(&declared)
        .map(|n| {
            let s = sites.iter().find(|s| &s.name == n).expect("just found");
            format!("{}:{} {}", s.file, s.line, n)
        })
        .collect();
    assert!(
        undeclared.is_empty(),
        "a synchronous #[tauri::command] landed with no argument for being synchronous: \
         {undeclared:?}. Tauri dispatches it on the webview thread (performance.md §1), so either \
         make it a thin async fn over run_blocking (§2 P1) or add a SYNC_COMMANDS row saying what \
         it does there and — for debt — who owns moving it off"
    );

    let stale: Vec<&String> = declared.difference(&sync).collect();
    assert!(
        stale.is_empty(),
        "SYNC_COMMANDS declares commands that are no longer synchronous (or no longer exist): \
         {stale:?}. Delete those rows in the PR that converted them — that deletion is the \
         roadmap, and a row left behind makes this manifest fiction"
    );
}

#[test]
fn cheap_commands_carry_no_spawn_shell_out_or_filesystem_marker() {
    // INV-2, belt and braces on top of the review. It cannot see through a
    // helper — until #746 converted it, `fm_open`'s one-line body hid a
    // blocking ShellExecuteW, which is why that command was `debt` by argument
    // rather than by detection — but it does refuse the cheapest way to get
    // this wrong: writing `cheap` on a row whose body visibly shells out or
    // touches the filesystem.
    let sites = commands();
    let mut problems: Vec<String> = Vec::new();
    for row in SYNC_COMMANDS.iter().filter(|r| r.class == Class::Cheap) {
        let Some(site) = sites.iter().find(|s| s.name == row.name) else {
            continue; // the equality test above owns this failure
        };
        let markers = markers_in(&site.code);
        if !markers.is_empty() {
            problems.push(format!(
                "{}:{} `{}` is declared cheap but its body contains {markers:?}",
                site.file, site.line, row.name
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "{problems:?} — `cheap` means in-memory only (performance.md §3 INV-1). A command whose \
         body carries one of these markers belongs in `debt` with an owning issue, or in an \
         argued `exception` in §4. Note what this can and cannot say: the marker list is INV-2's, \
         verbatim, so a filesystem PROBE (`Path::is_file`, `exists`) is not on it and a cheap row \
         can still hold one — `pty_backend_info` does, disclosed in its reason. Widening the list \
         is an INV-2 change, tracked as #769; until then the reason field carries that residue"
    );
}

#[test]
fn every_manifest_row_carries_an_argument_that_still_points_somewhere() {
    let design = std::fs::read_to_string(crate_root().join("../doc/design/performance.md"))
        .expect("doc/design/performance.md is the citable ground for every exception row");
    let mut problems: Vec<String> = Vec::new();

    for row in SYNC_COMMANDS {
        let at = format!("SYNC_COMMANDS[{}]", row.name);
        // The reason is the whole point of the row: a class with no sentence
        // behind it is a checkbox, and the next reader learns nothing.
        if row.reason.len() < 60 {
            problems.push(format!(
                "{at}: the reason must say what this command does on the webview thread, in a \
                 sentence"
            ));
        }
        // The owner-membership and debt-needs-an-issue rules used to live here
        // too. They moved to `debt_tier_problems` (#1592 review N1) so that
        // `the_debt_tier_rules_still_bite_while_the_tier_is_empty` can run them
        // over a synthetic manifest: with the real debt tier at ZERO rows they
        // are otherwise assertions over an empty set, which passes whether or
        // not they still work. They are asserted, once, by
        // `every_declared_owner_actually_owns_something` below.
        match row.class {
            Class::Debt => {}
            Class::Exception => {
                // An exception is only an exception because §4 argues it. The
                // citation has to resolve, or the row is asserting an argument
                // that does not exist.
                let cited = row
                    .reason
                    .split("§4 X")
                    .nth(1)
                    .map(|rest| {
                        rest.chars()
                            .take_while(char::is_ascii_digit)
                            .collect::<String>()
                    })
                    .filter(|d| !d.is_empty());
                match cited {
                    None => problems.push(format!(
                        "{at}: an `exception` must cite its performance.md §4 row by id (as \
                         `performance.md §4 X<n>`) — an exception that is not in the table is an \
                         unargued sync command wearing a nicer class"
                    )),
                    Some(id) => {
                        if !design.contains(&format!("**X{id}**")) {
                            problems.push(format!(
                                "{at}: cites §4 X{id}, which is not a row in \
                                 doc/design/performance.md — the argument moved or was renamed, \
                                 and the citation went stale with it"
                            ));
                        }
                    }
                }
            }
            Class::Cheap => {}
        }
    }

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for row in SYNC_COMMANDS {
        if !seen.insert(row.name) {
            problems.push(format!(
                "SYNC_COMMANDS declares `{}` twice — two rows for one command means the manifest \
                 asserts two different classes and the reader picks",
                row.name
            ));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Every debt-tier rule, over an ARBITRARY manifest and owner table (#1592
/// review N1).
///
/// These rules used to be inlined against the two real constants. That was fine
/// while the debt tier had rows in it; #1592 converted the last one, so every
/// one of them now iterates an empty set and passes whether or not it still
/// works — CLAUDE.md's "an absence-only assertion needs a positive control",
/// applied to a whole tier rather than one assertion. Taking `rows` and
/// `owners` as parameters is what lets
/// `the_debt_tier_rules_still_bite_while_the_tier_is_empty` feed them a
/// synthetic manifest that MUST fail, so the tier stays fail-able while it is
/// empty and the next debt row lands on checks somebody has seen bite.
///
/// Returns every problem rather than asserting, so one call reports all of them
/// and the synthetic test can assert on WHICH fired rather than merely that
/// something did.
fn debt_tier_problems(rows: &[Row], owners: &[(&str, &str)]) -> Vec<String> {
    let declared: BTreeSet<&str> = owners.iter().map(|(id, _)| *id).collect();
    let mut problems: Vec<String> = Vec::new();

    for row in rows {
        let at = format!("SYNC_COMMANDS[{}]", row.name);
        if let Some(issue) = row.issue {
            if !declared.contains(issue) {
                problems.push(format!(
                    "{at}: names owner `{issue}`, which is not in DEBT_OWNERS — add it there with \
                     the scope that issue actually accepted, so `who owns this` stays a \
                     declaration a reviewer can check"
                ));
            }
        }
        if row.class == Class::Debt && row.issue.is_none() {
            problems.push(format!(
                "{at}: a debt row must name the issue that owns converting it (performance.md §3 \
                 INV-1/INV-2) — debt with no owner is just a command nobody is going to fix"
            ));
        }
    }

    // The other direction. An owner whose rows have all been converted is a
    // closed issue still listed as pending work — the same staleness the
    // manifest equality refuses for rows, applied to the table that gives them
    // their meaning.
    //
    // **Debt rows only.** A non-debt row may name an issue as a pointer (a
    // `cheap` row noting who owns its lock scope, say), and counting those
    // would let one keep an owner alive after its last real debt converted —
    // exactly the staleness this exists to catch, hidden by a row that was
    // never the issue's work in the first place.
    let used: BTreeSet<&str> =
        rows.iter().filter(|r| r.class == Class::Debt).filter_map(|r| r.issue).collect();
    for (id, scope) in owners {
        if !used.contains(id) {
            problems.push(format!(
                "DEBT_OWNERS lists `{id}`, which no row names any more — every row that issue \
                 owned has been converted, so close it out and delete the entry rather than \
                 leaving a pointer to finished work"
            ));
        }
        if !matches!(
            id.strip_prefix('#').and_then(|rest| rest.chars().next()),
            Some(c) if c.is_ascii_digit()
        ) {
            problems.push(format!("DEBT_OWNERS entry `{id}` does not start with an issue number"));
        }
        if scope.len() < 60 {
            problems.push(format!(
                "DEBT_OWNERS[{id}]: say what scope that issue accepted, in a sentence — otherwise \
                 the owner is a number and not a commitment"
            ));
        }
    }
    problems
}

#[test]
fn every_declared_owner_actually_owns_something() {
    assert!(debt_tier_problems(SYNC_COMMANDS, DEBT_OWNERS).is_empty(), "{:#?}", debt_tier_problems(SYNC_COMMANDS, DEBT_OWNERS));
}

#[test]
fn the_debt_tier_rules_still_bite_while_the_tier_is_empty() {
    // The positive control for the test above (#1592 review N1). With the real
    // tier at zero rows and `DEBT_OWNERS` empty, that assertion is true of an
    // empty set and would stay green if every rule below were deleted. These
    // fixtures MUST fail, and each is asserted by WHICH rule it trips — a bare
    // "something failed" would pass if one rule fired for all four.
    const GOOD_SCOPE: &str = "A sentence long enough to be a commitment rather than a number, \
                              which is exactly what the length floor is checking for.";

    let debt_without_owner = [Row {
        name: "orphan_command",
        class: Class::Debt,
        reason: "A debt row with no owning issue at all — the manifest says somebody should move \
                 this off the webview thread and names nobody.",
        issue: None,
    }];
    let p = debt_tier_problems(&debt_without_owner, &[]);
    assert!(
        p.iter().any(|m| m.contains("a debt row must name the issue")),
        "a debt row with no issue must be refused, got {p:#?}"
    );

    let undeclared_owner = [Row {
        name: "misfiled_command",
        class: Class::Debt,
        reason: "A debt row naming an issue that no DEBT_OWNERS entry declares, so `who owns \
                 this` is free text rather than a checkable declaration.",
        issue: Some("#99999"),
    }];
    let p = debt_tier_problems(&undeclared_owner, &[]);
    assert!(
        p.iter().any(|m| m.contains("is not in DEBT_OWNERS")),
        "an undeclared owner must be refused, got {p:#?}"
    );

    // An owner nobody names — the staleness that FORCED `DEBT_OWNERS` to empty
    // in this PR. Without this, deleting that rule would go unnoticed.
    let p = debt_tier_problems(&[], &[("#123", GOOD_SCOPE)]);
    assert!(
        p.iter().any(|m| m.contains("which no row names any more")),
        "an owner with no rows must be refused, got {p:#?}"
    );

    // The owner-table shape rules.
    let p = debt_tier_problems(&[], &[("749", GOOD_SCOPE)]);
    assert!(
        p.iter().any(|m| m.contains("does not start with an issue number")),
        "an id with no `#<digit>` must be refused, got {p:#?}"
    );
    let p = debt_tier_problems(&[], &[("#123", "too short")]);
    assert!(
        p.iter().any(|m| m.contains("say what scope that issue accepted")),
        "a scope that is not a sentence must be refused, got {p:#?}"
    );

    // Non-vacuity: a WELL-FORMED pairing must produce NO problems, or every
    // assertion above would pass against a function that refuses everything.
    let ok_rows = [Row {
        name: "well_formed_command",
        class: Class::Debt,
        reason: "A debt row that names a declared owner, which is what the four refusals above \
                 are being told apart from.",
        issue: Some("#123"),
    }];
    assert!(
        debt_tier_problems(&ok_rows, &[("#123", GOOD_SCOPE)]).is_empty(),
        "a well-formed debt row and its declared owner must pass, or the refusals prove nothing"
    );
}

// ---------- the poll path (#1595) ----------

/// Where a polled command is polled FROM, as a source location this test reads
/// rather than a fact a reviewer has to remember. `needle` is a substring that
/// appears once, ending at the `[` whose entries are the batch.
const POLL_SITES: &[(&str, &str, &str)] = &[
    (
        "src/groupview.ts",
        "private async load(): Promise<void> {",
        "GroupView.load() -- the group view's 2 s poll",
    ),
    (
        "src/tabbar.ts",
        "private async pollStatusOnce(): Promise<void> {",
        "TabBar.pollStatusOnce() -- the 4 s tab-strip loop",
    ),
];

/// The commands a poll site is ALLOWED to reach: the two that are served from
/// the published snapshot (#1608, plan #1600 §3 Phase 1). Everything else on a
/// fixed cadence acquires a registry mutex on an unbounded `lock_safe`, which
/// is the whole mechanism §1.2 establishes.
const SNAPSHOT_SERVED: &[&str] = &["orch_group_view", "orch_strip_view"];

/// The frontend's `wrapperName -> "backend_command"` map, read out of
/// `src/orchestration.ts`'s `invoke<...>("...")` calls: walk BACK from each
/// `invoke` to the nearest preceding `export const NAME`, which is the shape
/// every wrapper there uses. A wrapper this cannot resolve is simply absent,
/// and the non-vacuity assertions below are what stop an absent mapping from
/// reading as a pass.
fn frontend_command_map() -> BTreeMap<String, String> {
    let src = std::fs::read_to_string(crate_root().join("../src/orchestration.ts"))
        .expect("src/orchestration.ts is the one place a wrapper names its command");
    let mut map = BTreeMap::new();
    let mut at = 0usize;
    while let Some(i) = src[at..].find("invoke<") {
        let abs = at + i;
        at = abs + "invoke<".len();
        let Some(q1) = src[abs..].find('"') else { continue };
        let start = abs + q1 + 1;
        let Some(q2) = src[start..].find('"') else { continue };
        let cmd = &src[start..start + q2];
        if cmd.is_empty() || !cmd.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            continue;
        }
        let Some(e) = src[..abs].rfind("export const ") else { continue };
        let rest = &src[e + "export const ".len()..];
        let name: String =
            rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            map.insert(name, cmd.to_string());
        }
    }
    map
}

/// The identifiers CALLED anywhere inside one poll function's body.
///
/// Was "the identifiers inside one `Promise.all([ ... ])` batch" until #1608
/// collapsed both batches into a single call each. Reading the whole function
/// body instead is strictly wider than the batch was: a command invoked from
/// anywhere on the poll path is now in scope, not only one that happened to be
/// a member of an array literal.
///
/// **Stated bound.** Comments are stripped before brace matching, so a `{` in
/// prose cannot end the body early. String and template literals are NOT
/// stripped: a brace inside one would still miscount, and the balance
/// assertion below is what turns that into a loud failure rather than a
/// silently short body. Neither poll body contains one today.
fn poll_body_calls(file: &str, needle: &str) -> Vec<String> {
    let raw = std::fs::read_to_string(crate_root().join("..").join(file))
        .unwrap_or_else(|e| panic!("{file}: {e}"));
    let src = strip_ts_comments(&raw);
    let at = src.find(needle).unwrap_or_else(|| {
        panic!(
            "{file}: `{needle}` not found -- the poll site moved, and this test is now watching \
             nothing. Re-point it rather than deleting it."
        )
    });
    let body_start = at + needle.len();
    let bytes = src.as_bytes();
    let mut depth = 1i32;
    let mut i = body_start;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    assert!(depth == 0, "{file}: unbalanced poll-function body -- refusing to guess its extent");
    let body = &src[body_start..i - 1];
    let mut out: Vec<String> = Vec::new();
    for (idx, _) in body.match_indices('(') {
        let head: String = body[..idx]
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        if head.is_empty() {
            continue;
        }
        if body[..idx - head.len()].chars().last() == Some('.') {
            continue;
        }
        if !out.contains(&head) {
            out.push(head);
        }
    }
    out
}

/// Blank out `//` and `/* */` comments, preserving byte offsets (each removed
/// byte becomes a space, newlines kept) so nothing downstream shifts.
fn strip_ts_comments(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0usize;
    while i + 1 < b.len() {
        if b[i] == b'/' && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if b[i] == b'/' && b[i + 1] == b'*' {
            while i < b.len() && !(i + 1 < b.len() && b[i] == b'*' && b[i + 1] == b'/') {
                if b[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            if i + 1 < b.len() {
                out[i] = b' ';
                out[i + 1] = b' ';
                i += 2;
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("blanking preserves utf8 boundaries")
}
/// **Every command on a fixed-cadence poll path is async AND served from the
/// published snapshot** (#1595, then L6 of #1600's plan).
///
/// The guard that would have caught #1595 before it shipped, and deliberately
/// NOT a timing test: "the UI stayed responsive" measured with a clock is flaky
/// on CI and passes for the wrong reason on a fast machine. The property that
/// matters is structural -- Tauri dispatches a sync command on the webview/GTK
/// main-loop thread, so a POLLED sync command parks that thread every tick for
/// as long as whoever holds the registry mutex takes, and `lock_safe` is
/// `Mutex::lock` with poison recovery: no timeout, no try-lock, no bound.
///
/// **Async was never the property; it was the least of them** (#1608). This
/// test's own #1595 half "would pass on beta6" -- #1600 §2.2 says so in as
/// many words -- because beta6's poll commands were all correctly async and
/// the app still stopped accepting input in every pane. Moving an unbounded
/// acquisition off the webview thread relocates the victim onto a shared
/// 512-thread blocking pool; at 2.5-5 parked threads per second that pool is
/// exhausted in minutes, and `write_pty` can no longer be scheduled. So the
/// async half stays and a second half is added: a polled command must take no
/// registry lock at all, which here means its body reads the published
/// snapshot (`views.load(`, `orchestration/views.rs`).
///
/// **Why this reads the FRONTEND.** `SYNC_COMMANDS` can say what a command
/// costs but not how often it is called, and that fact lives in `groupview.ts`
/// and `tabbar.ts`. Which is precisely how those five passed two rounds of
/// classification (#752's conversions and #743's census) as correctly `cheap`
/// and still froze the app. Making the poll SITE an input is what stops the
/// next one from quietly acquiring a member that reaches the registry.
#[test]
fn no_command_on_a_fixed_cadence_poll_path_is_synchronous() {
    let sites = commands();
    let sync: BTreeSet<String> =
        sites.iter().filter(|s| !s.is_async).map(|s| s.name.clone()).collect();
    let map = frontend_command_map();

    // Non-vacuity FIRST, so nothing below can pass over an empty set.
    assert!(
        map.len() >= 20,
        "the wrapper map resolved only {} entries -- the invoke shape changed and this test is \
         now checking almost nothing",
        map.len()
    );
    // Two named specimens, for two different reasons. `groupSummary` is the
    // wrapper #1595 was about; it is no longer ON a poll path (#1608 took it
    // off), and it stays here because the extractor must still be able to
    // resolve an ordinary wrapper. `groupView` is what the group view polls
    // NOW, so a rename that silently stopped the map resolving it would
    // otherwise leave the loop below with nothing to check.
    assert_eq!(
        map.get("groupSummary").map(String::as_str),
        Some("orch_group_summary"),
        "the wrapper map must resolve the command #1595 was about, or its resolution of \
         anything else is not evidence"
    );
    assert_eq!(
        map.get("groupView").map(String::as_str),
        Some("orch_group_view"),
        "the wrapper map must resolve the command the group view polls TODAY, or the poll-site \
         loop below resolves nothing and every assertion in it passes vacuously"
    );
    assert!(
        !sync.is_empty(),
        "the sync set is empty -- the scanner stopped telling async from sync, so `no polled \
         command is sync` would hold vacuously"
    );

    let mut checked = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    let mut unserved: Vec<String> = Vec::new();
    let mut not_reading_the_cell: Vec<String> = Vec::new();
    let mut reached: BTreeSet<String> = BTreeSet::new();
    for (file, needle, what) in POLL_SITES {
        let calls = poll_body_calls(file, needle);
        assert!(
            !calls.is_empty(),
            "{file}: parsed an EMPTY poll body -- the extractor broke, not the code"
        );
        let mut here = 0usize;
        for call in calls {
            let Some(cmd) = map.get(&call) else { continue };
            checked += 1;
            here += 1;
            reached.insert(cmd.clone());
            if sync.contains(cmd) {
                offenders.push(format!("{cmd} (via `{call}`) polled by {what}"));
            }
            // L6, the half #1608 adds. Being ASYNC only moves the wait off the
            // webview thread and onto a blocking-pool thread; #1600 §1.2 is the
            // release where that turned out to be the same defect with a
            // different victim. What makes a polled read safe is that it takes
            // no registry lock AT ALL, and the only way to do that here is to
            // read the published snapshot.
            if !SNAPSHOT_SERVED.contains(&cmd.as_str()) {
                unserved.push(format!("{cmd} (via `{call}`) polled by {what}"));
                continue;
            }
            let Some(site) = sites.iter().find(|s| &s.name == cmd) else {
                not_reading_the_cell.push(format!("{cmd}: named in SNAPSHOT_SERVED but not a command"));
                continue;
            };
            // `code` has comments and string contents blanked, so a mention of
            // the cell in prose cannot satisfy this.
            if !site.code.contains("views.load(") {
                not_reading_the_cell.push(format!("{}:{} {cmd}", site.file, site.line));
            }
        }
        assert!(
            here > 0,
            "{file}: {what} resolved NO backend command -- the poll site no longer calls one \
             through a wrapper this test can follow, so every assertion below passes over \
             nothing. Re-point the extractor rather than deleting it."
        );
    }

    // Non-vacuity, per poll site AND in total. #1608 collapsed a ten-invoke
    // batch and a two-per-tab sweep into one call each, so the old `checked >=
    // 10` would now fail for the RIGHT reason and pass for none -- it counted
    // the batch, and there is no batch. The property that replaced it is the
    // one that actually matters: the set of commands the poll path reaches is
    // exactly the set served from the snapshot, so a poll site that stops
    // calling one, or starts calling an eleventh, both fail here.
    assert!(
        checked >= POLL_SITES.len(),
        "only {checked} poll-path calls resolved to backend commands across {} sites -- the \
         extractor is under-matching",
        POLL_SITES.len()
    );
    let served: BTreeSet<String> = SNAPSHOT_SERVED.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        reached, served,
        "the set of backend commands reachable from a fixed-cadence poll site must EQUAL the \
         set served from the published snapshot. A command that appears here and not in \
         SNAPSHOT_SERVED is a new polled registry read; one in SNAPSHOT_SERVED that is no \
         longer reached is a row watching nothing."
    );

    assert!(
        offenders.is_empty(),
        "these commands are POLLED and SYNCHRONOUS, so every tick parks the webview/GTK main \
         loop on an unbounded registry-mutex acquisition (#1595, and #1592 before it): \
         {offenders:#?}. Make each async over run_blocking and delete its SYNC_COMMANDS row. \
         `cheap` does not save a polled command: it bounds the critical section, and what the UI \
         thread pays is the acquisition."
    );
    assert!(
        unserved.is_empty(),
        "these commands are on a fixed-cadence poll path and are NOT served from the published \
         snapshot: {unserved:#?}. Being async is not enough -- #1595 moved five polled commands \
         off the webview thread and #1600 §1.2 is the release where the same unbounded wait \
         exhausted the shared blocking pool instead, and stopped every pane accepting input. A \
         polled read must take no registry lock at all: add it to the publisher \
         (orchestration/views.rs) and serve it from views.load(), or take it off the poll path."
    );
    assert!(
        not_reading_the_cell.is_empty(),
        "these commands are named in SNAPSHOT_SERVED but their bodies do not read the published \
         cell (`views.load(`): {not_reading_the_cell:#?}. The manifest row is the claim; the \
         body is what makes it true, and a row whose command quietly went back to the registry \
         would otherwise read as enforcement."
    );
}
