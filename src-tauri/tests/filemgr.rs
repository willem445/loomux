//! Integration tests for the file-MANAGER backend (issue #214).
//!
//! Must be an integration test, not a unit test: linking `loomux_lib` pulls in the
//! full UI dependency graph, and on Windows the resulting test exe only loads
//! because build.rs embeds the comctl32-v6 manifest via `-tests`-scoped link args
//! (CLAUDE.md constraint #4). These drive the public `filemgr::*` helpers the Tauri
//! commands wrap, so no Tauri runtime is needed.
//!
//! `open_default` is NOT exercised end-to-end here, deliberately: a passing test
//! would mean this process had just launched Notepad (or a browser, or whatever is
//! registered) on a CI runner. What IS tested is everything up to the hand-off —
//! path containment and the directory refusal — which is the part that can be
//! wrong in a way that matters. The `ShellExecuteW` / `xdg-open` call itself is on
//! the human GUI-validation list.

use loomux_lib::filemgr::{
    capabilities, delete, delete_event, delete_mode, describe_delete_failure, list, new_file,
    new_folder, open_default, open_with, rename, reveal, validate_name,
};
use std::fs;
use std::path::Path;

/// The `code` half of the backend's `code: message` error convention.
fn err_code(msg: &str) -> &str {
    msg.split(':').next().unwrap_or("").trim()
}

/// Create a symlink, or report that this platform/user isn't allowed to (Windows
/// needs Developer Mode or admin). Mirrors the same helper in `fileedit.rs`.
fn try_symlink(original: &Path, link: &Path, is_dir: bool) -> bool {
    #[cfg(windows)]
    {
        let r = if is_dir {
            std::os::windows::fs::symlink_dir(original, link)
        } else {
            std::os::windows::fs::symlink_file(original, link)
        };
        r.is_ok()
    }
    #[cfg(unix)]
    {
        let _ = is_dir;
        std::os::unix::fs::symlink(original, link).is_ok()
    }
}

// ---------- path containment (the whole point of the choke point) ----------

#[test]
fn every_operation_refuses_to_escape_the_root() {
    // The manager DELETES things, so containment is not a formality. Each op is
    // pushed at `..` and must refuse — none of them may touch the parent dir.
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(parent.path().join("secret.txt"), "do not touch").unwrap();
    let rp = root.to_str().unwrap();

    for e in [
        list(rp, "../").err(),
        delete(rp, "../secret.txt").err(),
        rename(rp, "../secret.txt", "pwned.txt").err(),
        new_folder(rp, "..", "pwned").err(),
        new_file(rp, "..", "pwned.txt").err(),
        open_default(rp, "../secret.txt").err(),
    ] {
        let e = e.expect("a `..` path must be refused, not acted on");
        assert!(
            matches!(err_code(&e), "outside-root" | "not-found" | "invalid-path" | "not-dir"),
            "got: {e}"
        );
    }
    // The decisive assertion: the parent's file is still there, untouched.
    assert_eq!(
        fs::read_to_string(parent.path().join("secret.txt")).unwrap(),
        "do not touch"
    );
}

#[test]
fn a_rename_can_never_move_a_file_out_of_its_directory() {
    // `name` is one validated SEGMENT, so a rename can only re-label. A name
    // carrying a separator is what would turn it into a move — reject it at the
    // name, with a message the user can act on, before it ever reaches the path.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    fs::write(root.path().join("sub/a.txt"), "x").unwrap();

    for bad in ["../a.txt", "..\\a.txt", "other/a.txt", ".."] {
        let e = rename(rp, "sub/a.txt", bad).unwrap_err();
        assert_eq!(err_code(&e), "invalid-name", "for {bad:?}: {e}");
    }
    assert!(root.path().join("sub/a.txt").exists(), "the file must not have moved");
}

// ---------- names ----------

