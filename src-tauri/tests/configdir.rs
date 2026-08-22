//! A repo's committed config dir: `.orrerix/` preferred, `.loomux/` still read
//! (#1153 phase 4 — `doc/design/rebrand-filesystem.md`).
//!
//! The rule under test is one sentence — **the legacy spelling wins only when
//! it is the only one there, and is never renamed** — but it has to hold at
//! every surface a repo's workflow reaches: whether the file is found at all,
//! what gets parsed out of it, and which path the app then NAMES back to a
//! human or an agent. A resolver that reads the right file while reporting the
//! wrong path sends someone to edit a file that does not exist.
//!
//! An integration test (not inline `#[cfg(test)]`) per repo constraint #4: a
//! unit-test binary linking the full lib misses the comctl32-v6 manifest
//! `build.rs` only embeds for integration-test targets.

use loomux_lib::orchestration::workflow;

/// A scratch repo dir, cleaned up on drop.
struct Repo(std::path::PathBuf);

impl Repo {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("cfgdir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Repo(dir)
    }
    fn root(&self) -> String {
        self.0.to_string_lossy().to_string()
    }
    /// Write `yaml` at the repo-relative path `rel`, creating its directory.
    fn write(&self, rel: &str, yaml: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, yaml).unwrap();
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A minimal workflow whose `name:` identifies which file was parsed.
fn wf(name: &str) -> String {
    format!("version: 1\nname: {name}\nblocks:\n  - id: worker\n    kind: worker\n    cli: claude\n")
}

#[test]
fn a_repo_on_the_legacy_config_dir_is_found_parsed_and_named() {
    // The deprecation contract. A `.loomux/workflow.yml` committed on somebody's
    // main branch keeps working with no action from them — and every surface
    // agrees: it exists, it parses, and the path reported back is the one that
    // was actually read.
    let repo = Repo::new("legacy");
    repo.write(workflow::LEGACY_WORKFLOW_PATH, &wf("legacy-roster"));

    assert!(workflow::workflow_file_exists(&repo.root()), "the legacy file must be discovered");
    assert_eq!(
        workflow::load_workflow(&repo.root()).unwrap().unwrap().name,
        "legacy-roster",
        "and parsed"
    );
    assert_eq!(
        workflow::workflow_path(&repo.root()),
        workflow::LEGACY_WORKFLOW_PATH,
        "the path reported back must be the file that was read, not the preferred spelling"
    );
    assert!(workflow::workflow_file(&repo.root()).is_file());
}

#[test]
fn a_repo_on_the_preferred_config_dir_is_found_parsed_and_named() {
    let repo = Repo::new("preferred");
    repo.write(workflow::WORKFLOW_PATH, &wf("new-roster"));

    assert!(workflow::workflow_file_exists(&repo.root()));
    assert_eq!(workflow::load_workflow(&repo.root()).unwrap().unwrap().name, "new-roster");
    assert_eq!(workflow::workflow_path(&repo.root()), workflow::WORKFLOW_PATH);
}

#[test]
fn with_both_present_the_preferred_file_wins_and_the_legacy_one_is_left_alone() {
    // A repo mid-migration has both. Reading the legacy one there would mean
    // that ADDING `.orrerix/workflow.yml` has no effect until the author also
    // deletes `.loomux/` — the "why is my edit being ignored" trap.
    let repo = Repo::new("both");
    repo.write(workflow::LEGACY_WORKFLOW_PATH, &wf("stale-roster"));
    repo.write(workflow::WORKFLOW_PATH, &wf("live-roster"));

    assert_eq!(workflow::load_workflow(&repo.root()).unwrap().unwrap().name, "live-roster");
    assert_eq!(workflow::workflow_path(&repo.root()), workflow::WORKFLOW_PATH);
    // NEVER renamed, moved or deleted: it is a tracked file in someone's repo.
    assert!(
        repo.0.join(workflow::LEGACY_WORKFLOW_PATH).is_file(),
        "discovery must not touch the legacy file"
    );
}

#[test]
fn a_repo_with_neither_declares_no_workflow_and_is_told_the_preferred_name() {
    // "No workflow" must stay the ordinary, silent case — and a repo that is
    // about to create one should be pointed at the current spelling, not the
    // deprecated one.
    let repo = Repo::new("neither");
    assert!(!workflow::workflow_file_exists(&repo.root()));
    assert!(workflow::load_workflow(&repo.root()).unwrap().is_none());
    assert_eq!(workflow::workflow_path(&repo.root()), workflow::WORKFLOW_PATH);
}

#[test]
fn a_broken_legacy_file_reports_findings_rather_than_falling_through_to_absent() {
    // The fallback is about WHICH FILE, never about whether a file's problems
    // count. A `.loomux/workflow.yml` that exists and does not validate must
    // report its errors exactly as the preferred spelling would — silently
    // treating it as "no workflow" would hide a broken roster from its author.
    let repo = Repo::new("broken-legacy");
    repo.write(workflow::LEGACY_WORKFLOW_PATH, "version: 99\nblocks:\n  - id: worker\n    kind: worker\n");
    assert!(
        workflow::load_workflow(&repo.root()).is_err(),
        "a broken legacy file must surface as invalid, not as absent"
    );
}

#[test]
fn the_two_spellings_differ_only_in_their_directory() {
    // Both constants name the same file in two directories. If a future edit
    // ever pointed one at a different FILE the fallback would silently start
    // loading something else, and every test above would still pass.
    assert_eq!(workflow::WORKFLOW_PATH, ".orrerix/workflow.yml");
    assert_eq!(workflow::LEGACY_WORKFLOW_PATH, ".loomux/workflow.yml");
}
