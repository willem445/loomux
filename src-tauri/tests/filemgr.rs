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
    delete, delete_mode, list, new_file, new_folder, open_default, rename, validate_name,
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