#[test]
fn validate_name_rejects_separators_dots_reserved_names_and_trailing_junk() {
    assert!(validate_name("hello.txt").is_ok());
    assert_eq!(validate_name("  spaced.txt  ").unwrap(), "spaced.txt", "names are trimmed");
    assert!(validate_name(".gitignore").is_ok(), "a leading dot is a normal name");

    for bad in ["", "   ", ".", "..", "a/b", "a\\b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b"] {
        assert!(validate_name(bad).is_err(), "{bad:?} must be rejected");
    }
    // Windows silently STRIPS a trailing dot, so you'd get a file called something
    // other than what you typed — refuse rather than surprise. A trailing SPACE is
    // the same hazard but is simply trimmed, which is what a user expects.
    assert!(validate_name("trailing.").is_err());
    assert_eq!(validate_name("trailing ").unwrap(), "trailing");
    // Reserved device names, with or without an extension.
    for bad in ["con", "CON", "nul", "aux.txt", "COM1", "lpt9.log"] {
        assert!(validate_name(bad).is_err(), "{bad:?} is reserved on Windows");
    }
}

// ---------- listing ----------

#[test]
fn list_reports_kind_size_mtime_and_hidden_but_does_not_sort_or_filter() {
    // Sorting (folders first, case-insensitive) and the hidden filter are PURE
    // decisions and live in the frontend where they're unit-tested without a DOM.
    // The backend's job is to report the facts, including the platform-correct
    // hidden flag, which only it can know.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir(root.path().join("zdir")).unwrap();
    fs::write(root.path().join("a.txt"), "12345").unwrap();

    let entries = list(rp, "").unwrap();
    let f = entries.iter().find(|e| e.name == "a.txt").unwrap();
    assert!(!f.is_dir);
    assert_eq!(f.size, 5);
    assert!(f.modified_ms > 0, "a just-written file must carry an mtime");

    let d = entries.iter().find(|e| e.name == "zdir").unwrap();
    assert!(d.is_dir);
    assert_eq!(d.size, 0, "directories report no size");

    // Unsorted: the backend must not have imposed folders-first (the frontend does).
    assert_eq!(entries.len(), 2);
}

#[test]
fn list_shows_a_symlink_without_following_it() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "x").unwrap();
    if !try_symlink(outside.path(), &root.path().join("link"), true) {
        eprintln!("skipping: symlinks not permitted here");
        return;
    }
    let entries = list(root.path().to_str().unwrap(), "").unwrap();
    let l = entries.iter().find(|e| e.name == "link").unwrap();
    assert!(l.is_symlink);
    assert!(!l.is_dir, "a symlink is reported as a link, never as its target");

    // And the manager cannot be navigated THROUGH it — which is what stops a
    // symlinked directory smuggling the user (or a delete) outside the root.
    let e = list(root.path().to_str().unwrap(), "link").unwrap_err();
    assert_eq!(err_code(&e), "symlink", "got: {e}");
}

// ---------- new folder ----------

#[test]
fn new_folder_creates_in_the_current_directory_and_refuses_to_clobber() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();

    let rel = new_folder(rp, "sub", "made").unwrap();
    assert_eq!(rel, "sub/made", "returns the new entry's rel, forward-slashed");
    assert!(root.path().join("sub/made").is_dir());

    // A second create with the same name is an ERROR, never a silent no-op — the
    // user must not think they made a folder they didn't.
    let e = new_folder(rp, "sub", "made").unwrap_err();
    assert_eq!(err_code(&e), "exists", "got: {e}");
}

#[test]
fn new_folder_at_the_root_uses_an_empty_rel() {
    let root = tempfile::tempdir().unwrap();
    let rel = new_folder(root.path().to_str().unwrap(), "", "top").unwrap();
    assert_eq!(rel, "top");
    assert!(root.path().join("top").is_dir());
}

// ---------- rename ----------

#[test]
fn rename_relabels_in_place_and_returns_the_new_rel() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    fs::write(root.path().join("sub/old.txt"), "keep me").unwrap();

    let rel = rename(rp, "sub/old.txt", "new.txt").unwrap();
    assert_eq!(rel, "sub/new.txt", "stays in its own directory");
    assert_eq!(fs::read_to_string(root.path().join("sub/new.txt")).unwrap(), "keep me");
    assert!(!root.path().join("sub/old.txt").exists());
}

#[test]
fn rename_refuses_to_overwrite_an_existing_entry() {
    // fs::rename silently overwrites on Unix. Losing a file to a rename typo is
    // exactly what a file manager must never do, so the clobber is guarded.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::write(root.path().join("a.txt"), "A").unwrap();
    fs::write(root.path().join("b.txt"), "B").unwrap();

    let e = rename(rp, "a.txt", "b.txt").unwrap_err();
    assert_eq!(err_code(&e), "exists", "got: {e}");
    assert_eq!(fs::read_to_string(root.path().join("b.txt")).unwrap(), "B", "B survives");
    assert_eq!(fs::read_to_string(root.path().join("a.txt")).unwrap(), "A", "A survives");
}

