//! pi's session store, as the by-id lookup sees it (#2126 P2) — the mirror of
//! `tests/opencodesessions.rs` for the CLI that is file-backed rather than
//! database-backed.
//!
//! What each layer decides, and so what is pinned where:
//!
//! 1. `sessions::pi_sessions_root_from` — WHICH directory, given this process's
//!    environment. A port of the vendor's own resolution order, so every branch
//!    of it is a claim about someone else's code and gets its own assertion.
//! 2. `find_session_cwd("pi", …)` — that a pi id is looked up in pi's store and
//!    reads its header `cwd`, including the one case the dispatch used to get
//!    silently wrong (it fell through to claude's).
//! 3. The filename rule — `_<id>.jsonl` as an exact SUFFIX, never a prefix and
//!    never a bare `contains`.
//!
//! An integration test, not inline `#[cfg(test)]`, per repo constraint 4: it
//! links the full lib, so it needs the comctl32-v6 manifest `build.rs` embeds
//! only for `-tests`-scoped targets.
//!
//! **No pi is ever run** (constraint 3). Every fixture below is written here
//! from the format recorded in `doc/design/pi.md`, read off
//! `earendil-works/pi@b79e4cc8` (= tag `v0.84.4`),
//! `packages/coding-agent/docs/session-format.md`.

use loomux_lib::sessions::{
    find_session_cwd, pi_sessions_root_from, set_claude_projects_root_for_test,
    set_pi_sessions_root_for_test,
};
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One pi session file, in the layout `session-format.md` §File Location
/// describes: `<root>/--<cwd with separators replaced>--/<timestamp>_<id>.jsonl`.
///
/// The header line is written verbatim in the vendor's documented shape,
/// including `version: 3` and the key order, so a reader can compare this
/// fixture against the doc rather than against our paraphrase of it.
fn write_session(root: &Path, project: &str, timestamp: &str, id: &str, cwd: &str, prompts: &[&str]) -> PathBuf {
    let dir = root.join(project);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{timestamp}_{id}.jsonl"));
    let mut body = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\
         \"timestamp\":\"2026-09-03T14:00:00.000Z\",\"cwd\":\"{cwd}\"}}\n"
    );
    for (i, p) in prompts.iter().enumerate() {
        body.push_str(&format!(
            "{{\"type\":\"message\",\"id\":\"m{i}\",\"parentId\":\"p{i}\",\
             \"timestamp\":\"2026-09-03T14:00:0{i}.000Z\",\
             \"message\":{{\"role\":\"user\",\"content\":\"{p}\"}}}}\n"
        ));
    }
    fs::write(&path, body).unwrap();
    path
}

/// A temp root bound to BOTH the pi seam and the claude one. Binding claude too
/// is the point of one test below and harmless to the rest: an unbound claude
/// root would let a pi lookup that wrongly falls through reach the developer's
/// real `~/.claude`, where it would miss for the wrong reason and the assertion
/// would pass while proving nothing.
struct Seam {
    _tmp: tempfile::TempDir,
    pi: PathBuf,
    claude: PathBuf,
}

fn seam() -> Seam {
    let tmp = tempfile::tempdir().unwrap();
    let pi = tmp.path().join("pi-sessions");
    let claude = tmp.path().join("claude-projects");
    fs::create_dir_all(&pi).unwrap();
    fs::create_dir_all(&claude).unwrap();
    set_pi_sessions_root_for_test(Some(pi.clone()));
    set_claude_projects_root_for_test(Some(claude.clone()));
    Seam { _tmp: tmp, pi, claude }
}

impl Drop for Seam {
    fn drop(&mut self) {
        set_pi_sessions_root_for_test(None);
        set_claude_projects_root_for_test(None);
    }
}

// ---------------------------------------------------------------------------
// 1. Which directory — the vendor's resolution order, branch by branch
// ---------------------------------------------------------------------------

