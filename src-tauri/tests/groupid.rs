//! RED-EVIDENCE COMMIT (#904). This file is deliberately committed *before* the
//! `GroupId` change so CI records what the orchestration root does today with a
//! group id it was never asked to validate: it leaves it.
//!
//! Every assertion below is written against the **pre-change public API** —
//! `audit(&str, …)`, `opencode_db_path(&str)`, `promptsubmit_marker_path(&Path,
//! &str, &str)` — precisely so the run is a behavioral red, not a compile error.
//! The next commit lands the newtype and rewrites this file against the
//! validated surface.
//!
//! Integration tests, not unit tests: they link the full lib, which on Windows
//! needs the comctl32-v6 manifest `build.rs` embeds via `-tests`-scoped link
//! args (CLAUDE.md constraint 4).

use loomux_lib::orchestration::{promptsubmit_marker_path, OrchRegistry};
use serde_json::json;

/// `append_audit` is `root.join(group)` then `create_dir_all` + append. A group
/// id carrying `..` therefore writes a real file *outside* the orchestration
/// root — this is the concrete shape of the risk #888 §0 names when the caller
/// stops being our own in-process webview.
#[test]
fn a_traversal_group_id_must_not_write_outside_the_orchestration_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("orchestration");
    let escaped = tmp.path().join("escaped");

    let reg = OrchRegistry::new(root.clone());
    reg.audit("../escaped", "human", "probe", json!({ "probe": "#904" }));

    assert!(
        !escaped.exists(),
        "audit() with group id \"../escaped\" created {} — outside the orchestration root {}",
        escaped.display(),
        root.display()
    );
}

/// Same hole, reached through a *path builder* rather than a writer: every
/// group-scoped path in the process is supposed to be under the root.
#[test]
fn a_traversal_group_id_must_not_produce_a_path_outside_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("orchestration");
    let reg = OrchRegistry::new(root.clone());

    let p = reg.opencode_db_path("../../elsewhere");
    let resolved: std::path::PathBuf = p.components().fold(
        std::path::PathBuf::new(),
        |mut acc, c| match c {
            std::path::Component::ParentDir => {
                acc.pop();
                acc
            }
            other => {
                acc.push(other.as_os_str());
                acc
            }
        },
    );
    assert!(
        resolved.starts_with(&root),
        "opencode_db_path(\"../../elsewhere\") resolves to {} — outside {}",
        resolved.display(),
        root.display()
    );
}

/// The third independent join: the promptsubmit hook marker, built by a free
/// function from `root`, `group` and `agent_id` with no validation of any of
/// them.
#[test]
fn the_promptsubmit_marker_must_not_escape_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("orchestration");

    let p = promptsubmit_marker_path(&root, "../../elsewhere", "w-1");
    let resolved: std::path::PathBuf = p.components().fold(
        std::path::PathBuf::new(),
        |mut acc, c| match c {
            std::path::Component::ParentDir => {
                acc.pop();
                acc
            }
            other => {
                acc.push(other.as_os_str());
                acc
            }
        },
    );
    assert!(
        resolved.starts_with(&root),
        "promptsubmit_marker_path(root, \"../../elsewhere\", \"w-1\") resolves to {} — outside {}",
        resolved.display(),
        root.display()
    );
}