#[test]
fn renaming_an_entry_to_its_own_name_is_a_no_op_not_an_exists_error() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::write(root.path().join("a.txt"), "A").unwrap();
    // Committing an unchanged inline rename is an ordinary thing to do (open the
    // editor, press Enter). It must not report "already exists".
    assert_eq!(rename(rp, "a.txt", "a.txt").unwrap(), "a.txt");
    assert_eq!(fs::read_to_string(root.path().join("a.txt")).unwrap(), "A");
}

#[test]
fn renaming_something_that_vanished_reports_not_found() {
    let root = tempfile::tempdir().unwrap();
    let e = rename(root.path().to_str().unwrap(), "ghost.txt", "x.txt").unwrap_err();
    assert_eq!(err_code(&e), "not-found", "got: {e}");
}

// ---------- delete ----------

#[test]
fn delete_removes_a_file_and_reports_whether_it_was_recycled() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::write(root.path().join("bye.txt"), "x").unwrap();

    let recycled = delete(rp, "bye.txt").unwrap();
    assert!(!root.path().join("bye.txt").exists(), "the file is gone from the tree");
    // The return value must agree with what the UI was told to promise, or the
    // confirmation dialog lies about recoverability.
    assert_eq!(
        recycled,
        delete_mode().mode == "recycle",
        "delete()'s recycled flag must match delete_mode()"
    );
    assert_eq!(delete_mode().mode, if cfg!(windows) { "recycle" } else { "permanent" });
}

#[test]
fn delete_removes_a_directory_and_its_contents() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir_all(root.path().join("dir/nested")).unwrap();
    fs::write(root.path().join("dir/nested/deep.txt"), "x").unwrap();

    delete(rp, "dir").unwrap();
    assert!(!root.path().join("dir").exists());
}

#[test]
fn delete_refuses_to_remove_the_pane_root_itself() {
    // This test found a real one. `"   "` looks like an ordinary relative path to a
    // lexical "is rel empty" check — but Windows STRIPS trailing spaces from path
    // components, so it resolves to the root, and the first implementation happily
    // sent the pane's own root folder to the Recycle Bin. The guard is therefore on
    // the RESOLVED path, which catches this whole class ("." and "a/.." too).
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    for rel in ["", "/", "   ", ".", "./", "sub/.."] {
        let e = delete(rp, rel).unwrap_err();
        assert_eq!(err_code(&e), "invalid-path", "for {rel:?}: {e}");
    }
    assert!(root.path().is_dir(), "the root survives every one of them");
    assert!(root.path().join("sub").is_dir());
}

#[test]
fn rename_refuses_to_relabel_the_pane_root_itself() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    for rel in ["", ".", "   "] {
        let e = rename(rp, rel, "pwned").unwrap_err();
        assert_eq!(err_code(&e), "invalid-path", "for {rel:?}: {e}");
    }
    assert!(root.path().is_dir());
}

#[test]
fn deleting_something_that_vanished_reports_not_found() {
    let root = tempfile::tempdir().unwrap();
    let e = delete(root.path().to_str().unwrap(), "ghost.txt").unwrap_err();
    assert_eq!(err_code(&e), "not-found", "got: {e}");
}

// ---------- open with the default app ----------

#[test]
fn open_refuses_a_directory_without_launching_anything() {
    // Navigating into a folder is the manager's own job. Handing one to the shell
    // would pop a SECOND Explorer window — precisely the thing #214 exists to stop
    // the user needing. This is checked before any shell hand-off, so the test
    // launches nothing.
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    let e = open_default(root.path().to_str().unwrap(), "sub").unwrap_err();
    assert_eq!(err_code(&e), "is-dir", "got: {e}");
}

#[test]
fn open_refuses_a_missing_file_without_launching_anything() {
    let root = tempfile::tempdir().unwrap();
    let e = open_default(root.path().to_str().unwrap(), "ghost.pdf").unwrap_err();
    assert_eq!(err_code(&e), "not-found", "got: {e}");
}

// ---------- containment, round 3 (rev-102's live-probe findings, pinned) ----------
//
// These three behaviors held when rev-102 attacked them with a scratch test run, but
// nothing in the tree pinned them. They are load-bearing — each is a way the OS and
// Rust disagree about what a path means — so they get tests, not trust.

