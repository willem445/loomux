//! Integration tests for the file-editor backend (issue #174).
//!
//! Must be an integration test, not a unit test: linking `loomux_lib` pulls in
//! the full UI dependency graph, and on Windows the resulting test exe only
//! loads because build.rs embeds the comctl32-v6 manifest via `-tests`-scoped
//! link args (CLAUDE.md constraint #4). These drive the public `fileedit::*`
//! helpers the Tauri commands wrap, so no Tauri runtime is needed.

use loomux_lib::fileedit::{
    content_hash, list_dir, list_files, read_file, replace, search, search_planned, write_file,
    SearchOpts,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn opts(case_insensitive: bool, whole_word: bool) -> SearchOpts {
    SearchOpts {
        case_insensitive,
        whole_word,
        max_results: 0,
        include_ignored: false,
    }
}

/// Search opts with the gitignore toggle set — the only knob the #207 tests vary.
fn opts_ig(include_ignored: bool) -> SearchOpts {
    SearchOpts {
        case_insensitive: false,
        whole_word: false,
        max_results: 0,
        include_ignored,
    }
}

/// Collect a `search_planned` run into a flat match list + the set of files hit.
fn planned(
    root: &str,
    query: &str,
    opts: SearchOpts,
    cancelled: &dyn Fn() -> bool,
) -> (Vec<loomux_lib::fileedit::Match>, BTreeSet<String>) {
    let mut out = Vec::new();
    search_planned(root, query, opts, cancelled, &mut |b| out.extend(b)).unwrap();
    let files = out.iter().map(|m| m.rel.clone()).collect();
    (out, files)
}

/// True if a usable `git` is on PATH (the gitignore test skips otherwise, like
/// the symlink tests skip when the platform forbids symlinks).
fn git_available() -> bool {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

/// Init a real git repo at `root` (needed so `git ls-files` classifies files);
/// isolates config so a developer's global gitignore/hooks can't skew the test.
fn git_init(root: &Path) {
    let run = |args: &[&str]| {
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(root).args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        assert!(cmd.output().unwrap().status.success(), "git {args:?} failed");
    };
    run(&["init"]);
    run(&["config", "core.excludesFile", ""]);
}

/// Stage `path` into the repo index (no identity needed — `add`, not `commit`),
/// so it's a genuinely *tracked* file enumerated via `git ls-files --cached`.
fn git_add(root: &Path, path: &str) {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(root).args(["add", path]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    assert!(cmd.output().unwrap().status.success(), "git add {path} failed");
}

/// Best-effort symlink creation; returns false if the platform refuses (Windows
/// without the privilege), letting a test skip its symlink assertions rather
/// than fail on an environment limitation.
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

fn err_code(msg: &str) -> &str {
    msg.split(':').next().unwrap_or("")
}

// ---------- path safety ----------

#[test]
fn rejects_parent_dir_escape() {
    let root = tempfile::tempdir().unwrap();
    let r = read_file(root.path().to_str().unwrap(), "../secret.txt");
    let e = r.unwrap_err();
    // A `..` that climbs out is either normalized-and-caught as outside-root or
    // rejected earlier; either way it must not read the parent.
    assert!(
        matches!(err_code(&e), "outside-root" | "not-found" | "invalid-path"),
        "got: {e}"
    );
}

#[test]
fn rejects_absolute_rel() {
    let root = tempfile::tempdir().unwrap();
    let abs = if cfg!(windows) { "C:\\Windows\\win.ini" } else { "/etc/passwd" };
    let e = list_dir(root.path().to_str().unwrap(), abs).unwrap_err();
    assert_eq!(err_code(&e), "invalid-path", "got: {e}");
}

#[test]
fn accepts_nested_legit_path() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("a/b")).unwrap();
    fs::write(root.path().join("a/b/c.txt"), "hi").unwrap();
    let fr = read_file(root.path().to_str().unwrap(), "a/b/c.txt").unwrap();
    assert_eq!(fr.content, "hi");
}

#[test]
fn refuses_to_read_through_symlinked_dir() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "TOP SECRET").unwrap();
    let link = root.path().join("link");
    if !try_symlink(outside.path(), &link, true) {
        eprintln!("skipping: symlink creation not permitted here");
        return;
    }
    // Lexically `link/secret.txt` is inside root, but the component is a symlink
    // to another dir — the walk must refuse it.
    let e = read_file(root.path().to_str().unwrap(), "link/secret.txt").unwrap_err();
    assert_eq!(err_code(&e), "symlink", "got: {e}");
}

// ---------- read ----------

