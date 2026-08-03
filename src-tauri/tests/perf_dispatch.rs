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
//! commands exist": 66 do today, enumerated in #743's census (planning comments
//! parts 1-2, which `performance.md` §5 names as the one source of truth) and
//! owned by the issues in `DEBT_OWNERS`. The property is that **one cannot be
//! added silently**. A new sync command fails this test until somebody writes
//! down what it does on the webview thread and who owns moving it off, and that
//! sentence is a review-visible diff. The debt tier is the census made
//! executable: deleting a row is the roadmap, adding one has to argue itself.
//!
//! THE BOUND OF THE CLAIM, stated rather than implied. **A source scan cannot
//! follow call chains.** A sync command whose *helper* spawns is not caught
//! mechanically, and the shipped tree has live examples: `fm_open`'s body is
//! one line and the blocking `ShellExecuteW` is in `filemgr.rs`'s helper, and
//! `orch_confirm_solo_copilot_autopilot`'s one-line body hands work to a raw
//! thread it never names. (`orch_open_ref` was the third until #762 converted
//! it — a scan-invisible pair of `git` spawns, found by reading rather than by
//! failing, which is the residue this paragraph is about.) This is the same
//! bound `gh.rs`'s in-module
//! enumeration test (the seed this generalizes) already accepts. The scan pins
//! the shape — which commands are sync, and that each one's cost is written
//! down — and the manifest reason plus review carry the residue. For the same
//! reason the `cheap` marker check is belt-and-braces on top of the review, not
//! a substitute for it.
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
    (
        "#746",
        "F1 of #743: the remaining sync gesture commands (filemgr, fileedit, uistate, sessions, \
         cliprobe, editor, spawn_pty, dir_info, voice) convert to async + run_blocking in two \
         quick-tier slices, each deleting its rows here. Two rows ride with them that F1's list \
         does not name — discover_git_bash and git_watch, both pty/gitwatch commands of the same \
         shape; each says so in its own reason rather than widening the issue silently.",
    ),
    (
        "#749",
        "F4 of #743: orch_session_roles fans out over every group ever created — an index or a \
         live-groups filter, not just a thread hop.",
    ),
];