#[test]
fn trailing_dot_component_cannot_alias_a_sibling_entry() {
    // The root-delete test covers the trim-to-empty arm ("   " → the root). This is
    // the OTHER half of the same hazard: Win32 strips "sub." to "sub", so an op on
    // "sub." would have Rust believing it acted on one entry while the OS acted on a
    // DIFFERENT, real one. `has_mangled_component` refuses it before the filesystem
    // ever sees it.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    fs::write(root.path().join("sub/keep.txt"), "x").unwrap();

    for (rel, e) in [
        ("sub.", delete(rp, "sub.")),
        ("sub ", delete(rp, "sub ")),
        ("sub.", rename(rp, "sub.", "gone").map(|_| true)),
    ] {
        assert_eq!(err_code(&e.unwrap_err()), "invalid-path", "{rel:?}");
    }
    assert!(
        root.path().join("sub/keep.txt").exists(),
        "the real `sub` must be untouched — the alias must never have reached it"
    );
}

#[test]
fn drive_relative_unc_and_absolute_rels_are_refused() {
    // The drive-relative form is the sneaky one: `root.join("C:evil.txt")` REPLACES
    // the whole path in Rust rather than appending to it, so a naive join-and-go
    // would write outside the root entirely. `safe_resolve` rejects any `rel`
    // carrying a Prefix/RootDir component, which covers this whole family.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    for rel in [
        "C:evil.txt",             // drive-relative — REPLACES the join, not appends
        "C:\\Windows\\evil.txt",  // absolute, with a drive prefix
        "\\\\server\\share\\x",   // UNC
        "\\\\?\\C:\\x",           // verbatim
        "/abs.txt",
        "\\abs.txt",
    ] {
        let e = delete(rp, rel).unwrap_err();
        assert!(
            matches!(err_code(&e), "invalid-path" | "outside-root" | "not-found"),
            "{rel:?}: {e}"
        );
    }
}

#[test]
fn ops_on_a_symlink_entry_itself_are_refused_and_the_target_survives() {
    // A symlink (or a Windows junction) is SHOWN in the listing but is inert: it is
    // never followed, and it is never operated on either. `ensure_no_symlink` lstats
    // the FINAL component too, so delete/rename/open on the link are refused outright
    // — which is what makes a junction pointing outside the root a non-vector for a
    // recursive Recycle-Bin delete. The question "does FO_DELETE recurse through a
    // junction" never gets to be asked, because the op never runs.
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "precious").unwrap();
    if !try_symlink(outside.path(), &root.path().join("link"), true) {
        eprintln!("skipping: symlinks not permitted here");
        return;
    }
    let rp = root.path().to_str().unwrap();

    assert_eq!(err_code(&delete(rp, "link").unwrap_err()), "symlink");
    assert_eq!(err_code(&rename(rp, "link", "l2").unwrap_err()), "symlink");
    assert_eq!(err_code(&open_default(rp, "link").unwrap_err()), "symlink");

    assert_eq!(
        fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
        "precious",
        "nothing outside the root may be touched through the link"
    );
    assert!(
        fs::symlink_metadata(root.path().join("link")).is_ok(),
        "and the link entry itself is left alone"
    );
}

// ---------- new file (human demo feedback, #214) ----------

#[test]
fn new_file_creates_an_empty_file_and_refuses_to_clobber() {
    // Mirrors the new_folder contract exactly — same validation path, same containment
    // choke point, same refuse-don't-clobber rule. The only differences that matter:
    // what lands on disk is a file, and it is EMPTY (loomux does not decide what a
    // file the user just made is for — their next double-click hands it to their app).
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();

    let rel = new_file(rp, "sub", "notes.md").unwrap();
    assert_eq!(rel, "sub/notes.md", "returns the new entry's rel, forward-slashed");
    let made = root.path().join("sub/notes.md");
    assert!(made.is_file());
    assert_eq!(fs::read(&made).unwrap().len(), 0, "a NEW file is empty");

    // A second create with the same name is an ERROR, never a silent no-op — and in
    // particular never a truncation of the file that's already there.
    let e = new_file(rp, "sub", "notes.md").unwrap_err();
    assert_eq!(err_code(&e), "exists", "got: {e}");
}