#[test]
fn reads_utf8_and_hash_is_stable() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("f.txt"), "héllo").unwrap();
    let a = read_file(root.path().to_str().unwrap(), "f.txt").unwrap();
    let b = read_file(root.path().to_str().unwrap(), "f.txt").unwrap();
    assert_eq!(a.content, "héllo");
    assert_eq!(a.hash, b.hash, "hash must be deterministic");
    assert_eq!(a.hash, content_hash("héllo".as_bytes()));
    assert!(!a.truncated);
}

#[test]
fn rejects_binary_file() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("bin"), [0x00, 0x01, 0x02, 0x03]).unwrap();
    let e = read_file(root.path().to_str().unwrap(), "bin").unwrap_err();
    assert_eq!(err_code(&e), "binary", "got: {e}");
}

#[test]
fn rejects_oversize_file() {
    let root = tempfile::tempdir().unwrap();
    // Just over the 2 MiB read cap, all printable so it isn't caught as binary.
    let big = vec![b'a'; 2 * 1024 * 1024 + 16];
    fs::write(root.path().join("big.txt"), &big).unwrap();
    let e = read_file(root.path().to_str().unwrap(), "big.txt").unwrap_err();
    assert_eq!(err_code(&e), "too-large", "got: {e}");
}

// ---------- list ----------

#[test]
fn lists_entries_dirs_first_with_flags() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("zdir")).unwrap();
    fs::write(root.path().join("afile.txt"), "x").unwrap();
    fs::write(root.path().join(".hidden"), "y").unwrap();
    let entries = list_dir(root.path().to_str().unwrap(), "").unwrap();
    // Dirs sort before files; hidden files are present (not filtered by the tree).
    assert_eq!(entries[0].name, "zdir");
    assert!(entries[0].is_dir);
    assert!(entries.iter().any(|e| e.name == ".hidden"));
    let f = entries.iter().find(|e| e.name == "afile.txt").unwrap();
    assert!(!f.is_dir);
    assert_eq!(f.size, 1);
}

#[test]
fn list_flags_symlink_and_does_not_expand_it() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("realdir")).unwrap();
    let link = root.path().join("linkdir");
    if !try_symlink(&root.path().join("realdir"), &link, true) {
        eprintln!("skipping: symlink creation not permitted here");
        return;
    }
    let entries = list_dir(root.path().to_str().unwrap(), "").unwrap();
    let l = entries.iter().find(|e| e.name == "linkdir").unwrap();
    assert!(l.is_symlink, "symlink must be flagged");
    assert!(!l.is_dir, "symlink must not be presented as an expandable dir");
    // And it must not be traversable.
    let e = list_dir(root.path().to_str().unwrap(), "linkdir").unwrap_err();
    assert_eq!(err_code(&e), "symlink", "got: {e}");
}

// ---------- write + conflict ----------

#[test]
fn writes_create_and_overwrite_atomically() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    // Create.
    let w = write_file(rp, "new.txt", "first", None).unwrap();
    assert_eq!(fs::read_to_string(root.path().join("new.txt")).unwrap(), "first");
    // Overwrite with the correct expected hash.
    let w2 = write_file(rp, "new.txt", "second", Some(w.hash.clone())).unwrap();
    assert_eq!(fs::read_to_string(root.path().join("new.txt")).unwrap(), "second");
    assert_eq!(w2.hash, content_hash(b"second"));
    // No stray temp files left behind.
    let leftover = fs::read_dir(root.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
    assert!(!leftover, "atomic write must not leave temp files");
}

#[test]
fn write_into_missing_dir_errors_and_leaves_no_orphan_temp() {
    // The write path can't create the temp sibling (its parent dir is absent), so
    // atomic_write fails — and must not leave a `.tmp` orphan behind (finding #4).
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    let e = write_file(rp, "nope/deep/f.txt", "data", None).unwrap_err();
    assert_eq!(err_code(&e), "io", "got: {e}");
    let orphan = fs::read_dir(root.path())
        .unwrap()
        .filter_map(|d| d.ok())
        .any(|d| d.file_name().to_string_lossy().ends_with(".tmp"));
    assert!(!orphan, "a failed write must not leave a temp file");
}

#[test]
fn write_with_stale_hash_is_conflict_and_leaves_file_untouched() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    write_file(rp, "f.txt", "original", None).unwrap();
    // A hash that doesn't match what's on disk simulates a concurrent edit.
    let e = write_file(rp, "f.txt", "clobber", Some("deadbeefdeadbeef".into())).unwrap_err();
    assert_eq!(err_code(&e), "conflict", "got: {e}");
    assert_eq!(
        fs::read_to_string(root.path().join("f.txt")).unwrap(),
        "original",
        "conflicting write must not touch the file"
    );
}