/// The 51 synchronous `#[tauri::command]`s at this commit, seeded verbatim from
/// #743's census (planning comments parts 1-2, reconciled against
/// `APP_COMMANDS`) with #726's 16 git conversions, #752's 8 polled
/// orchestration conversions and #762's 40 orchestration mutation and
/// lifecycle conversions already removed. This is today's truth, not the
/// target state.
///
/// Reconciliation against the census's own totals, so a reader can check this
/// list rather than trust it: census A=20, T=4, C=20, B=91 of 135. Here
/// 84 async = 20 A + #726's 16 + #752's 8 + #762's 40; 20 `cheap` = the 20 C;
/// 5 `exception` = the 4 T plus `resize_pty` (census B, but §4 X1 argues it
/// stays sync); 26 `debt` = 91 B − 16 (#726) − 8 (#752) − 40 (#762) − 1
/// (`resize_pty`), owned by #746 (25) and #749 (1).
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
    // DELIBERATELY WRONG (#762 red probe 2 of 2): `orch_set_notify` IS converted
    // — it is a thin async fn over run_blocking two commits ago — and this row
    // was left behind. That is the "a conversion that lands without deleting its
    // row is just as red" half of the manifest equality, and probe 1 could not
    // show it: the `undeclared` assertion lives in the same test and fires
    // first, so it masked this one. Reverted in the next commit.
    Row {
        name: "orch_set_notify",
        class: Class::Cheap,
        reason: "A stale row kept on purpose for one CI run, to show the manifest equality biting \
                 in the direction that polices #762's own 40 deletions.",
        issue: None,
    },
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
                 on `resize_pty` itself (pty.rs:1566-1594 at this commit).",
        issue: None,
    },
    Row {
        name: "fm_delete_start",
        class: Class::Exception,
        reason: "Hands the delete to a dedicated OS thread that enters its own STA, because \
                 SHFileOperationW is a Shell/COM API and a generic async pool has no defined \
                 apartment state. See performance.md §4 X2; argued in the doc comment on \
                 `fm_delete_start` itself (filemgr.rs:846-886 at this commit).",
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
        reason: "Removes one entry from the watches map so the poll loop stops servicing it. A \
                 single map mutation under a briefly-held lock; unlike git_watch it reads nothing \
                 from disk.",
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
        name: "orch_group_paused",
        class: Class::Cheap,
        reason: "A HashSet contains() against the paused-group set. It is in the group view's 2 s \
                 poll batch, which is exactly why it must stay in-memory: a thread-pool hop would \
                 add latency to a lookup that costs nothing.",
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
        name: "orch_notify_enabled",
        class: Class::Cheap,
        reason: "Reads the in-memory notify flag for a group. Another member of the 2 s poll \
                 batch that is deliberately left sync because it touches no file.",
        issue: None,
    },
    Row {
        name: "orch_spawn_expanded",
        class: Class::Cheap,
        reason: "Reads the in-memory spawn-strip expansion flag for a group. In the 2 s poll \
                 batch; its writer (orch_set_spawn_expanded) is the row that does the IO.",
        issue: None,
    },
    Row {
        name: "orch_group_summary",
        class: Class::Cheap,
        reason: "An in-memory filter over the registry's agent records. Its own doc contrasts it \
                 with orch_group_usage explicitly: same poll batch, same 4 s tab-strip loop, but \
                 no transcript reads — which is what keeps it in this class.",
        issue: None,
    },
    Row {
        name: "orch_group_watches",
        class: Class::Cheap,
        reason: "Returns the live notify_when watches for a group from the in-memory table. In \
                 the 2 s poll batch; the watches themselves are serviced by the poll thread, not \
                 by this read.",
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
    // debt — #746 (F1): the sync gesture commands outside orchestration.
    // ---------------------------------------------------------------------
    Row {
        name: "spawn_pty",
        class: Class::Debt,
        reason: "Runs openpty plus the child process creation (ConPTY handshake and CreateProcess) \
                 on the webview thread; the ptys lock is taken only for the insert. Once per pane \
                 and human-gestured, but it is a process spawn on the GUI thread, which INV-2 \
                 admits only as declared debt.",
        issue: Some("#746"),
    },
    Row {
        name: "dir_info",
        class: Class::Debt,
        reason: "Stats and reads .git and HEAD walking up the parent directories, on the webview \
                 thread. Was the hottest row in this block — one sync read per OSC-7 cwd report \
                 and per git-changed event, per pane watching the repo, so a rebase or an agent's \
                 commit loop drove it. #764 (S5) bounded the CALLER to one read per pane per \
                 REPO_SIGNAL_WINDOW_MS with a leading edge, which is why this is now an ordinary \
                 gesture-rate row rather than the urgent one; the sync dispatch itself is what F1 \
                 still owns.",
        issue: Some("#746"),
    },
    Row {
        name: "discover_git_bash",
        class: Class::Debt,
        reason: "Stats a bounded candidate list and scans PATH looking for git-bash, on the \
                 webview thread, once at launcher time. F1's list enumerates the other pty.rs \
                 gesture commands and omits this one; it is the same shape and rides with them.",
        issue: Some("#746"),
    },
    Row {
        name: "ft_list_dir",
        class: Class::Debt,
        reason: "A read_dir of the requested directory on the webview thread. Bounded by the \
                 directory's size, which nothing in the app bounds — a directory with tens of \
                 thousands of entries is a stall the user sees as the file tree freezing.",
        issue: Some("#746"),
    },
    Row {
        name: "ft_read_file",
        class: Class::Debt,
        reason: "An fs::read of up to 2 MiB on the webview thread. The cap bounds the memory, not \
                 the latency: 2 MiB off a cold or network path is a visible freeze.",
        issue: Some("#746"),
    },
    Row {
        name: "ft_write_file",
        class: Class::Debt,
        reason: "Write, fsync, then rename — on the webview thread. The fsync is the expensive \
                 part and is deliberate (it is what makes the replace atomic), which is precisely \
                 why it does not belong on the thread that services paint.",
        issue: Some("#746"),
    },
    Row {
        name: "ft_replace",
        class: Class::Debt,
        reason: "Per-file read plus an fsync-ed atomic write, over an UNBOUNDED number of files, \
                 on the webview thread. The worst latency shape in this block: a project-wide \
                 replace freezes the GUI for the whole operation.",
        issue: Some("#746"),
    },
    Row {
        name: "fm_list",
        class: Class::Debt,
        reason: "Enumerates a directory and stats each entry for the file manager grid, on the \
                 webview thread. Same unbounded-directory exposure as ft_list_dir.",
        issue: Some("#746"),
    },
    Row {
        name: "fm_new_folder",
        class: Class::Debt,
        reason: "Creates a directory on the webview thread. One syscall, but on a slow or remote \
                 path it blocks input and paint for as long as the filesystem takes.",
        issue: Some("#746"),
    },
    Row {
        name: "fm_new_file",
        class: Class::Debt,
        reason: "Creates an empty file on the webview thread — the same one-syscall-but-blocking \
                 shape as fm_new_folder, and converted with it.",
        issue: Some("#746"),
    },
    Row {
        name: "fm_rename",
        class: Class::Debt,
        reason: "Renames a path on the webview thread. Cheap locally and arbitrarily slow across \
                 a network share, which is the case the conversion exists for.",
        issue: Some("#746"),
    },
    Row {
        name: "fm_reveal",
        class: Class::Debt,
        reason: "Spawns explorer.exe to reveal a path. A process spawn on the webview thread — \
                 the INV-2 violation the debt class exists to name — and the spawn is in a helper, \
                 so the marker check would not see it either.",
        issue: Some("#746"),
    },
    Row {
        name: "fm_open",
        class: Class::Debt,
        reason: "Calls the BLOCKING ShellExecuteW to open a path with its registered handler, from \
                 a one-line body whose helper does the call. Shell handler resolution can take \
                 seconds on a cold association, all of it on the GUI thread.",
        issue: Some("#746"),
    },
    Row {
        name: "fm_open_with",
        class: Class::Debt,
        reason: "The open-with dialog variant of fm_open: also a blocking ShellExecuteW in a \
                 helper, and it can put a modal shell dialog up while the webview thread is the \
                 one waiting on it.",
        issue: Some("#746"),
    },
    Row {
        name: "load_ui_tabs",
        class: Class::Debt,
        reason: "Reads the persisted tab layout from disk on the webview thread, including the \
                 quarantine-rename path taken when the file will not parse. Once per launch, but \
                 it is launch latency the user feels as a slow start.",
        issue: Some("#746"),
    },
    Row {
        name: "save_ui_tabs",
        class: Class::Debt,
        reason: "Serialises the tab layout, fsyncs it and renames it into place, on the webview \
                 thread. Fired on layout gestures, so the fsync lands mid-interaction.",
        issue: Some("#746"),
    },
    Row {
        name: "load_settings",
        class: Class::Debt,
        reason: "Reads and parses settings.json on the webview thread, with the same \
                 quarantine-rename fallback as load_ui_tabs when the file is corrupt.",
        issue: Some("#746"),
    },
    Row {
        name: "save_settings",
        class: Class::Debt,
        reason: "Writes settings.json with an fsync and a rename, on the webview thread, from the \
                 settings dialog's save gesture.",
        issue: Some("#746"),
    },
    Row {
        name: "record_copilot_launch_posture",
        class: Class::Debt,
        reason: "Reads the launch-intent file and writes it back fsync-ed and renamed, on the \
                 webview thread. There is no lock: concurrent-write safety rests on rename \
                 atomicity alone, which the conversion must preserve rather than assume.",
        issue: Some("#746"),
    },
    Row {
        name: "record_claude_launch_posture",
        class: Class::Debt,
        reason: "The Claude twin of record_copilot_launch_posture: same intent read, same \
                 fsync-and-rename write on the webview thread, same rename-atomicity argument.",
        issue: Some("#746"),
    },
    Row {
        name: "probe_agent_cli",
        class: Class::Debt,
        reason: "On a cache miss it spawns the agent CLI with --help and poll-joins it for up to \
                 8 seconds, on the webview thread. Cached afterwards, so the freeze is once per \
                 CLI per session — the longest single stall any command here can produce.",
        issue: Some("#746"),
    },
    Row {
        name: "open_in_editor",
        class: Class::Debt,
        reason: "Probes PATH and PATHEXT with stats and then spawns the editor detached, on the \
                 webview thread. The spawn is detached so the wait is short, but the PATH probing \
                 in front of it is not bounded by anything the app controls.",
        issue: Some("#746"),
    },
    Row {
        name: "voice_start",
        class: Class::Debt,
        reason: "Waits on ready_rx.recv() for the WASAPI device to open WHILE HOLDING the \
                 recording mutex, on the webview thread. An indeterminate wait, not a bounded \
                 one: a slow or contended audio device freezes the GUI for as long as it takes.",
        issue: Some("#746"),
    },
    Row {
        name: "voice_cancel",
        class: Class::Debt,
        reason: "Blocking thread-join of the capture thread on the webview thread. Locks are not \
                 held across it, and it is called from Esc, pane close and app teardown — so the \
                 stall lands exactly when the user is trying to get out.",
        issue: Some("#746"),
    },
    Row {
        name: "git_watch",
        class: Class::Debt,
        reason: "Computes the repo signature (stats and reads) on the webview thread. #763 (S7) \
                 moved that IO OUTSIDE the watches mutex, so the lock-scope half — the reason this \
                 row used to belong to S7 — is done; what is left is the plain one, a sync \
                 filesystem command Tauri dispatches on the thread that services paint. F1's list \
                 does not name gitwatch, same as discover_git_bash above; it is the same shape and \
                 rides with them.",
        issue: Some("#746"),
    },
    // ---------------------------------------------------------------------
    // debt — #749: the one orchestration row with its own dedicated issue.
    // ---------------------------------------------------------------------
    Row {
        name: "orch_session_roles",
        class: Class::Debt,
        reason: "read_dir over the whole orchestration root, then group.json plus tasks.json plus \
                 merged records PER GROUP — so it scales with groups EVER CREATED, not with live \
                 ones, and it runs at app boot and on every sidebar open. Unbounded fan-out on \
                 the webview thread; a thread hop alone would not fix it.",
        issue: Some("#749"),
    },
];