#[test]
fn new_file_never_truncates_an_existing_file() {
    // The decisive assertion behind `create_new(true)`: the refusal must leave the
    // existing file's CONTENT alone. A plain `File::create` would have emptied it.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::write(root.path().join("precious.txt"), "do not lose me").unwrap();

    assert_eq!(err_code(&new_file(rp, "", "precious.txt").unwrap_err()), "exists");
    assert_eq!(
        fs::read_to_string(root.path().join("precious.txt")).unwrap(),
        "do not lose me"
    );
}

#[test]
fn new_file_at_the_root_uses_an_empty_rel() {
    let root = tempfile::tempdir().unwrap();
    let rel = new_file(root.path().to_str().unwrap(), "", "top.txt").unwrap();
    assert_eq!(rel, "top.txt");
    assert!(root.path().join("top.txt").is_file());
}

#[test]
fn new_file_rejects_bad_names_through_the_same_validator_as_new_folder() {
    // Not a separate name policy — the same `validate_name`. A separator is the
    // load-bearing case: it would turn "create here" into "create somewhere else".
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    for bad in ["", "  ", ".", "..", "a/b.txt", "a\\b.txt", "trailing.", "con"] {
        let e = new_file(rp, "", bad).unwrap_err();
        assert_eq!(err_code(&e), "invalid-name", "for {bad:?}: {e}");
    }
}

// ---------- reveal / open-with (SHAPE only — nothing is ever launched) ----------
//
// These stop at the containment + kind checks, deliberately: a test that got as far as
// the hand-off would have popped Explorer, or the Open-with chooser, on a CI runner.
// What CAN be wrong here is what the checks let through, and that is what is tested.

#[test]
fn reveal_and_open_with_refuse_to_escape_the_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(parent.path().join("secret.txt"), "do not touch").unwrap();
    let rp = root.to_str().unwrap();

    for e in [reveal(rp, "../secret.txt").err(), open_with(rp, "../secret.txt").err()] {
        let e = e.expect("a `..` path must be refused before any shell hand-off");
        assert!(
            matches!(err_code(&e), "outside-root" | "not-found" | "invalid-path"),
            "got: {e}"
        );
    }
}

#[test]
fn reveal_refuses_something_that_is_not_there() {
    // Refused BEFORE the spawn, so no stray Explorer window opens on a dead path.
    let root = tempfile::tempdir().unwrap();
    let e = reveal(root.path().to_str().unwrap(), "ghost.txt").unwrap_err();
    assert_eq!(err_code(&e), "not-found", "got: {e}");
}

#[test]
fn open_with_refuses_a_directory_and_a_missing_file_before_any_shell_call() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("sub")).unwrap();
    let rp = root.path().to_str().unwrap();

    assert_eq!(err_code(&open_with(rp, "sub").unwrap_err()), "is-dir");
    assert_eq!(err_code(&open_with(rp, "ghost.pdf").unwrap_err()), "not-found");
}

#[test]
fn open_with_is_refused_as_unsupported_where_the_os_has_no_chooser() {
    // Windows has the `openas` verb; macOS and Linux have no clean equivalent, so the
    // command says `unsupported` and `capabilities()` reports it — the menu HIDES the
    // item there rather than offering something that fails when clicked. This pins that
    // the two agree, so the UI can't advertise a capability the backend refuses.
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.txt"), "x").unwrap();
    let caps = capabilities();

    if caps.open_with {
        // Windows. Not driven further — succeeding would mean CI just popped a chooser.
        assert!(cfg!(windows));
    } else {
        let e = open_with(root.path().to_str().unwrap(), "a.txt").unwrap_err();
        assert_eq!(err_code(&e), "unsupported", "got: {e}");
    }
}

#[test]
fn capabilities_tell_the_truth_about_this_platform() {
    let caps = capabilities();
    // The delete mode must agree with the pure fn the confirmation dialog is keyed off,
    // or the UI promises a Recycle Bin the backend won't deliver.
    assert_eq!(caps.delete_mode, delete_mode().mode);
    assert_eq!(caps.open_with, cfg!(windows), "only Windows has an open-with chooser");
    assert!(caps.reveal, "every platform can at least open the containing folder");
    // Linux can open the folder but cannot SELECT the entry — the menu labels that
    // honestly rather than over-promising.
    assert_eq!(caps.reveal_selects, cfg!(any(windows, target_os = "macos")));
}