// ---------- search ----------

#[test]
fn search_literal_case_and_whole_word() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::write(root.path().join("a.txt"), "Foo foobar\nbaz FOO\n").unwrap();

    // Case-sensitive literal: matches "Foo" and the "Foo" inside "foobar"? No —
    // "foobar" is lowercase. "Foo" appears once on line 1.
    let cs = search(rp, "Foo", opts(false, false)).unwrap();
    assert_eq!(cs.matches.len(), 1);
    assert_eq!(cs.matches[0].line, 1);
    assert_eq!(cs.matches[0].col, 1);

    // Case-insensitive: "Foo" (l1), "foo" in "foobar" (l1), "FOO" (l2) = 3.
    let ci = search(rp, "foo", opts(true, false)).unwrap();
    assert_eq!(ci.matches.len(), 3, "got {:?}", ci.matches);

    // Whole-word case-insensitive: excludes the "foo" inside "foobar".
    let ww = search(rp, "foo", opts(true, true)).unwrap();
    assert_eq!(ww.matches.len(), 2, "got {:?}", ww.matches);
}

#[test]
fn search_skips_excludes_and_empty_query_guarded() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir_all(root.path().join("node_modules")).unwrap();
    fs::write(root.path().join("node_modules/dep.js"), "needle").unwrap();
    fs::create_dir_all(root.path().join(".git")).unwrap();
    fs::write(root.path().join(".git/config"), "needle").unwrap();
    fs::write(root.path().join("src.txt"), "needle").unwrap();

    let out = search(rp, "needle", opts(false, false)).unwrap();
    assert_eq!(out.matches.len(), 1, "excluded dirs must be skipped");
    assert_eq!(out.matches[0].rel, "src.txt");

    let e = search(rp, "", opts(false, false)).unwrap_err();
    assert_eq!(err_code(&e), "empty-query", "got: {e}");
}

#[test]
fn search_flags_truncation_at_cap() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    let mut body = String::new();
    for _ in 0..20 {
        body.push_str("x\n");
    }
    fs::write(root.path().join("many.txt"), &body).unwrap();
    let capped = SearchOpts {
        case_insensitive: false,
        whole_word: false,
        max_results: 5,
        include_ignored: false,
    };
    let out = search(rp, "x", capped).unwrap();
    assert_eq!(out.matches.len(), 5);
    assert!(out.truncated, "hitting the cap must be surfaced, not silent");
}

// ---------- streaming search: gitignore + cancellation (issue #207) ----------

#[test]
fn search_respects_gitignore_by_default_and_toggle_includes_ignored() {
    if !git_available() {
        eprintln!("skipping: git not on PATH");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    git_init(root.path());
    fs::write(root.path().join(".gitignore"), "ignored.txt\nbuilddir/\n").unwrap();
    // A TRACKED (staged) file — enumerated via `--cached`. Staging it is what
    // makes this test actually cover tracked files: drop `--cached` from the
    // ls-files call and the `tracked.txt` assertion below fails.
    fs::write(root.path().join("tracked.txt"), "needle here").unwrap();
    git_add(root.path(), "tracked.txt");
    // An UNTRACKED-but-unignored file — enumerated via `--others --exclude-standard`.
    fs::write(root.path().join("untracked.txt"), "needle here").unwrap();
    // A gitignored file and a gitignored directory — skipped by default.
    fs::write(root.path().join("ignored.txt"), "needle here").unwrap();
    fs::create_dir_all(root.path().join("builddir")).unwrap();
    fs::write(root.path().join("builddir/gen.txt"), "needle here").unwrap();

    // Default (include_ignored=false): tracked + untracked-unignored are searched,
    // the ignored file + dir are skipped. This is the ignored-by-default guarantee.
    let (_, files) = planned(rp, "needle", opts_ig(false), &|| false);
    assert!(
        files.contains("tracked.txt"),
        "tracked (staged) file must be searched — pins `--cached`, got {files:?}"
    );
    assert!(
        files.contains("untracked.txt"),
        "untracked-unignored file must be searched — pins `--others`, got {files:?}"
    );
    assert!(!files.contains("ignored.txt"), "gitignored file must be skipped");
    assert!(
        !files.iter().any(|f| f.starts_with("builddir")),
        "gitignored dir must be skipped, got {files:?}"
    );

    // Toggle on: the full walk now reaches the ignored file and directory.
    let (_, all) = planned(rp, "needle", opts_ig(true), &|| false);
    assert!(all.contains("ignored.txt"), "toggle must include the ignored file");
    assert!(
        all.iter().any(|f| f == "builddir/gen.txt"),
        "toggle must include the ignored dir, got {all:?}"
    );
}

#[test]
fn search_cancellation_stops_the_walk_before_it_reads() {
    // A cancel flag that is already set before the search starts must stop the
    // walk at the first check — no files are scanned, so no results land. This is
    // the backend half of the "a cancelled search never yields results" contract
    // (the frontend half — dropping late batches by id — is in searchsession).
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    for i in 0..40 {
        fs::write(root.path().join(format!("f{i}.txt")), "needle\n").unwrap();
    }
    let cancel = AtomicBool::new(true); // pre-cancelled
    let (matches, _) = planned(rp, "needle", opts_ig(true), &|| cancel.load(Ordering::Relaxed));
    assert!(
        matches.is_empty(),
        "a pre-cancelled search must scan nothing, got {} matches",
        matches.len()
    );
}

#[test]
fn search_cancellation_stops_partway_through_the_walk() {
    // Complements the pre-cancelled test: the flag flips true after a handful of
    // between-files polls, so the walk must stop well before all 40 files are
    // scanned. This pins that cancellation is checked *between files* — the
    // property that makes a superseding keystroke stop the old walk promptly.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    for i in 0..40 {
        fs::write(root.path().join(format!("f{i:02}.txt")), "needle\n").unwrap();
    }
    let polls = AtomicUsize::new(0);
    let (matches, _) = planned(rp, "needle", opts_ig(true), &|| {
        polls.fetch_add(1, Ordering::Relaxed) >= 8
    });
    assert!(!matches.is_empty(), "some files should scan before the cancel");
    assert!(
        matches.len() < 40,
        "cancel mid-walk must stop early, got {} of 40",
        matches.len()
    );
}