#[test]
fn the_pi_session_dir_variable_wins_exactly_as_the_vendor_resolves_it() {
    let home = PathBuf::from("/home/dev");
    let agent = PathBuf::from("/opt/pi-agent");

    // Precedence, `DOCS` settings.md §Sessions: PI_CODING_AGENT_SESSION_DIR
    // names the sessions directory ITSELF — no `sessions` segment is appended,
    // because the variable is the vendor's answer to "where do session files
    // go", not to "where is the agent directory".
    assert_eq!(
        pi_sessions_root_from(Some("/srv/sessions"), Some("/opt/pi-agent"), Some(&home)),
        Some(PathBuf::from("/srv/sessions")),
        "the session-dir variable must win over the agent dir AND over home"
    );

    // Next: PI_CODING_AGENT_DIR relocates the agent directory, under which
    // `sessions` still lives.
    assert_eq!(
        pi_sessions_root_from(None, Some("/opt/pi-agent"), Some(&home)),
        Some(agent.join("sessions"))
    );

    // Default: ~/.pi/agent/sessions.
    assert_eq!(
        pi_sessions_root_from(None, None, Some(&home)),
        Some(home.join(".pi").join("agent").join("sessions"))
    );

    // An EMPTY or whitespace variable is not a value — the shell exports one
    // routinely, and treating it as a path would send every lookup to the
    // process's own working directory.
    assert_eq!(
        pi_sessions_root_from(Some("   "), None, Some(&home)),
        Some(home.join(".pi").join("agent").join("sessions")),
        "a blank session-dir variable must not shadow the default"
    );
    assert_eq!(
        pi_sessions_root_from(None, Some(""), Some(&home)),
        Some(home.join(".pi").join("agent").join("sessions"))
    );

    // No home and no variables: there is no store to name, so `None` — never a
    // relative path that would resolve against the app's own cwd.
    assert_eq!(pi_sessions_root_from(None, None, None), None);
    // …but a NAMED directory still answers with no home at all.
    assert_eq!(
        pi_sessions_root_from(Some("/srv/sessions"), None, None),
        Some(PathBuf::from("/srv/sessions"))
    );
}

// ---------------------------------------------------------------------------
// 2. The by-id lookup
// ---------------------------------------------------------------------------

#[test]
fn a_pi_session_is_found_by_id_across_project_dirs_and_reads_its_header_cwd() {
    let s = seam();
    write_session(&s.pi, "--home-dev-alpha--", "20260901T090000", "aaaa-1111", "/home/dev/alpha", &["first"]);
    write_session(&s.pi, "--home-dev-beta--", "20260902T090000", "bbbb-2222", "/home/dev/beta", &["second"]);

    // Found in the SECOND project directory, so the scan really walks them all
    // rather than answering out of the first.
    assert_eq!(
        find_session_cwd("pi", "bbbb-2222").unwrap().as_deref(),
        Some("/home/dev/beta")
    );
    assert_eq!(
        find_session_cwd("pi", "aaaa-1111").unwrap().as_deref(),
        Some("/home/dev/alpha")
    );

    // An id no file carries is `None` — "no such session", distinct from the
    // found-but-cwd-less answer below.
    assert_eq!(find_session_cwd("pi", "cccc-3333").unwrap(), None);
}