// ---------- the scanner ----------

/// Kept split so the marker never appears as a whole line in this file. The
/// walk reads `src/` only, so this file cannot be its own specimen today —
/// splitting it is what keeps that true if the walk is ever widened.
const ATTR: &str = concat!("#[tauri::", "command]");

/// The INV-2 markers. A `cheap` body may contain none of them.
const HAZARD_MARKERS: &[&str] = &["Command::new", "ShellExecuteW", ".output(", "fs::"];

/// Either shape of the delegation INV-1 requires in an async command's own
/// body: `run_blocking` is the crate's thin wrapper (git.rs, gh.rs, pty.rs),
/// `spawn_blocking` the runtime call it wraps (voice.rs, sessions.rs).
const DELEGATION: &[&str] = &["run_blocking(", "spawn_blocking("];

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
    // ends the body two lines into the first `if` block anywhere else. 63 of
    // the 135 commands have nested blocks.
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
    assert!(
        sites.iter().any(|s| s.file.contains("orchestration/")),
        "the walk did not descend into src/orchestration/ — 63 of the 135 commands live there, so \
         a non-recursive walk would leave nearly half the surface undeclared rather than red"
    );
}

#[test]
fn every_async_command_hands_its_body_to_a_blocking_pool() {
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
        "these async commands never call run_blocking( or spawn_blocking( in their own body: \
         {offenders:?}. An async command that does its work inline is polled on the webview \
         thread and blocks it just as a sync one would (performance.md §1, §2 P1) — hand the \
         WHOLE body over, with nothing before the first await"
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
    // helper — `fm_open`'s one-line body hides a blocking ShellExecuteW, which
    // is why that command is `debt` by argument rather than by detection — but
    // it does refuse the cheapest way to get this wrong: writing `cheap` on a
    // row whose body visibly shells out or touches the filesystem.
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
    let owners: BTreeSet<&str> = DEBT_OWNERS.iter().map(|(id, _)| *id).collect();
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
        if let Some(issue) = row.issue {
            if !owners.contains(issue) {
                problems.push(format!(
                    "{at}: names owner `{issue}`, which is not in DEBT_OWNERS — add it there with \
                     the scope that issue actually accepted, so `who owns this` stays a \
                     declaration a reviewer can check"
                ));
            }
        }
        match row.class {
            Class::Debt => {
                if row.issue.is_none() {
                    problems.push(format!(
                        "{at}: a debt row must name the issue that owns converting it \
                         (performance.md §3 INV-1/INV-2) — debt with no owner is just a command \
                         nobody is going to fix"
                    ));
                }
            }
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

#[test]
fn every_declared_owner_actually_owns_something() {
    // The other direction of DEBT_OWNERS. An owner whose rows have all been
    // converted is a closed issue still listed as pending work — the same
    // staleness the manifest equality refuses for rows, applied to the table
    // that gives them their meaning.
    //
    // **Debt rows only.** A non-debt row may name an issue as a pointer (a
    // `cheap` row noting who owns its lock scope, say), and counting those
    // would let one keep an owner alive after its last real debt converted —
    // exactly the staleness this test exists to catch, hidden by a row that
    // was never the issue's work in the first place.
    let used: BTreeSet<&str> = SYNC_COMMANDS
        .iter()
        .filter(|r| r.class == Class::Debt)
        .filter_map(|r| r.issue)
        .collect();
    let unused: Vec<&str> = DEBT_OWNERS
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !used.contains(id))
        .collect();
    assert!(
        unused.is_empty(),
        "DEBT_OWNERS lists {unused:?}, which no row names any more — every row that issue owned \
         has been converted, so close it out and delete the entry rather than leaving a pointer \
         to finished work"
    );
    for (id, scope) in DEBT_OWNERS {
        assert!(
            matches!(
                id.strip_prefix('#').and_then(|rest| rest.chars().next()),
                Some(c) if c.is_ascii_digit()
            ),
            "DEBT_OWNERS entry `{id}` does not start with an issue number"
        );
        assert!(
            scope.len() >= 60,
            "DEBT_OWNERS[{id}]: say what scope that issue accepted, in a sentence — otherwise the \
             owner is a number and not a commitment"
        );
    }
}