// ---------- replace ----------

#[test]
fn replace_applies_only_confirmed_files_atomically() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::write(root.path().join("a.txt"), "alpha alpha").unwrap();
    fs::write(root.path().join("b.txt"), "alpha here").unwrap();

    // Only a.txt is confirmed; b.txt must stay untouched even though it matches.
    let res = replace(
        rp,
        "alpha",
        "beta",
        vec!["a.txt".into()],
        opts(false, false),
    )
    .unwrap();
    assert_eq!(res.changed.len(), 1);
    assert_eq!(res.changed[0].replacements, 2);
    assert_eq!(fs::read_to_string(root.path().join("a.txt")).unwrap(), "beta beta");
    assert_eq!(
        fs::read_to_string(root.path().join("b.txt")).unwrap(),
        "alpha here",
        "unconfirmed file must not be modified"
    );
}

#[test]
fn replace_skips_bad_file_without_partial_write() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::write(root.path().join("good.txt"), "cat cat").unwrap();
    // A confirmed file that no longer matches at apply time (changed since
    // preview) plus one outside the root: neither corrupts the batch.
    fs::write(root.path().join("nomatch.txt"), "dog").unwrap();

    let res = replace(
        rp,
        "cat",
        "lion",
        vec!["good.txt".into(), "nomatch.txt".into(), "../escape.txt".into()],
        opts(false, false),
    )
    .unwrap();

    assert_eq!(res.changed.len(), 1);
    assert_eq!(res.changed[0].rel, "good.txt");
    assert_eq!(fs::read_to_string(root.path().join("good.txt")).unwrap(), "lion lion");
    // nomatch.txt (no-match) and ../escape.txt (path-rejected) both skipped.
    assert_eq!(res.skipped.len(), 2, "got {:?}", res.skipped.iter().map(|s| &s.rel).collect::<Vec<_>>());
    assert_eq!(fs::read_to_string(root.path().join("nomatch.txt")).unwrap(), "dog");
}

// ---------- file-NAME enumeration (issue #214) ----------

/// Collect a `list_files` run into a flat, sorted path list + the truncated flag.
fn listed(root: &str, include_ignored: bool, cancelled: &dyn Fn() -> bool) -> (Vec<String>, bool) {
    let mut out: Vec<String> = Vec::new();
    let truncated = list_files(root, include_ignored, cancelled, &mut |b| out.extend(b)).unwrap();
    out.sort();
    (out, truncated)
}