#[test]
fn a_pi_id_never_falls_into_claudes_arm() {
    // THE MISTYPE THIS ARM FIXES, PINNED FROM BOTH SIDES.
    //
    // `find_session_cwd` dispatched "copilot, else claude", so a `pi` source —
    // whose ids are UUIDs, indistinguishable by shape from claude's — was
    // probed for under `~/.claude/projects`. That is not a crash: the probe
    // MISSES and the miss reads as `Ok(None)`, i.e. "no such session", so a
    // resume silently lost its workspace with nothing red to say so.
    //
    // Two halves, because either alone passes on the broken code:
    //   * the id lives ONLY in pi's store and must be found (a claude-arm
    //     lookup returns None there);
    //   * a DIFFERENT id lives only in claude's store under the same stem, and
    //     asking for it as `pi` must NOT find it (a claude-arm lookup would).
    let s = seam();
    write_session(&s.pi, "--home-dev-alpha--", "20260901T090000", "only-in-pi", "/home/dev/alpha", &["hi"]);

    let claude_project = s.claude.join("-home-dev-gamma");
    fs::create_dir_all(&claude_project).unwrap();
    fs::write(
        claude_project.join("only-in-claude.jsonl"),
        "{\"type\":\"user\",\"cwd\":\"/home/dev/gamma\",\"message\":{\"content\":\"hi\"}}\n",
    )
    .unwrap();

    assert_eq!(
        find_session_cwd("pi", "only-in-pi").unwrap().as_deref(),
        Some("/home/dev/alpha"),
        "a pi id must be looked up in pi's store"
    );
    assert_eq!(
        find_session_cwd("pi", "only-in-claude").unwrap(),
        None,
        "a pi lookup must not answer out of claude's store"
    );

    // The control: claude's own arm still answers for claude, so the assertion
    // above is about the DISPATCH and not about a broken fixture.
    assert_eq!(
        find_session_cwd("claude", "only-in-claude").unwrap().as_deref(),
        Some("/home/dev/gamma")
    );
}

#[test]
fn a_pi_session_id_is_matched_by_exact_filename_suffix() {
    // pi's file name is `<timestamp>_<uuid>`, so the id is a SUFFIX rather than
    // the stem — and every weaker match has a real victim:
    //   * a prefix/`contains` match would let `1111` answer for `aaaa-1111`;
    //   * dropping the `_` would let `bb-2222` answer for `aaaa-bb-2222`.
    // Both are the same failure: a resume command carrying an id that belongs to
    // somebody else's conversation.
    let s = seam();
    write_session(&s.pi, "--proj--", "20260901T090000", "aaaa-1111", "/home/dev/one", &["a"]);
    write_session(&s.pi, "--proj--", "20260902T090000", "aaaa-bb-2222", "/home/dev/two", &["b"]);

    assert_eq!(find_session_cwd("pi", "aaaa-1111").unwrap().as_deref(), Some("/home/dev/one"));
    assert_eq!(find_session_cwd("pi", "aaaa-bb-2222").unwrap().as_deref(), Some("/home/dev/two"));

    // The two near-misses.
    assert_eq!(find_session_cwd("pi", "1111").unwrap(), None, "a suffix of an id is not that id");
    assert_eq!(find_session_cwd("pi", "bb-2222").unwrap(), None, "the `_` separator is load-bearing");
    // And the timestamp half never answers, however it is spelled.
    assert_eq!(find_session_cwd("pi", "20260901T090000").unwrap(), None);
}

#[test]
fn a_pi_session_with_no_header_cwd_is_found_with_an_unknown_workspace() {
    // `Ok(Some(""))` and `Ok(None)` are deliberately different answers, the same
    // distinction the claude half draws: a truncated or header-less file is
    // FOUND with an unknown workspace, which `resolve_resume_cwd` tags
    // `resume-workspace-missing`, not `resume-not-found`.
    let s = seam();
    let dir = s.pi.join("--proj--");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("20260901T090000_headerless.jsonl"),
        "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"no header above me\"}}\n",
    )
    .unwrap();

    assert_eq!(find_session_cwd("pi", "headerless").unwrap().as_deref(), Some(""));
    assert_ne!(find_session_cwd("pi", "headerless").unwrap(), None);
}

#[test]
fn an_unusable_pi_id_and_an_absent_store_both_read_as_no_such_session() {
    let s = seam();
    write_session(&s.pi, "--proj--", "20260901T090000", "real", "/home/dev/one", &["a"]);

    // An id that is not one path component cannot name a file under the root,
    // and is refused by `PathSegment::parse` before any directory is touched.
    assert_eq!(find_session_cwd("pi", "../../outside").unwrap(), None);
    assert_eq!(find_session_cwd("pi", "").unwrap(), None);

    // Positive control: the SAME call shape finds a real id, so the three
    // `None`s above are the refusal and not a broken fixture.
    assert_eq!(find_session_cwd("pi", "real").unwrap().as_deref(), Some("/home/dev/one"));

    // A store pi has never written is "nothing recorded", never an error.
    set_pi_sessions_root_for_test(Some(s.pi.join("does-not-exist")));
    assert_eq!(find_session_cwd("pi", "real").unwrap(), None);
}

