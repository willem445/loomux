//! The two backend facts the workflow pane's CREATE path is built on (#222 v2, rev-15 F2).
//!
//! The pane cannot test its own IPC sequencing without a DOM, but it can pin the properties it
//! relies on — and it must, because the bug they fix was data loss: the pane wrote a scaffolded
//! `.loomux/workflow.yml` with a null expected hash, which `write_file` reads as *write
//! unconditionally*. A workflow that appeared while the pane sat on its start surface (an agent
//! wrote one, a `git pull` brought one in) was destroyed, and the pane reported "Saved".
//!
//! So the pane now CLAIMS the path with `fm_new_file` first. These tests are the two halves of
//! why that works, stated where the behaviour actually lives — so that a future change to either
//! backend fails here rather than silently re-arming the same data loss in the frontend.

use loomux_lib::fileedit::{read_file, write_file};
use loomux_lib::filemgr::new_file;

/// A scratch repo, cleaned up on drop.
struct Repo(std::path::PathBuf);

impl Repo {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("wf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".loomux")).unwrap();
        Repo(dir)
    }
    fn root(&self) -> String {
        self.0.to_string_lossy().to_string()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// THE ROOT-MISMATCH THEORY, TESTED AND KILLED (#222 live bug).
///
/// The pane's live symptoms had an attractive explanation: that the read probe resolved the repo
/// root one way and the write resolved it another, so the workflow "didn't exist" for the read
/// (hence "Can't read .loomux/workflow.yml" over a file that was plainly there) while the write
/// still landed on the real one (hence the overwrite). One cause, both symptoms — and wrong.
///
/// This is the experiment that says so, shaped like the live app rather than like a test: the
/// process cwd is pointed at a DECOY repo of identical layout, and the root arrives in the forms
/// Windows actually hands over — backslashes, trailing separator — while `rel` arrives in the
/// frontend's forward-slash form (`.loomux/workflow.yml`, joined in TypeScript). If resolution
/// were asymmetric, or fell back to the cwd for either call, the read returns the decoy's bytes or
/// the write lands in the decoy. Neither happens: read and write resolve the same absolute file,
/// every time, from every spelling of the root.
///
/// The real cause was in the stylesheet — `display: flex` out-ranks the `hidden` attribute, so the
/// pane drew all three of its mutually exclusive surfaces at once (test/hiddenrule.test.ts). The
/// "Can't read" banner was never a failed read; it was a surface that had never been hidden.
#[test]
fn read_and_write_resolve_the_same_file_from_a_cwd_that_is_somewhere_else() {
    let repo = Repo::new("root-truth");
    let decoy = Repo::new("root-decoy");
    let theirs = "version: 1\nname: the-real-workflow\n";
    std::fs::write(repo.0.join(".loomux/workflow.yml"), theirs).unwrap();
    std::fs::write(decoy.0.join(".loomux/workflow.yml"), "version: 1\nname: DECOY\n").unwrap();

    // The live app's process cwd is neither the pane's root nor the tab's repo. Reproduce that.
    // (Safe to set process-wide: every other test in this file addresses its files absolutely.)
    std::env::set_current_dir(&decoy.0).unwrap();

    // The spellings the live app can hand over. `rel` stays forward-slashed throughout — that is
    // what `WORKFLOW_FILE` is in TypeScript, and it never gets converted on the way down.
    let with_backslashes = repo.root().replace('/', "\\");
    let trailing = format!("{}\\", with_backslashes.trim_end_matches('\\'));
    for root in [repo.root(), with_backslashes, trailing] {
        let read = read_file(&root, ".loomux/workflow.yml")
            .unwrap_or_else(|e| panic!("the file is THERE — a live root must read it: {root} => {e}"));
        assert_eq!(
            read.content, theirs,
            "read resolved somewhere else — the decoy, or nowhere ({root})"
        );

        // And the write goes back to the same file: guarded by the hash the read just returned, so
        // if it were resolving a DIFFERENT path this would either create a stray file in the decoy
        // or fail the hash check outright.
        let mine = format!("version: 1\nname: written-via-{}\n", root.len());
        write_file(&root, ".loomux/workflow.yml", &mine, Some(read.hash)).unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.0.join(".loomux/workflow.yml")).unwrap(),
            mine,
            "the write landed somewhere other than the file the read came from ({root})"
        );
        std::fs::write(repo.0.join(".loomux/workflow.yml"), theirs).unwrap(); // reset for the next spelling
    }

    // The decoy is untouched: nothing ever resolved through the cwd.
    assert_eq!(
        std::fs::read_to_string(decoy.0.join(".loomux/workflow.yml")).unwrap(),
        "version: 1\nname: DECOY\n"
    );
}