#[test]
fn list_files_enumerates_paths_without_reading_them() {
    // The whole point of the name search: it must list files the CONTENT search
    // refuses to open — a binary blob and an over-cap file are perfectly valid
    // things to jump to by name. If this ever starts reading/sniffing, they'd
    // vanish from the list and this fails.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir_all(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/pane.ts"), "x").unwrap();
    fs::write(root.path().join("binary.bin"), [0u8, 1, 2, 0, 3]).unwrap();
    // Over the content search's 1 MiB per-file bound, so `search` skips it entirely.
    fs::write(root.path().join("huge.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();

    let (files, truncated) = listed(rp, true, &|| false);
    assert!(!truncated);
    assert_eq!(files, vec!["binary.bin", "huge.txt", "src/pane.ts"]);
}

#[test]
fn list_files_uses_forward_slashes_and_lists_no_directories() {
    // Paths are the frontend's `rel` convention (forward slashes, root-relative),
    // and only FILES are listed — a directory in the list would be un-openable.
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    fs::create_dir_all(root.path().join("a/b/c")).unwrap();
    fs::write(root.path().join("a/b/c/deep.ts"), "x").unwrap();

    let (files, _) = listed(rp, true, &|| false);
    assert_eq!(files, vec!["a/b/c/deep.ts"], "dirs must not be listed; slashes must be /");
}

#[test]
fn list_files_respects_gitignore_by_default_and_the_toggle_includes_ignored() {
    // The toggle must mean the SAME thing it means for the content search (#207) —
    // both go through plan_enumeration, and this pins that they can't drift.
    if !git_available() {
        eprintln!("skipping: git not on PATH");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    git_init(root.path());
    fs::write(root.path().join(".gitignore"), "ignored.txt\nbuilddir/\n").unwrap();
    fs::write(root.path().join("tracked.txt"), "x").unwrap();
    git_add(root.path(), "tracked.txt");
    fs::write(root.path().join("untracked.txt"), "x").unwrap();
    fs::write(root.path().join("ignored.txt"), "x").unwrap();
    fs::create_dir_all(root.path().join("builddir")).unwrap();
    fs::write(root.path().join("builddir/gen.txt"), "x").unwrap();

    let (default, _) = listed(rp, false, &|| false);
    assert!(default.contains(&"tracked.txt".to_string()), "got {default:?}");
    assert!(default.contains(&"untracked.txt".to_string()), "got {default:?}");
    assert!(!default.contains(&"ignored.txt".to_string()), "gitignored file must be skipped");
    assert!(
        !default.iter().any(|f| f.starts_with("builddir")),
        "gitignored dir must be skipped, got {default:?}"
    );

    let (all, _) = listed(rp, true, &|| false);
    assert!(all.contains(&"ignored.txt".to_string()), "toggle must include the ignored file");
    assert!(
        all.contains(&"builddir/gen.txt".to_string()),
        "toggle must include the ignored dir, got {all:?}"
    );
}

#[test]
fn list_files_never_lists_the_git_directory() {
    // .git is metadata, not source — the walk skips it in EVERY mode, including
    // include_ignored (which drops the heuristic excludes but not this one).
    if !git_available() {
        eprintln!("skipping: git not on PATH");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    git_init(root.path());
    fs::write(root.path().join("real.txt"), "x").unwrap();

    let (all, _) = listed(rp, true, &|| false);
    assert!(all.contains(&"real.txt".to_string()));
    assert!(
        !all.iter().any(|f| f.starts_with(".git/")),
        "the .git dir must never be enumerated, got {all:?}"
    );
}

#[test]
fn list_files_does_not_follow_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "x").unwrap();
    if !try_symlink(outside.path(), &root.path().join("link"), true) {
        eprintln!("skipping: symlinks not permitted here");
        return;
    }
    fs::write(root.path().join("real.txt"), "x").unwrap();

    let (all, _) = listed(root.path().to_str().unwrap(), true, &|| false);
    assert_eq!(all, vec!["real.txt"], "a symlinked dir must not be walked into");
}

#[test]
fn list_files_is_cancellable_and_reports_truncation_rather_than_cutting_silently() {
    let root = tempfile::tempdir().unwrap();
    let rp = root.path().to_str().unwrap();
    for i in 0..40 {
        fs::write(root.path().join(format!("f{i:02}.txt")), "x").unwrap();
    }
    // Pre-cancelled: the enumeration must stop at the first check, listing nothing.
    let (none, _) = listed(rp, true, &|| true);
    assert!(none.is_empty(), "a pre-cancelled enumeration must list nothing, got {}", none.len());

    // Cancelling partway through the git-list arm is checked per path; the walk arm
    // is checked per directory, so with one directory the flat 40 files come back
    // whole. What must NOT happen either way is a silent cut — a real ceiling hit
    // sets `truncated`, and here nothing is truncated.
    let (all, truncated) = listed(rp, true, &|| false);
    assert_eq!(all.len(), 40);
    assert!(!truncated, "40 files is nowhere near the ceiling");
}