// ---------- #216: delete runs on a WORKER THREAD, in its own COM apartment ----------
//
// `fm_delete` used to be a synchronous Tauri command, so it ran on the main (webview)
// thread and froze the entire window for the duration of a big tree. Moving it to a worker
// is the obvious fix and the naive one is WRONG: `SHFileOperationW` is a Shell/COM API whose
// apartment requirement the main thread was silently satisfying for us (wry OleInitialize's
// it as an STA). A bare worker inherits nothing.
//
// These tests drive `delete` from threads that have NEVER touched COM — which is exactly the
// condition the worker runs under. If `ComApartment` were missing or wrong, this is where it
// would show.

#[test]
fn delete_works_from_a_fresh_thread_that_has_never_initialized_com() {
    // THE #216 TEST. The main thread's apartment is not available here, so the delete has to
    // stand up its own. A freshly spawned std thread is precisely what `fm_delete_start` uses.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap().to_string();
    fs::create_dir_all(root.path().join("tree/nested/deeper")).unwrap();
    fs::write(root.path().join("tree/a.txt"), "x").unwrap();
    fs::write(root.path().join("tree/nested/b.txt"), "x").unwrap();
    fs::write(root.path().join("tree/nested/deeper/c.txt"), "x").unwrap();

    let handle = std::thread::spawn(move || delete(&rp, "tree"));
    let recycled = handle.join().expect("the worker must not panic").unwrap();

    assert_eq!(recycled, delete_mode().mode == "recycle");
    assert!(!root.path().join("tree").exists(), "the whole tree is gone");
}

#[test]
fn repeated_deletes_on_fresh_threads_do_not_unbalance_com() {
    // The apartment guard is RAII: every enter must be matched by exactly one leave, on every
    // exit path. A leak wouldn't fail loudly — it would quietly hold an apartment reference
    // per delete for the life of the thread. Hammering it across many short-lived threads is
    // the cheap way to notice a guard that only *sometimes* runs: an unbalanced init surfaces
    // as a hang or a failure long before this loop ends.
    let root = tempfile::tempdir().unwrap();
    for i in 0..25 {
        let name = format!("f{i}.txt");
        fs::write(root.path().join(&name), "x").unwrap();
        let rp = root.path().to_str().unwrap().to_string();
        let n = name.clone();
        std::thread::spawn(move || delete(&rp, &n))
            .join()
            .expect("no worker may panic")
            .unwrap_or_else(|e| panic!("delete {i} failed: {e}"));
        assert!(!root.path().join(&name).exists());
    }
}

#[test]
fn a_delete_on_a_worker_still_enforces_containment_and_the_root_guard() {
    // Moving the call off the main thread must not have moved it out from behind the guards.
    // Same refusals, same reasons, just on a different thread.
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(parent.path().join("secret.txt"), "do not touch").unwrap();
    let rp = root.to_str().unwrap().to_string();

    for rel in ["../secret.txt", "", ".", "   "] {
        let r = rp.clone();
        let rel_owned = rel.to_string();
        let e = std::thread::spawn(move || delete(&r, &rel_owned))
            .join()
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(err_code(&e), "outside-root" | "not-found" | "invalid-path"),
            "for {rel:?}: {e}"
        );
    }
    assert_eq!(
        fs::read_to_string(parent.path().join("secret.txt")).unwrap(),
        "do not touch"
    );
}