#[test]
fn a_malformed_line_never_costs_a_pi_session_its_cwd() {
    // Best-effort, like every other scanner here: a partially-flushed final line
    // (pi appends as it goes) or a stray blank must not stop the header being
    // read. The header is written SECOND here on purpose — a scanner that only
    // ever looked at line 1 would fail this.
    let s = seam();
    let dir = s.pi.join("--proj--");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("20260901T090000_ragged.jsonl"),
        "\n{\"type\":\"session\",\"version\":3,\"id\":\"ragged\",\"cwd\":\"/home/dev/ragged\"}\n\
         {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n\
         {\"type\":\"message\",\"messa",
    )
    .unwrap();

    assert_eq!(find_session_cwd("pi", "ragged").unwrap().as_deref(), Some("/home/dev/ragged"));
}
#[test]
fn a_pi_store_named_by_the_env_override_is_read_in_the_flat_layout_pi_writes_there() {
    // REVIEW ROUND 1, FINDING 1 — and the same failure class this whole slice
    // exists to fix, one store layout over.
    //
    // pi nests one directory per cwd under its DEFAULT root, and does NOT under
    // an explicit `--session-dir` / `PI_CODING_AGENT_SESSION_DIR`: the named
    // directory is used verbatim and files land flat in it (`SOURCE`
    // `core/session-manager.ts:1521` `dir = normalizePath(sessionDir)`, `:954`
    // `join(getSessionDir(), <ts>_<id>.jsonl)`; pi's own `list` reads it with a
    // flat `readdir`, `:824`).
    //
    // The first version of this scanner walked two levels unconditionally, so
    // every `.jsonl` in an override store was `read_dir`-ed as a directory,
    // failed, and was skipped — the lookup then answered `Ok(None)`, i.e. "no
    // such session", for a session that exists.
    let s = seam();
    let path = s.pi.join("20260901T090000_flat-1.jsonl");
    fs::write(
        &path,
        "{\"type\":\"session\",\"version\":3,\"id\":\"flat-1\",\"cwd\":\"/srv/work\"}\n\
         {\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n",
    )
    .unwrap();

    assert_eq!(
        find_session_cwd("pi", "flat-1").unwrap().as_deref(),
        Some("/srv/work"),
        "a session file sitting directly in the sessions root must be found"
    );

    // The suffix rule still applies in this layout — it is the same matcher,
    // not a looser second one.
    assert_eq!(find_session_cwd("pi", "1").unwrap(), None);
    assert_eq!(find_session_cwd("pi", "20260901T090000").unwrap(), None);
}

#[test]
fn one_pi_store_can_hold_both_layouts_at_once() {
    // A human who sets `PI_CODING_AGENT_SESSION_DIR` partway through, or unsets
    // it, ends up with both shapes under one root. Neither may hide the other —
    // which is why the walker accepts both unconditionally rather than picking a
    // shape from whichever root won.
    let s = seam();
    fs::write(
        s.pi.join("20260901T090000_flat-1.jsonl"),
        "{\"type\":\"session\",\"version\":3,\"id\":\"flat-1\",\"cwd\":\"/srv/flat\"}\n",
    )
    .unwrap();
    write_session(&s.pi, "--home-dev-nested--", "20260902T090000", "nested-1", "/home/dev/nested", &["hi"]);

    assert_eq!(find_session_cwd("pi", "flat-1").unwrap().as_deref(), Some("/srv/flat"));
    assert_eq!(find_session_cwd("pi", "nested-1").unwrap().as_deref(), Some("/home/dev/nested"));

    // A non-session file in the root is not mistaken for either — and, being a
    // file rather than a directory, must not cost the nested walk anything.
    fs::write(s.pi.join("notes.txt"), "not a session").unwrap();
    assert_eq!(find_session_cwd("pi", "nested-1").unwrap().as_deref(), Some("/home/dev/nested"));
}
