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