#[test]
fn the_failure_path_surfaces_an_error_rather_than_claiming_success() {
    // A file the OS won't let go of. On Windows an open handle without FILE_SHARE_DELETE is a
    // sharing violation and the shell refuses; the delete must report that, and the file must
    // still be there afterwards — a delete that half-fails and reports success is how a user
    // loses track of what is actually on disk.
    //
    // POSIX unlink happily removes an open file, so there is no equivalent condition there;
    // the non-Windows arm of this is the containment test above (an error path that IS
    // reachable on every platform).
    if !cfg!(windows) {
        eprintln!("skipping: an open file is not un-deletable on POSIX");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap().to_string();
    let path = root.path().join("locked.txt");
    fs::write(&path, "held open").unwrap();

    // Hold it open for the duration of the delete.
    let _held = std::fs::File::open(&path).unwrap();
    let result = std::thread::spawn(move || delete(&rp, "locked.txt"))
        .join()
        .unwrap();

    // Windows *can* still recycle a file open for shared reading, so this is not a hard
    // assertion that it fails — it is an assertion that the two agree. Whatever the shell
    // decided, the reported outcome must match what is on disk.
    let still_there = path.exists();
    match (&result, still_there) {
        (Ok(_), false) => {} // deleted, and says so
        (Err(e), true) => {
            // Refused, and says so — with prose, not a bare number.
            assert_eq!(err_code(e), "io", "got: {e}");
            assert!(e.len() > 10, "the failure must be described, not just coded: {e}");
        }
        (Ok(_), true) => panic!("claimed success while the file is still on disk"),
        (Err(e), false) => panic!("reported failure ({e}) but the file is gone"),
    }
}

#[test]
fn shell_failure_codes_are_translated_into_something_a_human_can_act_on() {
    // SHFileOperationW's codes are its OWN — not GetLastError, not HRESULT. A bare number in a
    // toast is useless; these are the ones a user can actually do something about.
    assert!(describe_delete_failure(0x20).contains("open in another program"));
    assert!(describe_delete_failure(0x05).contains("permission"));
    assert!(describe_delete_failure(0x02).contains("no longer exists"));
    assert!(describe_delete_failure(0x85).contains("disk is full"));
    assert!(describe_delete_failure(0x10000).contains("Recycle Bin"));

    // An unknown code keeps the raw number: a bug report with a code beats prose that
    // invented a cause.
    let unknown = describe_delete_failure(0x1234);
    assert!(unknown.contains("0x1234"), "got: {unknown}");
}

// ---------- the completion-event contract (#216) ----------
//
// The delete now finishes on a worker thread and reports by event, so the *payload* is the
// interface — the pane has nothing else to go on. `fm_delete_start` itself needs a live
// AppHandle to emit and cannot be called from here, which is exactly why the payload is
// built by `delete_event`: the contract stays reachable.

#[test]
fn a_completed_delete_reports_what_actually_happened_to_the_tree() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    let dir = root.join("tree");
    fs::create_dir_all(dir.join("nested")).unwrap();
    fs::write(dir.join("nested/a.txt"), b"a").unwrap();
    fs::write(dir.join("b.txt"), b"b").unwrap();

    // Exactly what the worker does: run the delete, turn the outcome into the event.
    let outcome = delete(root.to_str().unwrap(), "tree");
    let event = delete_event(7, "tree".into(), outcome);
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();

    assert_eq!(json["id"], 7, "the id the pane will match its busy row against");
    assert_eq!(json["rel"], "tree", "and the path, so it can match without remembering");
    assert!(json.get("error").is_none(), "a success must carry NO error: the pane branches on it");
    assert!(json["recycled"].is_boolean(), "…and must say whether it is recoverable");

    // The event claims success — so the tree, recursively, must be gone. An event that says
    // "deleted" over a directory still on disk is the one failure the pane cannot detect.
    assert!(!dir.exists(), "the whole tree, not just the entries we could see");
    assert!(root.exists(), "and nothing above it");
}

#[test]
fn a_failed_delete_reports_an_error_and_never_a_recycled_flag() {
    // The other half of the contract: `error` present, `recycled` ABSENT. If both were
    // absent the pane would render a silent success over a file that is still there.
    let td = tempfile::tempdir().unwrap();
    let outcome = delete(td.path().to_str().unwrap(), "does-not-exist");
    assert!(outcome.is_err(), "deleting a phantom must fail, not no-op into a success");
    let json = serde_json::to_value(delete_event(9, "does-not-exist".into(), outcome)).unwrap();

    assert!(json["error"].is_string());
    assert!(json.get("recycled").is_none(), "a failure must not also claim a Recycle Bin outcome");
    assert_eq!(json["id"], 9);
}

#[test]
fn the_event_carries_the_path_so_a_pane_that_navigated_away_can_still_place_it() {
    // The pane is free to browse elsewhere WHILE a delete runs (not blocking navigation is
    // half the point of #216). When the event lands it must be able to answer "was that in
    // the directory I'm looking at now?" — which it does from `rel`, not from memory of
    // what it was doing when it started.
    let json = serde_json::to_value(delete_event(1, "deep/nested/tree".into(), Ok(true))).unwrap();
    assert_eq!(json["rel"], "deep/nested/tree", "the full rel, not a basename");
    assert_eq!(json["recycled"], true);
}