/// WHY THE OVERWRITE WAS NOT THE BACKEND'S FAULT, AND COULD NOT HAVE BEEN CAUGHT HERE.
///
/// The human pressed "Create workflow" over a workflow the pane had already read and shown, and
/// the scaffold replaced it. It is tempting to look for the missing refusal down here. There isn't
/// one to add: the pane held the file's real hash (it had just read it), so the create arrived as
/// an ordinary guarded write, the hash MATCHED — nothing else had touched the file — and honouring
/// it is exactly the contract every other save depends on.
///
/// So this test pins the fact rather than a fix: a guarded write whose hash matches overwrites, by
/// design. The refusal has to live above it, in the rule that decides whether a create may happen
/// at all (`createAllowed`, workflowpane.ts) — which is now the same rule that decides whether the
/// button is on screen, so the two cannot disagree again.
#[test]
fn a_guarded_write_whose_hash_matches_overwrites_by_design() {
    let repo = Repo::new("guarded");
    let theirs = "version: 1\nname: someone-elses-work\n";
    std::fs::write(repo.0.join(".loomux/workflow.yml"), theirs).unwrap();

    // Exactly what the pane held when the button was pressed: the file's own, current hash.
    let read = read_file(&repo.root(), ".loomux/workflow.yml").unwrap();
    let scaffold = "version: 1\nname: default\n";
    write_file(&repo.root(), ".loomux/workflow.yml", scaffold, Some(read.hash))
        .expect("the hash matches, so this is an ordinary save and MUST succeed");

    assert_eq!(
        std::fs::read_to_string(repo.0.join(".loomux/workflow.yml")).unwrap(),
        scaffold,
        "their workflow is gone — and the backend did nothing wrong. The button should never have \
         been pressable."
    );
}

/// HALF ONE — why the old code lost data: a write with no expected hash clobbers, silently.
///
/// This is not a bug in `write_file`; an unconditional write is exactly what `None` asks for, and
/// the conflict dialog's "Overwrite" needs it. The bug was the PANE asking for it while believing
/// (from a read that happened minutes earlier) that there was no file to lose.
#[test]
fn a_write_with_no_expected_hash_overwrites_whatever_is_there() {
    let repo = Repo::new("clobber");
    let theirs = "version: 1\nname: someone-elses-work\n";
    std::fs::write(repo.0.join(".loomux/workflow.yml"), theirs).unwrap();

    write_file(&repo.root(), ".loomux/workflow.yml", "version: 1\nname: mine\n", None).unwrap();

    let on_disk = std::fs::read_to_string(repo.0.join(".loomux/workflow.yml")).unwrap();
    assert!(
        !on_disk.contains("someone-elses-work"),
        "a null expected hash means 'write unconditionally' — which is precisely why a CREATE \
         must never use one"
    );
}

/// HALF TWO — why claiming the path fixes it: `new_file` is `create_new(true)`, so "create, but
/// only if it isn't there" is ATOMIC. There is no window between the check and the create for a
/// workflow to arrive in, and a file that IS there keeps its contents.
#[test]
fn claiming_the_path_refuses_an_existing_file_without_truncating_it() {
    let repo = Repo::new("claim");

    // Nothing there yet: the claim succeeds, and hands back an empty file to write into.
    new_file(&repo.root(), ".loomux", "workflow.yml").expect("the first claim must succeed");
    let claimed = read_file(&repo.root(), ".loomux/workflow.yml").unwrap();
    assert_eq!(claimed.content, "", "a claim creates an EMPTY file");

    // Somebody else's workflow lands in it (the agent / git-pull case).
    let theirs = "version: 1\nname: someone-elses-work\n";
    std::fs::write(repo.0.join(".loomux/workflow.yml"), theirs).unwrap();

    // The second claim is refused — and, crucially, refused WITHOUT truncating.
    let err = new_file(&repo.root(), ".loomux", "workflow.yml").expect_err("must refuse to clobber");
    assert!(err.starts_with("exists:"), "the frontend branches on this code: {err}");
    assert_eq!(
        std::fs::read_to_string(repo.0.join(".loomux/workflow.yml")).unwrap(),
        theirs,
        "their workflow must be exactly as they left it"
    );

    // And the hash the claim yields is what makes the rest of the save an ordinary guarded write:
    // the file moved after we claimed it, so writing against the claimed hash is a CONFLICT — the
    // human gets asked, instead of the file getting overwritten.
    let err = write_file(
        &repo.root(),
        ".loomux/workflow.yml",
        "version: 1\nname: mine\n",
        Some(claimed.hash),
    )
    .expect_err("the file changed since we claimed it");
    assert!(err.starts_with("conflict:"), "{err}");
    assert_eq!(
        std::fs::read_to_string(repo.0.join(".loomux/workflow.yml")).unwrap(),
        theirs,
        "still theirs — nothing was overwritten"
    );
}
