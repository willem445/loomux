//! GitHub issue integration for the per-pane issues view. Everything shells out
//! to the authenticated `gh` CLI — mirroring how `git.rs` shells out to `git`
//! — so loomux stores no token, OAuth, or secret and inherits the user's
//! existing `gh auth login`.
//!
//! Trust boundary (CLAUDE.md constraint 6): `repo` is resolved backend-side
//! from the pane's cwd (via `git::git_repo_root`) and used only as the working
//! directory; `gh` infers the GitHub repository from that checkout's remote —
//! no frontend-supplied repo string ever reaches a `--repo` flag. Labels that
//! can be written are gated by a fixed allow-list, so a create/label call can
//! never attach an arbitrary label even though the webview is trusted.
//!
//! Like `git.rs`, spawns are arg-vectors (shell injection is impossible) and
//! `gh` output is decoded lossily. The `--json` field set is pinned rather than
//! parsing human output, so `gh` cosmetic changes don't break parsing.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Output};

/// Run a `gh`-backed computation off the webview main thread (issue #716).
/// Tauri dispatches a *synchronous* `#[tauri::command]` by calling it directly
/// on the main thread — the mechanism `git.rs`'s `run_blocking` (issue #399)
/// and the file-editor search command (issue #207) already document. Every
/// command in this module spawns the `gh` CLI, which is a process spawn *plus*
/// a network round trip, so its worst case is longer than git's: sub-second on
/// a good day, seconds on a slow link, unbounded if `gh` hangs. For that whole
/// duration a sync command blocks the GUI thread — no keystroke serviced,
/// nothing painted.
///
/// So every `#[tauri::command]` here is a thin `async fn` that hands the real
/// work — still a plain, directly unit-testable `*_sync` function — to a
/// blocking-pool thread and awaits it here instead. A test in this module
/// scans this file and fails if any command in it is left synchronous: a lone
/// straggler would keep the freeze while the module claimed not to.
///
/// What this gives up, and why that is safe: the freeze WAS an accidental
/// mutual exclusion — while the main thread sat in a `gh` spawn no second
/// invoke could start, so two of these could never overlap. Off-thread they
/// can. Nothing here relied on that: every command is stateless (it spawns
/// `gh`, parses stdout, returns — no shared state, no `tauri::State`, no file
/// or registry write, and no event emitted, so there is no event ordering to
/// preserve either). The only shared state is GitHub itself, and the one place
/// that races — two `gh label create`s for the same label — is already treated
/// as success by `ensure_labels_with`, which was written for the concurrent
/// create it could always hit from another client.
async fn run_blocking<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => Err(format!("gh task panicked: {e}")),
    }
}

/// Labels the issues view is permitted to add/remove **whatever the repo's
/// workflow says**. These are the durable signals the orchestrator's intake poll
/// watches for (see `orchestration/templates/orchestrator.md`): `agent-ready` /
/// `agent-investigation` say *start*, `agent-managed` says *owned*. Anything
/// outside the resolved set is rejected before a spawn — the allow-list is the
/// whole point of routing labels through the backend rather than letting the
/// frontend pass label strings.
///
/// The **veto** label is deliberately NOT here: `intake.labels.hold` is
/// repo-configurable (#778), so it is resolved per repo by [`hold_label`] and
/// added to this fixed set by [`allowed_labels`]. See that function for why the
/// allow-list stays an allow-list.
///
/// NB: the label that actually exists on the repo (and that `gh issue edit
/// --add-label` therefore accepts) is `agent-investigation`, not the shorter
/// `agent-investigate` the issue-#82 plan text used. We use the real label so
/// the write succeeds and the orchestrator's substring match still picks it up.
const FIXED_LABELS: [&str; 3] = ["agent-ready", "agent-investigation", "agent-managed"];

/// This repo's spelling of the hold veto (#778) — `intake.labels.hold` from its
/// `.loomux/workflow.yml`, or the built-in `agent-hold` when the file declares
/// none, is absent, or does not parse.
///
/// **Resolved through the engine, never re-implemented here.** `load_workflow`
/// is the same function `create_group` and `orch_workflow_preview` run, and
/// `Workflow::intake` is always fully resolved (built-in defaults filled in), so
/// the spelling this returns is by construction the spelling the intake poller
/// will honor for a group launched on this repo. A second parser here is exactly
/// how the UI and the poller would drift apart again.
///
/// **A broken workflow file falls back to the built-in**, deliberately: a repo
/// whose file does not parse gets the built-in roster from `create_group` too,
/// so its poller is watching `agent-hold`, and the UI must write what the poller
/// watches. Failing closed (refusing every hold write) would take away the veto
/// gesture over a typo elsewhere in the file.
fn hold_label(repo: &str) -> String {
    loomux_engine::workflow::load_workflow(repo)
        .ok()
        .flatten()
        .map(|wf| wf.intake.hold)
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| loomux_engine::workflow::builtin_intake_profile().hold)
}

/// The full set of labels writable for `repo`: the three fixed signals plus this
/// repo's resolved hold spelling.
///
/// **Still an allow-list, and that is the load-bearing part.** The hold entry is
/// ONE value resolved from the repo's own committed config — not a wildcard, not
/// a pattern, and never a string the frontend supplied. `sanitize_intake_label`
/// has already constrained it at parse time to the `sanitize_id` alphabet
/// (letters, digits, `-`, `_`), and a label that would need rewriting is rejected
/// by the parser rather than coerced, so nothing shell-ish or `--flag`-shaped can
/// reach an argv through this door. A repo that declares no `hold:` gets exactly
/// the previous four-label set.
fn allowed_labels(repo: &str) -> Vec<String> {
    let mut out: Vec<String> = FIXED_LABELS.iter().map(|s| s.to_string()).collect();
    let hold = hold_label(repo);
    if !out.contains(&hold) {
        out.push(hold);
    }
    out
}

/// Per-list cap for [`gh_activity`] (the progress-timeline view, #608). Bounded
/// so a long-lived repo can't hand the timeline an unbounded payload — but the
/// bound is *reported* (`GhActivity::limit` + the `*_truncated` flags) rather
/// than silently trimming history. A chart that looks complete when it isn't is
/// the exact failure class `.loomux/lessons.md` names under "no silent caps".
const ACTIVITY_LIMIT: usize = 100;

/// Color (6-hex, no `#`) and description used to *create* an allow-listed label
/// in a repo that doesn't have it yet (see `ensure_labels_exist`). `gh issue
/// edit --add-label` fails outright on a label the repo has never defined, so a
/// fresh repo could never be handed to an orchestrator from the issues view
/// without this. Kept in lockstep with `ALLOWED_LABELS` (a test asserts every
/// allowed label has a spec). `agent-managed`'s color/description match the
/// orchestrator template's convention so a loomux-created label is
/// indistinguishable from one the orchestrator itself would create.
fn label_spec(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        "agent-managed" => Some(("5319e7", "Managed by a loomux orchestrator")),
        "agent-ready" => Some(("0e8a16", "Groomed and ready for a loomux agent to build")),
        "agent-investigation" => Some((
            "fbca04",
            "Research only — findings as an issue comment; no code",
        )),
        _ => None,
    }
}

/// The create spec for a label allowed on `repo`, resolving the one entry whose
/// NAME is repo-configurable.
///
/// The hold veto (#778) cannot be matched by literal in [`label_spec`] any more:
/// a repo that renamed it to `do-not-touch` must still get the label *created*
/// on first use, or its one-click veto fails on a fresh repo with `gh`'s "label
/// not found" — which is the same silent-veto failure the rename support exists
/// to prevent, one layer down. Matching on "is this the resolved hold spelling"
/// rather than on the literal is what makes the rename total.
///
/// Red, and the description states the rule rather than naming the label, so it
/// stays legible on GitHub to someone who has never read the orchestrator
/// contract — including the human deciding whether to apply it — under whatever
/// name the repo chose.
fn label_spec_for(repo: &str, name: &str) -> Option<(&'static str, &'static str)> {
    if let Some(spec) = label_spec(name) {
        return Some(spec);
    }
    (name == hold_label(repo)).then_some((
        "b60205",
        "Held by the human — full-autonomy agents must not start this",
    ))
}

/// Spawn `gh` and capture the raw `Output` (status + stdout + stderr). Only a
/// spawn failure is an `Err`; a non-zero exit is left for the caller to
/// interpret (e.g. `gh auth status` exits non-zero when unauthenticated, which
/// is a normal state, not an error). A missing binary maps to the sentinel
/// `"gh-not-found"` so callers can render the install hint.
///
/// `repo` is the working directory; `None` for repo-independent commands like
/// `gh auth status`.
fn gh_output(repo: Option<&str>, args: &[&str]) -> Result<Output, String> {
    let mut cmd = Command::new("gh");
    if let Some(r) = repo {
        if !Path::new(r).is_dir() {
            return Err(format!("no such directory: {r}"));
        }
        cmd.current_dir(r);
    }
    // NO_COLOR keeps `auth status` text free of ANSI escapes for parsing;
    // GH_PAGER="" and GH_PROMPT_DISABLED keep gh non-interactive so a command
    // can never block waiting on a pager or a prompt.
    cmd.args(args)
        .env("NO_COLOR", "1")
        .env("GH_PAGER", "")
        .env("GH_PROMPT_DISABLED", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "gh-not-found".to_string()
        } else {
            e.to_string()
        }
    })
}

/// Run `gh` and require success, returning stdout. Non-zero exit → Err(stderr),
/// mirroring `git.rs`'s `run_git`.
fn run_gh(repo: Option<&str>, args: &[&str]) -> Result<String, String> {
    let out = gh_output(repo, args)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // A totally silent failure is unhelpful; fall back to a generic note.
        if err.is_empty() {
            Err(format!("gh exited with {}", out.status))
        } else {
            Err(err)
        }
    }
}

// ---------- types ----------

/// Result of `gh auth status`, driving the view's empty-state.
#[derive(Serialize)]
pub struct GhAuth {
    /// The `gh` binary is on PATH.
    pub installed: bool,
    /// `gh` reports an authenticated account.
    pub authenticated: bool,
    /// The logged-in account name, when parseable.
    pub login: Option<String>,
}

/// One open issue, from `gh issue list --json`.
#[derive(Serialize, PartialEq, Debug)]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    /// Label names only — the frontend highlights the agent go-signals itself.
    pub labels: Vec<String>,
    /// "OPEN" / "CLOSED" as gh reports it.
    pub state: String,
    /// RFC-3339 timestamp string, e.g. "2026-07-07T04:18:09Z".
    pub updated_at: String,
    pub url: String,
}

/// A freshly created issue.
#[derive(Serialize, PartialEq, Debug)]
pub struct GhIssueRef {
    pub number: u64,
    pub url: String,
}

/// One comment on an issue or PR, from the `comments` field of `gh {issue,pr}
/// view --json`. `author` is the commenter's login (None for a deleted/ghost
/// account). All fields are GitHub-authored text — the frontend renders them
/// with `textContent` only (the #129 XSS boundary), never innerHTML.
#[derive(Serialize, PartialEq, Debug)]
pub struct GhComment {
    pub author: Option<String>,
    /// RFC-3339 timestamp string, e.g. "2026-07-07T04:18:09Z".
    pub created_at: String,
    pub body: String,
}

/// Full detail for one issue or PR, from `gh {issue,pr} view --json`. The two
/// share a shape (title/body/labels/state/author/comments), so one struct backs
/// both the issue- and PR-detail panes; `state` distinguishes them at the edges
/// (a PR can be "MERGED"). `body` is the markdown description verbatim.
#[derive(Serialize, PartialEq, Debug)]
pub struct GhDetail {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub state: String,
    pub author: Option<String>,
    pub comments: Vec<GhComment>,
}

/// One open pull request, from `gh pr list --json`. Mirrors `GhIssue` (so the
/// same client-side filter/sort applies) plus `head_ref` — the source branch,
/// handy context in the list. Read-only in v1: the view lists, opens detail, and
/// comments on PRs, but never labels/merges/approves.
#[derive(Serialize, PartialEq, Debug)]
pub struct GhPr {
    pub number: u64,
    pub title: String,
    /// "OPEN" / "CLOSED" / "MERGED" as gh reports it (v1 lists only open).
    pub state: String,
    pub labels: Vec<String>,
    pub updated_at: String,
    pub url: String,
    /// The PR's source (head) branch name.
    pub head_ref: String,
}

/// One issue's *lifecycle* timestamps, for the progress-timeline view (#608).
/// Distinct from [`GhIssue`], which is the issues-view row: open-state only and
/// carrying `updatedAt` alone. A time axis plots when a thing *happened*, so
/// this carries the opened/closed instants instead.
#[derive(Serialize, PartialEq, Debug)]
pub struct GhIssueActivity {
    pub number: u64,
    pub title: String,
    /// "OPEN" / "CLOSED" as gh reports it.
    pub state: String,
    /// RFC-3339 open time. Empty when gh omitted it: one row with a broken
    /// timestamp is parked by the frontend as "undatable" rather than failing
    /// the whole list or being plotted at the epoch.
    pub created_at: String,
    /// RFC-3339 close time; `None` while the issue is open.
    pub closed_at: Option<String>,
    /// RFC-3339 last-activity time — the key this list is *sorted* by, and what
    /// lets the frontend state a precise coverage floor when the list is
    /// truncated: nothing omitted was active more recently than the oldest row
    /// returned, so the window above that instant is complete.
    pub updated_at: String,
    pub url: String,
}

/// One PR's lifecycle timestamps. Mirrors [`GhIssueActivity`] plus `merged_at`
/// and the head branch. `merged_at` is what separates the two ways a PR ends:
/// a PR closed unmerged has `closed_at` set and `merged_at` `None`, and must
/// never render as a merge.
#[derive(Serialize, PartialEq, Debug)]
pub struct GhPrActivity {
    pub number: u64,
    pub title: String,
    /// "OPEN" / "CLOSED" / "MERGED" as gh reports it.
    pub state: String,
    pub created_at: String,
    /// RFC-3339 close time. Set for a merge too — GitHub closes a PR when it
    /// merges it — so `merged_at`, not this, decides "was it merged".
    pub closed_at: Option<String>,
    /// RFC-3339 merge time; `None` for an open PR and for one closed unmerged.
    pub merged_at: Option<String>,
    pub updated_at: String,
    pub url: String,
    /// The PR's source (head) branch name.
    pub head_ref: String,
}

/// Issue + PR lifecycle activity for one repo, with its own coverage boundary
/// attached. `limit` and the `*_truncated` flags exist so the view can *say*
/// how far back the data reaches instead of implying it covers everything (see
/// [`ACTIVITY_LIMIT`]).
#[derive(Serialize, PartialEq, Debug)]
pub struct GhActivity {
    pub issues: Vec<GhIssueActivity>,
    pub prs: Vec<GhPrActivity>,
    /// The per-list cap that produced these lists — reported rather than
    /// duplicated as a frontend constant, so the two can't drift.
    pub limit: usize,
    /// The issue list came back full, so older activity may exist beyond it.
    /// Deliberately conservative: a repo with exactly `limit` issues reports
    /// truncated, because gh's page alone cannot distinguish "exactly full"
    /// from "full and more" — over-reporting the boundary is the safe error.
    pub issues_truncated: bool,
    /// Same, for the PR list.
    pub prs_truncated: bool,
}

// gh's JSON uses camelCase and nests labels as objects; these mirror it for
// deserialization only. Extra fields (id, color, description) are ignored.
#[derive(Deserialize)]
struct RawIssue {
    number: u64,
    title: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    state: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    url: String,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

// gh's `author` object carries several fields (id, is_bot, name, login); we only
// keep `login`. `#[serde(default)]` so a missing login decodes to "" (mapped to
// None by parse_detail) rather than failing the whole parse.
#[derive(Deserialize)]
struct RawAuthor {
    #[serde(default)]
    login: String,
}

// `gh {issue,pr} view --json comments` element. Extra fields (id, url,
// authorAssociation, reactionGroups, includesCreatedEdit) are ignored.
#[derive(Deserialize)]
struct RawComment {
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(rename = "createdAt", default)]
    created_at: String,
    #[serde(default)]
    body: String,
}

// `gh {issue,pr} view --json title,body,labels,state,author,comments`. `body`
// defaults to "" (an issue can have an empty description).
#[derive(Deserialize)]
struct RawDetail {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    state: String,
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(default)]
    comments: Vec<RawComment>,
}

// `gh pr list --json number,title,state,labels,updatedAt,url,headRefName`.
#[derive(Deserialize)]
struct RawPr {
    number: u64,
    title: String,
    state: String,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    url: String,
    #[serde(rename = "headRefName", default)]
    head_ref: String,
}

// `gh issue list --json number,title,state,createdAt,closedAt,updatedAt,url`.
// Every field but `number` defaults, so a gh field rename degrades that one
// column instead of failing the whole timeline load.
#[derive(Deserialize)]
struct RawIssueActivity {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(rename = "createdAt", default)]
    created_at: String,
    #[serde(rename = "closedAt", default)]
    closed_at: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at: String,
    #[serde(default)]
    url: String,
}

// `gh pr list --json number,title,state,createdAt,closedAt,mergedAt,updatedAt,url,headRefName`.
#[derive(Deserialize)]
struct RawPrActivity {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(rename = "createdAt", default)]
    created_at: String,
    #[serde(rename = "closedAt", default)]
    closed_at: Option<String>,
    #[serde(rename = "mergedAt", default)]
    merged_at: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "headRefName", default)]
    head_ref: String,
}

// ---------- commands ----------

/// Report whether `gh` is installed and authenticated. Never errors on a
/// missing/unauthenticated `gh` — those are states the UI renders, not faults.
#[tauri::command]
pub async fn gh_auth_status() -> Result<GhAuth, String> {
    run_blocking(gh_auth_status_sync).await
}

fn gh_auth_status_sync() -> Result<GhAuth, String> {
    match gh_output(None, &["auth", "status"]) {
        Ok(out) => {
            // gh has emitted `auth status` on stdout in some versions and
            // stderr in others — concatenate so the login parse is robust.
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            Ok(GhAuth {
                installed: true,
                authenticated: out.status.success(),
                login: parse_auth_login(&text),
            })
        }
        Err(e) if e == "gh-not-found" => Ok(GhAuth {
            installed: false,
            authenticated: false,
            login: None,
        }),
        Err(e) => Err(e),
    }
}

/// List open issues for the pane's repo (first page, up to 50). Labels are
/// returned verbatim; matching/highlighting happens client-side (the
/// orchestrator note warns `--label` server-side filtering silently misses
/// issues that carry the label).
#[tauri::command]
pub async fn gh_issue_list(repo: String) -> Result<Vec<GhIssue>, String> {
    run_blocking(move || gh_issue_list_sync(repo)).await
}

fn gh_issue_list_sync(repo: String) -> Result<Vec<GhIssue>, String> {
    let out = run_gh(
        Some(&repo),
        &[
            "issue",
            "list",
            "--state",
            "open",
            "--json",
            "number,title,labels,state,updatedAt,url",
            "--limit",
            "50",
        ],
    )?;
    parse_issue_list(&out)
}

/// Create an issue from a title and body, returning its number and URL.
#[tauri::command]
pub async fn gh_issue_create(
    repo: String,
    title: String,
    body: String,
) -> Result<GhIssueRef, String> {
    run_blocking(move || gh_issue_create_sync(repo, title, body)).await
}

fn gh_issue_create_sync(repo: String, title: String, body: String) -> Result<GhIssueRef, String> {
    if title.trim().is_empty() {
        return Err("empty issue title".to_string());
    }
    let args = issue_create_args(&title, &body);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_gh(Some(&repo), &argv)?;
    parse_issue_ref(&out)
}

/// Add and/or remove labels on an issue. Every label — add or remove — is
/// validated against [`allowed_labels`] for this repo before any spawn, so this
/// can never attach or strip a label outside the agent go-signal set plus the
/// repo's own resolved hold spelling (#778).
#[tauri::command]
pub async fn gh_issue_set_labels(
    repo: String,
    number: u64,
    add: Vec<String>,
    remove: Vec<String>,
) -> Result<(), String> {
    run_blocking(move || gh_issue_set_labels_sync(repo, number, add, remove)).await
}

fn gh_issue_set_labels_sync(
    repo: String,
    number: u64,
    add: Vec<String>,
    remove: Vec<String>,
) -> Result<(), String> {
    // Resolved once, from the repo's own committed workflow config, and used for
    // both directions: a repo that renamed the veto must be able to APPLY its
    // spelling and to LIFT it again.
    let allowed = allowed_labels(&repo);
    validate_labels(&allowed, &add)?;
    validate_labels(&allowed, &remove)?;
    // Nothing to do — don't spawn gh just to no-op (gh issue edit with neither
    // flag would open an interactive editor).
    if add.is_empty() && remove.is_empty() {
        return Ok(());
    }
    // `gh issue edit --add-label` errors if the label isn't defined on the repo,
    // so create any allow-listed label we're about to add that's missing. Only
    // adds need this; removing a label the repo lacks is already a no-op at gh.
    if !add.is_empty() {
        ensure_labels_exist(&repo, &add)?;
    }
    let args = issue_edit_args(number, &add, &remove);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_gh(Some(&repo), &argv).map(|_| ())
}

/// The label vocabulary the issues view may write for a repo (#778).
///
/// Exists because the veto label's spelling is repo-configurable and the
/// frontend must not guess it: before this, `issuesmodel.ts` held
/// `AGENT_HOLD = "agent-hold"` as a literal, so a repo that renamed the veto got
/// a strike button writing a label its own poller ignored. The frontend now
/// *asks* rather than knowing, and what it is told is the same value
/// [`allowed_labels`] validates the write against — one resolution, so the
/// button and the allow-list cannot disagree.
///
/// Read-only and free: it opens the repo's `.loomux/workflow.yml` and parses it,
/// spawning no `gh` at all. Off-thread anyway (#716), because a file read on the
/// paint thread is still a file read.
/// **Only `hold` is repo-resolved, and that asymmetry is the honest state of the
/// world, not an oversight.** `intake.labels.ready`/`investigate`/`owned`/
/// `prototype` are parsed and stored but not yet honored anywhere (#382 P2 — the
/// gap is uniform: renaming them simply does not work), so reporting a
/// configured spelling for them here would advertise a configurability nothing
/// implements — the exact defect this struct exists to fix, pointed the other
/// way. They are reported as the built-ins that `FIXED_LABELS` actually allows,
/// and they start being repo-resolved on the day #382 P2 lands, in one place.
#[derive(Serialize, PartialEq, Debug)]
pub struct GhLabelVocabulary {
    /// "Build this" — the go signal. Built-in; see the note above.
    pub ready: String,
    /// "Look, don't build." Built-in; see the note above.
    pub investigate: String,
    /// The human's veto (#778) — `agent-hold` unless this repo renamed it. The
    /// one field here that is genuinely resolved from the repo's config.
    pub hold: String,
}

#[tauri::command]
pub async fn gh_label_vocabulary(repo: String) -> GhLabelVocabulary {
    run_blocking(move || Ok(gh_label_vocabulary_sync(&repo)))
        .await
        // A panicking blocking pool must not leave the issues view with no
        // vocabulary at all: fall back to the built-ins, which is exactly what a
        // repo with no workflow file resolves to anyway.
        .unwrap_or_else(|_| gh_label_vocabulary_sync(""))
}

#[doc(hidden)] // pub for integration tests: resolve a repo's vocabulary without a Tauri runtime
pub fn gh_label_vocabulary_sync(repo: &str) -> GhLabelVocabulary {
    let builtin = loomux_engine::workflow::builtin_intake_profile();
    GhLabelVocabulary {
        ready: builtin.ready,
        investigate: builtin.investigate,
        hold: hold_label(repo),
    }
}

/// Full detail for one issue: description, labels, state, author, and the whole
/// comment thread — backing the issues-view detail pane. Read-only; writes go
/// through `gh_issue_comment` / `gh_issue_set_labels`.
#[tauri::command]
pub async fn gh_issue_view(repo: String, number: u64) -> Result<GhDetail, String> {
    run_blocking(move || gh_issue_view_sync(repo, number)).await
}

fn gh_issue_view_sync(repo: String, number: u64) -> Result<GhDetail, String> {
    let n = number.to_string();
    let out = run_gh(
        Some(&repo),
        &[
            "issue",
            "view",
            &n,
            "--json",
            "title,body,labels,state,author,comments",
        ],
    )?;
    parse_detail(&out)
}

/// Post a comment on an issue. `body` is the user's text, passed as the VALUE of
/// `--body` (a discrete arg, never interpolated), so a leading `-`, spaces, or
/// newlines stay data — see `comment_args`. Empty/whitespace bodies are rejected
/// before spawning (gh would open an interactive editor with no `--body`).
#[tauri::command]
pub async fn gh_issue_comment(repo: String, number: u64, body: String) -> Result<(), String> {
    run_blocking(move || gh_issue_comment_sync(repo, number, body)).await
}

fn gh_issue_comment_sync(repo: String, number: u64, body: String) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("empty comment".to_string());
    }
    let args = comment_args("issue", number, &body);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_gh(Some(&repo), &argv).map(|_| ())
}

/// List open pull requests for the pane's repo (first page, up to 50). Mirrors
/// `gh_issue_list`; labels returned verbatim for client-side matching. Read-only
/// — the view lists and comments on PRs but never labels/merges/approves.
#[tauri::command]
pub async fn gh_pr_list(repo: String) -> Result<Vec<GhPr>, String> {
    run_blocking(move || gh_pr_list_sync(repo)).await
}

fn gh_pr_list_sync(repo: String) -> Result<Vec<GhPr>, String> {
    let out = run_gh(
        Some(&repo),
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "number,title,state,labels,updatedAt,url,headRefName",
            "--limit",
            "50",
        ],
    )?;
    parse_pr_list(&out)
}

/// Full detail for one PR — same shape as `gh_issue_view` (`gh pr view` exposes
/// the identical `--json` fields), so both feed the one detail pane.
#[tauri::command]
pub async fn gh_pr_view(repo: String, number: u64) -> Result<GhDetail, String> {
    run_blocking(move || gh_pr_view_sync(repo, number)).await
}

fn gh_pr_view_sync(repo: String, number: u64) -> Result<GhDetail, String> {
    let n = number.to_string();
    let out = run_gh(
        Some(&repo),
        &[
            "pr",
            "view",
            &n,
            "--json",
            "title,body,labels,state,author,comments",
        ],
    )?;
    parse_detail(&out)
}

/// Post a comment on a PR. Same discrete-`--body` safety and empty-body guard as
/// `gh_issue_comment` (commenting is the one write the read-only PR mode allows).
#[tauri::command]
pub async fn gh_pr_comment(repo: String, number: u64, body: String) -> Result<(), String> {
    run_blocking(move || gh_pr_comment_sync(repo, number, body)).await
}

fn gh_pr_comment_sync(repo: String, number: u64, body: String) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("empty comment".to_string());
    }
    let args = comment_args("pr", number, &body);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_gh(Some(&repo), &argv).map(|_| ())
}

/// Issue and PR *lifecycle* activity for the pane's repo — the gh half of the
/// progress-timeline view (#608). Read-only, and the only gh command in this
/// file that looks at closed/merged history rather than the open worklist.
///
/// Both lists are capped at [`ACTIVITY_LIMIT`] and ordered most-recently-active
/// first (see [`activity_issue_args`] for why that ordering has to be pinned
/// explicitly). The cap and whether it was hit come back in the result so the
/// view can render its own coverage boundary.
///
/// One gh failure fails the whole call rather than returning a half-populated
/// timeline: a chart silently missing every PR would look like a quiet period,
/// which is worse than an error the view can say out loud.
#[tauri::command]
pub async fn gh_activity(repo: String) -> Result<GhActivity, String> {
    run_blocking(move || gh_activity_sync(repo)).await
}

/// The two `gh` runs stay sequential inside ONE blocking task rather than
/// becoming two concurrent ones: the fail-whole rule above is only meaningful
/// if the issue failure short-circuits before the PR list is even attempted,
/// and doubling the concurrent `gh` processes per refresh buys nothing the
/// 60 s cadence needs.
fn gh_activity_sync(repo: String) -> Result<GhActivity, String> {
    let issue_args = activity_issue_args(ACTIVITY_LIMIT);
    let argv: Vec<&str> = issue_args.iter().map(String::as_str).collect();
    let issues = parse_issue_activity(&run_gh(Some(&repo), &argv)?)?;

    let pr_args = activity_pr_args(ACTIVITY_LIMIT);
    let argv: Vec<&str> = pr_args.iter().map(String::as_str).collect();
    let prs = parse_pr_activity(&run_gh(Some(&repo), &argv)?)?;

    Ok(assemble_activity(issues, prs, ACTIVITY_LIMIT))
}

/// Create any allow-listed label in `labels` that the repo doesn't already
/// define, so a following `gh issue edit --add-label` can attach it. Callers
/// must have validated `labels` against the allow-list first. Thin wrapper over
/// [`ensure_labels_with`] that binds the `gh` runner AND the spec lookup to this
/// repo — the latter because the hold label's NAME is repo-configurable (#778),
/// so "what colour/description do I create this with" is a per-repo question now.
fn ensure_labels_exist(repo: &str, labels: &[String]) -> Result<(), String> {
    ensure_labels_with(labels, |name| label_spec_for(repo, name), |args| {
        run_gh(Some(repo), args)
    })
}

/// The label-ensure flow, parameterized over a `gh` runner so it can be unit
/// tested without a real `gh`. `run` receives an argv (e.g. `label list …` or
/// `label create …`) and returns gh's stdout on success / stderr on failure.
///
/// Two design points, both defending a toggle that would otherwise have
/// succeeded on a repo that already has the labels:
///
/// 1. **List-first, not blind-create.** We list the repo's labels once and
///    create only the genuinely-missing ones — a user who *can* toggle labels
///    but *can't* manage them still succeeds when the labels already exist,
///    whereas a blind create would 403 and wrongly block the toggle. Names are
///    compared case-insensitively because GitHub label names are
///    case-insensitively unique — an existing `Agent-Ready` already satisfies an
///    add of `agent-ready`, so we must not attempt a doomed create.
/// 2. **List failure is non-fatal.** A transient `gh label list` error (rate
///    limit, network blip) must not abort a toggle the pre-ensure edit-only path
///    would have completed. On a list failure we fall back to an empty "known"
///    set (best-effort create) AND, because we can no longer trust that a label
///    is truly missing, we swallow create failures too and let the subsequent
///    `gh issue edit` be the source of truth. Only a create that failed *after*
///    we reliably confirmed the label absent is surfaced (with a friendly
///    permission hint). An "already exists" create failure is always success —
///    it covers both the create/create race and the label-existed-all-along case
///    when listing blipped.
fn ensure_labels_with<S, F>(labels: &[String], spec_of: S, mut run: F) -> Result<(), String>
where
    S: Fn(&str) -> Option<(&'static str, &'static str)>,
    F: FnMut(&[&str]) -> Result<String, String>,
{
    // `--limit` is deliberately generous: an allow-listed label past the page
    // would only cost a redundant create that the already-exists path absorbs.
    let existing = match run(&["label", "list", "--json", "name", "--limit", "500"]) {
        Ok(json) => parse_label_names(&json).ok(),
        Err(_) => None,
    };
    // Existence is only trustworthy if the list both ran and parsed.
    let existence_reliable = existing.is_some();
    let existing = existing.unwrap_or_default();

    for name in labels {
        if existing.iter().any(|e| e.eq_ignore_ascii_case(name)) {
            continue;
        }
        // Unreachable for validated input (every allow-listed label has a spec,
        // asserted by test); guard rather than panic if the two ever drift.
        let (color, description) =
            spec_of(name).ok_or_else(|| format!("no label spec for {name:?}"))?;
        let args = label_create_args(name, color, description);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err(e) = run(&argv) {
            if is_label_exists_error(&e) {
                continue; // label is there now (race, or it existed and listing blipped).
            }
            if existence_reliable {
                // We know it was missing and the create genuinely failed — surface it.
                return Err(map_label_create_error(name, &e));
            }
            // Listing failed, so we don't actually know the label is missing;
            // let `gh issue edit` report the real outcome instead of blocking.
        }
    }
    Ok(())
}

// ---------- pure helpers (unit-tested) ----------

/// Reject any label not in `allowed` — the repo's resolved set from
/// [`allowed_labels`]. Takes the set rather than reading the const so the
/// resolution happens once per call at the command boundary, and so this stays a
/// pure function a unit test can drive with any vocabulary.
fn validate_labels(allowed: &[String], labels: &[String]) -> Result<(), String> {
    for l in labels {
        if !allowed.iter().any(|a| a == l) {
            return Err(format!("label not allowed: {l:?}"));
        }
    }
    Ok(())
}

/// Build the `gh issue create` argv. Title/body are separate args (never
/// interpolated into a string), so their content — including a leading `-` or
/// newlines — is data, not flags.
fn issue_create_args(title: &str, body: &str) -> Vec<String> {
    vec![
        "issue".into(),
        "create".into(),
        "--title".into(),
        title.into(),
        "--body".into(),
        body.into(),
    ]
}

/// Build the `gh issue edit <n>` argv with `--add-label`/`--remove-label` for
/// each label. Callers must validate labels first (see `validate_labels`).
fn issue_edit_args(number: u64, add: &[String], remove: &[String]) -> Vec<String> {
    let mut args = vec!["issue".into(), "edit".into(), number.to_string()];
    for l in add {
        args.push("--add-label".into());
        args.push(l.clone());
    }
    for l in remove {
        args.push("--remove-label".into());
        args.push(l.clone());
    }
    args
}

/// Build the `gh label create <name>` argv. Name/color/description are discrete
/// args (never interpolated), so a description containing spaces, an em-dash, or
/// a leading `-` stays data. Colors are passed without a leading `#` per gh.
fn label_create_args(name: &str, color: &str, description: &str) -> Vec<String> {
    vec![
        "label".into(),
        "create".into(),
        name.into(),
        "--color".into(),
        color.into(),
        "--description".into(),
        description.into(),
    ]
}

/// Parse `gh label list --json name` into a flat list of names. Reuses the
/// `RawLabel` shape (`gh` emits the same `{"name": …}` objects here).
fn parse_label_names(json: &str) -> Result<Vec<String>, String> {
    let raw: Vec<RawLabel> =
        serde_json::from_str(json).map_err(|e| format!("gh label list: bad JSON: {e}"))?;
    Ok(raw.into_iter().map(|l| l.name).collect())
}

/// True when a `gh label create` failure means the label already exists — the
/// race outcome we treat as success. `gh` phrases this as
/// "… already exists"; match case-insensitively so a wording tweak doesn't slip.
fn is_label_exists_error(stderr: &str) -> bool {
    stderr.to_lowercase().contains("already exists")
}

/// True when a `gh label create` failure looks like a permissions problem (the
/// account can view issues but can't manage labels): `gh` surfaces the API's
/// 403 as "HTTP 403", "Resource not accessible", or a "must have … permission"
/// GraphQL message. Best-effort — only used to pick a friendlier wording.
fn looks_like_permission_error(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    s.contains("403")
        || s.contains("not accessible")
        || s.contains("must have")
        || s.contains("permission")
}

/// Turn a real (non-race) `gh label create` failure into the message the issues
/// view renders in its toast. The permission case gets an actionable hint since
/// it's the common one (a contributor without label-management rights); anything
/// else keeps gh's own text so network/other failures stay diagnosable.
fn map_label_create_error(name: &str, stderr: &str) -> String {
    if looks_like_permission_error(stderr) {
        format!(
            "Can't create the '{name}' label — your GitHub account lacks permission to manage labels on this repo. Ask a maintainer to add the agent labels, then try again."
        )
    } else {
        format!("Couldn't create the '{name}' label: {stderr}")
    }
}

/// Parse `gh issue list --json …` into `GhIssue`s, flattening label objects to
/// their names.
fn parse_issue_list(json: &str) -> Result<Vec<GhIssue>, String> {
    let raw: Vec<RawIssue> =
        serde_json::from_str(json).map_err(|e| format!("gh issue list: bad JSON: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|r| GhIssue {
            number: r.number,
            title: r.title,
            labels: r.labels.into_iter().map(|l| l.name).collect(),
            state: r.state,
            updated_at: r.updated_at,
            url: r.url,
        })
        .collect())
}

/// Extract the new issue's URL + number from `gh issue create` stdout, which
/// prints the issue URL (possibly after a tip line). The number is the last
/// path segment.
fn parse_issue_ref(stdout: &str) -> Result<GhIssueRef, String> {
    let url = stdout
        .lines()
        .map(str::trim)
        .find(|l| l.contains("/issues/"))
        .ok_or("gh issue create: no issue URL in output")?;
    let number = url
        .rsplit('/')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| format!("gh issue create: cannot parse issue number from {url:?}"))?;
    Ok(GhIssueRef {
        number,
        url: url.to_string(),
    })
}

/// Build a `gh <kind> comment <n> --body <text>` argv (`kind` is "issue" or
/// "pr"). The body is the VALUE of `--body`, so — like `issue_create_args` — its
/// content is data, never a flag: a body starting with `-` can't be parsed as an
/// option (the leading-`-` convention shared across git.rs/gh.rs), and newlines
/// pass through intact.
fn comment_args(kind: &str, number: u64, body: &str) -> Vec<String> {
    vec![
        kind.into(),
        "comment".into(),
        number.to_string(),
        "--body".into(),
        body.into(),
    ]
}

/// Parse `gh {issue,pr} view --json …` into a `GhDetail`, flattening label and
/// author objects to their names/logins. An empty author login (or absent
/// author) becomes `None` rather than an empty string.
fn parse_detail(json: &str) -> Result<GhDetail, String> {
    let raw: RawDetail =
        serde_json::from_str(json).map_err(|e| format!("gh view: bad JSON: {e}"))?;
    let login = |a: Option<RawAuthor>| a.map(|a| a.login).filter(|s| !s.is_empty());
    Ok(GhDetail {
        title: raw.title,
        body: raw.body,
        labels: raw.labels.into_iter().map(|l| l.name).collect(),
        state: raw.state,
        author: login(raw.author),
        comments: raw
            .comments
            .into_iter()
            .map(|c| GhComment {
                author: login(c.author),
                created_at: c.created_at,
                body: c.body,
            })
            .collect(),
    })
}

/// Parse `gh pr list --json …` into `GhPr`s, flattening label objects to names.
fn parse_pr_list(json: &str) -> Result<Vec<GhPr>, String> {
    let raw: Vec<RawPr> =
        serde_json::from_str(json).map_err(|e| format!("gh pr list: bad JSON: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|r| GhPr {
            number: r.number,
            title: r.title,
            state: r.state,
            labels: r.labels.into_iter().map(|l| l.name).collect(),
            updated_at: r.updated_at,
            url: r.url,
            head_ref: r.head_ref,
        })
        .collect())
}

/// Build the `gh issue list` argv for [`gh_activity`].
///
/// The `--search sort:updated-desc` pin is the load-bearing part. `gh issue
/// list` / `gh pr list` return items in issue-NUMBER descending order — i.e.
/// newest-*created* first, not newest-*active* first (verified against gh
/// 2.95.0: a listing of this repo came back 633, 632, 628, 625, 622 with
/// non-monotonic `updatedAt`). Without the pin, a 12-hour window would silently
/// omit an old issue closed minutes ago as soon as `--limit` newer issues
/// exist — precisely the events a progress timeline is for. Sorting by activity
/// instead makes the page "the N most recently touched items", so the newest
/// window is always the covered one.
///
/// `--state all` is likewise required: the default (`open`) has no close/merge
/// events in it at all.
fn activity_issue_args(limit: usize) -> Vec<String> {
    vec![
        "issue".into(),
        "list".into(),
        "--state".into(),
        "all".into(),
        "--search".into(),
        "sort:updated-desc".into(),
        "--json".into(),
        "number,title,state,createdAt,closedAt,updatedAt,url".into(),
        "--limit".into(),
        limit.to_string(),
    ]
}

/// Build the `gh pr list` argv for [`gh_activity`]. Same ordering/state pin as
/// [`activity_issue_args`], plus `mergedAt` and `headRefName`.
fn activity_pr_args(limit: usize) -> Vec<String> {
    vec![
        "pr".into(),
        "list".into(),
        "--state".into(),
        "all".into(),
        "--search".into(),
        "sort:updated-desc".into(),
        "--json".into(),
        "number,title,state,createdAt,closedAt,mergedAt,updatedAt,url,headRefName".into(),
        "--limit".into(),
        limit.to_string(),
    ]
}

/// Normalize an optional gh timestamp to "absent" for the values that mean
/// absent but aren't `null`: an empty string, and Go's zero time
/// (`0001-01-01T00:00:00Z`), which gh has emitted for unset timestamps. Both
/// would otherwise become a real-looking event plotted at the far left of the
/// axis — the "never plot a fake instant" rule this view is built on.
fn absent_ts(ts: Option<String>) -> Option<String> {
    ts.filter(|s| !s.trim().is_empty() && !s.starts_with("0001-01-01"))
}

/// Parse `gh issue list --json …` (the activity field set) into
/// `GhIssueActivity`s.
fn parse_issue_activity(json: &str) -> Result<Vec<GhIssueActivity>, String> {
    let raw: Vec<RawIssueActivity> =
        serde_json::from_str(json).map_err(|e| format!("gh issue list (activity): bad JSON: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|r| GhIssueActivity {
            number: r.number,
            title: r.title,
            state: r.state,
            created_at: r.created_at,
            closed_at: absent_ts(r.closed_at),
            updated_at: r.updated_at,
            url: r.url,
        })
        .collect())
}

/// Parse `gh pr list --json …` (the activity field set) into `GhPrActivity`s.
fn parse_pr_activity(json: &str) -> Result<Vec<GhPrActivity>, String> {
    let raw: Vec<RawPrActivity> =
        serde_json::from_str(json).map_err(|e| format!("gh pr list (activity): bad JSON: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|r| GhPrActivity {
            number: r.number,
            title: r.title,
            state: r.state,
            created_at: r.created_at,
            closed_at: absent_ts(r.closed_at),
            merged_at: absent_ts(r.merged_at),
            updated_at: r.updated_at,
            url: r.url,
            head_ref: r.head_ref,
        })
        .collect())
}

/// Assemble the two lists into a [`GhActivity`], stamping the coverage
/// boundary. Split out from the command so the truncation rule is unit-tested
/// without a real `gh`.
fn assemble_activity(
    issues: Vec<GhIssueActivity>,
    prs: Vec<GhPrActivity>,
    limit: usize,
) -> GhActivity {
    GhActivity {
        issues_truncated: issues.len() >= limit,
        prs_truncated: prs.len() >= limit,
        issues,
        prs,
        limit,
    }
}

/// Pull the account name out of `gh auth status` text. Handles both the current
/// "Logged in to github.com account NAME (keyring)" and the older
/// "Logged in to github.com as NAME (oauth_token)" phrasings. Returns None when
/// unauthenticated (no such line) rather than failing.
fn parse_auth_login(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some((_, rest)) = line.split_once("Logged in to ") else {
            continue;
        };
        // rest e.g. "github.com account willem445 (keyring)" — take the token
        // after " account " or " as ", up to the next space or '('.
        let after = rest
            .split_once(" account ")
            .or_else(|| rest.split_once(" as "))
            .map(|(_, a)| a)?;
        let name = after
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
            .trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    None
}

// ---------- tests ----------
//
// All hermetic: fixtures are captured `gh` output, no network / no real gh.
// These are pure functions that don't link the lib, so they stay inline
// #[cfg(test)] unit tests (CLAUDE.md constraint 4 — integration-only rule —
// is unaffected).

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed but faithful `gh issue list --json …` blob (extra label fields
    // present, one issue with no labels, "OPEN" state, camelCase updatedAt).
    const LIST_FIXTURE: &str = r#"[
      {"labels":[
         {"id":"LA_1","name":"agent-managed","description":"Managed","color":"5319e7"},
         {"id":"LA_2","name":"agent-ready","description":"Ready","color":"d475bc"}],
       "number":120,"state":"OPEN",
       "title":"Add a task board \"delete all done\" button",
       "updatedAt":"2026-07-07T04:09:31Z",
       "url":"https://github.com/willem445/loomux/issues/120"},
      {"labels":[],"number":117,"state":"OPEN","title":"A spawned agent takes focus",
       "updatedAt":"2026-07-07T04:09:25Z",
       "url":"https://github.com/willem445/loomux/issues/117"}
    ]"#;

    #[test]
    fn parse_issue_list_flattens_labels_and_fields() {
        let issues = parse_issue_list(LIST_FIXTURE).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].number, 120);
        // Title with an embedded quote survives JSON decoding.
        assert_eq!(issues[0].title, "Add a task board \"delete all done\" button");
        assert_eq!(issues[0].labels, vec!["agent-managed", "agent-ready"]);
        assert_eq!(issues[0].state, "OPEN");
        assert_eq!(issues[0].updated_at, "2026-07-07T04:09:31Z");
        assert_eq!(issues[0].url, "https://github.com/willem445/loomux/issues/120");
        // An issue with no labels yields an empty vec, not a parse error.
        assert!(issues[1].labels.is_empty());
    }

    #[test]
    fn parse_issue_list_handles_empty_array() {
        assert!(parse_issue_list("[]").unwrap().is_empty());
    }

    #[test]
    fn parse_issue_list_rejects_garbage() {
        assert!(parse_issue_list("not json").is_err());
    }

    #[test]
    fn parse_issue_ref_extracts_number_and_url() {
        // gh prints the URL, sometimes after a tip line.
        let stdout = "\nhttps://github.com/willem445/loomux/issues/456\n";
        let r = parse_issue_ref(stdout).unwrap();
        assert_eq!(
            r,
            GhIssueRef {
                number: 456,
                url: "https://github.com/willem445/loomux/issues/456".to_string(),
            }
        );
    }

    #[test]
    fn parse_issue_ref_errors_without_url() {
        assert!(parse_issue_ref("Creating issue...\n").is_err());
    }

    // A faithful `gh issue view --json title,body,labels,state,author,comments`
    // blob: embedded quotes in the body, camelCase createdAt, a comment whose
    // author is null (deleted account), and one label.
    const DETAIL_FIXTURE: &str = r#"{
      "title":"Add a \"detail\" pane",
      "body":"First line\nSecond line",
      "labels":[{"name":"agent-ready","color":"0e8a16"}],
      "state":"OPEN",
      "author":{"login":"willem445","is_bot":false},
      "comments":[
        {"author":{"login":"octocat"},"createdAt":"2026-07-07T05:00:00Z","body":"nice"},
        {"author":null,"createdAt":"2026-07-07T06:00:00Z","body":"from a ghost"}
      ]
    }"#;

    #[test]
    fn parse_detail_flattens_author_labels_and_comments() {
        let d = parse_detail(DETAIL_FIXTURE).unwrap();
        assert_eq!(d.title, "Add a \"detail\" pane");
        // Body newlines survive verbatim (rendered pre-wrap on the frontend).
        assert_eq!(d.body, "First line\nSecond line");
        assert_eq!(d.labels, vec!["agent-ready"]);
        assert_eq!(d.state, "OPEN");
        assert_eq!(d.author.as_deref(), Some("willem445"));
        assert_eq!(d.comments.len(), 2);
        assert_eq!(d.comments[0].author.as_deref(), Some("octocat"));
        assert_eq!(d.comments[0].created_at, "2026-07-07T05:00:00Z");
        assert_eq!(d.comments[0].body, "nice");
        // A null author decodes to None, not a parse failure.
        assert_eq!(d.comments[1].author, None);
    }

    #[test]
    fn parse_detail_tolerates_missing_body_and_comments() {
        // An issue with an empty description and no comments (gh emits body:"" and
        // comments:[]); author with an empty login collapses to None.
        let json = r#"{"title":"t","body":"","labels":[],"state":"CLOSED",
                       "author":{"login":""},"comments":[]}"#;
        let d = parse_detail(json).unwrap();
        assert_eq!(d.body, "");
        assert!(d.comments.is_empty());
        assert!(d.labels.is_empty());
        assert_eq!(d.author, None);
    }

    #[test]
    fn parse_detail_rejects_garbage() {
        assert!(parse_detail("not json").is_err());
    }

    // A faithful `gh pr list --json …` blob: headRefName present, a MERGED state
    // is representable, and a PR with no labels.
    const PR_LIST_FIXTURE: &str = r#"[
      {"number":130,"title":"Umbrella PR","state":"OPEN",
       "labels":[{"name":"agent-managed"}],
       "updatedAt":"2026-07-07T04:09:31Z",
       "url":"https://github.com/willem445/loomux/pull/130",
       "headRefName":"orch/82-gh-issues"},
      {"number":128,"title":"Backend gh commands","state":"OPEN","labels":[],
       "updatedAt":"2026-07-07T03:00:00Z",
       "url":"https://github.com/willem445/loomux/pull/128",
       "headRefName":"feat/82-backend"}
    ]"#;

    #[test]
    fn parse_pr_list_flattens_labels_and_head_ref() {
        let prs = parse_pr_list(PR_LIST_FIXTURE).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 130);
        assert_eq!(prs[0].title, "Umbrella PR");
        assert_eq!(prs[0].labels, vec!["agent-managed"]);
        assert_eq!(prs[0].head_ref, "orch/82-gh-issues");
        assert_eq!(prs[0].url, "https://github.com/willem445/loomux/pull/130");
        assert!(prs[1].labels.is_empty());
        assert_eq!(prs[1].head_ref, "feat/82-backend");
    }

    #[test]
    fn parse_pr_list_handles_empty_and_garbage() {
        assert!(parse_pr_list("[]").unwrap().is_empty());
        assert!(parse_pr_list("not json").is_err());
    }

    #[test]
    fn comment_args_keeps_body_as_data() {
        // A body starting with '-' must remain the VALUE of --body (never a flag),
        // and newlines pass through — the arg-vector form guarantees both.
        let args = comment_args("issue", 82, "-not a flag\nsecond line");
        assert_eq!(
            args,
            vec![
                "issue",
                "comment",
                "82",
                "--body",
                "-not a flag\nsecond line",
            ]
        );
        // PR path differs only in the leading subcommand.
        assert_eq!(comment_args("pr", 130, "hi")[0], "pr");
    }

    #[test]
    fn issue_comment_rejects_empty_body_before_spawning() {
        // Validation happens before any gh spawn, so this fails fast with no gh /
        // no repo — a whitespace-only body would otherwise open gh's editor.
        let err =
            gh_issue_comment_sync("C:/nonexistent".to_string(), 1, "   \n".to_string()).unwrap_err();
        assert!(err.contains("empty comment"), "got: {err}");
    }

    #[test]
    fn pr_comment_rejects_empty_body_before_spawning() {
        let err = gh_pr_comment_sync("C:/nonexistent".to_string(), 1, "".to_string()).unwrap_err();
        assert!(err.contains("empty comment"), "got: {err}");
    }

    /// The default vocabulary — what `allowed_labels` resolves to for a repo
    /// with no workflow file, and what every pre-#778 caller effectively used.
    fn default_allowed() -> Vec<String> {
        allowed_labels("C:/nonexistent-repo-with-no-workflow")
    }

    #[test]
    fn validate_labels_allows_only_go_signals() {
        // Every allow-listed label passes.
        let allowed = default_allowed();
        for ok in &allowed {
            assert!(validate_labels(&allowed, &[ok.to_string()]).is_ok(), "{ok}");
        }
        // The #778 veto label is writable from the issues view: it is the human's
        // one-click hold gesture under full autonomy, so a rejection here would
        // leave the consent boundary with no UI at all.
        assert!(validate_labels(&allowed, &["agent-hold".to_string()]).is_ok());
        // A plausible-but-wrong label (the plan's misspelling) is rejected — it
        // isn't the real repo label, so writing it would fail at gh anyway.
        assert!(validate_labels(&allowed, &["agent-investigate".to_string()]).is_err());
        // Near-misses of the hold label are still rejected: only the exact
        // spelling the poller and the contract use may be written.
        assert!(validate_labels(&allowed, &["human-only".to_string()]).is_err());
        assert!(validate_labels(&allowed, &["agent-held".to_string()]).is_err());
        // Arbitrary labels are rejected outright.
        assert!(validate_labels(&allowed, &["bug".to_string()]).is_err());
        // A mixed set fails if any entry is disallowed.
        assert!(validate_labels(&allowed, &["agent-ready".into(), "wontfix".into()]).is_err());
    }

    /// **The allow-list is still an allow-list once the hold spelling is
    /// repo-resolved (#778).** Widening it to "any label the frontend sends" is
    /// the failure this whole module's trust boundary exists to prevent, and
    /// "resolve a value from config" is one refactor away from it — so the
    /// closed-ness is pinned directly, against a vocabulary that is NOT the
    /// default.
    #[test]
    fn a_resolved_hold_spelling_widens_the_allow_list_by_exactly_one_value() {
        let renamed: Vec<String> =
            FIXED_LABELS.iter().map(|s| s.to_string()).chain(["do-not-touch".to_string()]).collect();

        assert!(validate_labels(&renamed, &["do-not-touch".to_string()]).is_ok());
        // The BUILT-IN spelling is not silently kept alongside the rename: the
        // poller honors the repo's spelling and `agent-hold` "then means
        // nothing", so writing it from the UI would be a no-op veto — the exact
        // silent failure the rename support exists to prevent.
        assert!(validate_labels(&renamed, &["agent-hold".to_string()]).is_err());
        // Everything else stays shut: no wildcard, no prefix rule, no "any label
        // that looks like a hold".
        for nope in ["do-not-touch-me", "do_not_touch", "DO-NOT-TOUCH-X", "bug", "--force"] {
            assert!(
                validate_labels(&renamed, &[nope.to_string()]).is_err(),
                "{nope} must not be writable"
            );
        }
        // And the three fixed signals are unaffected by a rename.
        for fixed in FIXED_LABELS {
            assert!(validate_labels(&renamed, &[fixed.to_string()]).is_ok(), "{fixed}");
        }
    }

    /// A repo with no workflow file (and one whose file is unreadable) resolves
    /// to the built-in veto, so the default install is byte-identical to the
    /// pre-#778 four-label set. The fallback direction matters: failing closed
    /// here would remove the veto gesture over an unrelated typo in the file.
    #[test]
    fn a_repo_without_a_workflow_file_gets_the_builtin_hold_label() {
        assert_eq!(hold_label("C:/nonexistent-repo-with-no-workflow"), "agent-hold");
        assert_eq!(
            default_allowed(),
            vec!["agent-ready", "agent-investigation", "agent-managed", "agent-hold"]
        );
    }

    /// Write `yaml` as a repo's `.loomux/workflow.yml` and hand back the repo
    /// root. `tempfile` is the dev-dependency with `default-features = false`
    /// (CLAUDE.md constraint 2 — the getrandom feature is what breaks the
    /// Windows 10 baseline), so this costs no new dependency.
    fn repo_with_workflow(yaml: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".loomux");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("workflow.yml"), yaml).unwrap();
        dir
    }

    /// **The write side of the rename, read from a real file.** This is what
    /// makes the issues view's one-click veto land on the label the repo's own
    /// poller honors: `gh.rs` has no group to ask, so it resolves the spelling
    /// from the same `.loomux/workflow.yml` `create_group` parses.
    #[test]
    fn a_declared_hold_spelling_is_resolved_from_the_repos_workflow_file() {
        let dir = repo_with_workflow(
            "version: 1\nblocks:\n  - id: worker\n    kind: worker\n\
             intake:\n  labels:\n    hold: do-not-touch\n",
        );
        let repo = dir.path().to_string_lossy().to_string();

        assert_eq!(hold_label(&repo), "do-not-touch");
        assert_eq!(gh_label_vocabulary_sync(&repo).hold, "do-not-touch");
        // The write path agrees with what the view was told — one resolution,
        // so the button and the allow-list cannot disagree.
        let allowed = allowed_labels(&repo);
        assert!(validate_labels(&allowed, &["do-not-touch".to_string()]).is_ok());
        assert!(
            validate_labels(&allowed, &["agent-hold".to_string()]).is_err(),
            "the built-in means nothing to this repo's poller, so writing it would be a no-op veto"
        );
        // ...and the label can be CREATED under its own name on a fresh repo.
        assert!(label_spec_for(&repo, "do-not-touch").is_some());

        // The go-signals are reported as built-ins, not as anything the file
        // says: #382 P2 has not landed, and claiming otherwise here would be the
        // same overclaim this whole change is fixing, pointed the other way.
        let vocab = gh_label_vocabulary_sync(&repo);
        assert_eq!(vocab.ready, "agent-ready");
        assert_eq!(vocab.investigate, "agent-investigation");
    }

    /// A file that does not parse falls back to the built-in rather than
    /// leaving the repo with no veto at all — the same direction `create_group`
    /// takes when it drops to the built-in roster, so the UI keeps writing what
    /// that repo's poller is actually watching.
    #[test]
    fn a_broken_workflow_file_falls_back_to_the_builtin_veto() {
        let dir = repo_with_workflow("this: is not: a valid workflow\n  - at all\n");
        let repo = dir.path().to_string_lossy().to_string();
        assert_eq!(hold_label(&repo), "agent-hold");
        assert!(validate_labels(&allowed_labels(&repo), &["agent-hold".to_string()]).is_ok());
    }

    /// A file that declares an `intake:` block but no `hold:` key inherits the
    /// built-in — the migration case every other label field already has, and
    /// the one where an empty-string fallback would produce a veto that matches
    /// nothing at all.
    #[test]
    fn an_intake_block_without_a_hold_key_inherits_the_builtin_veto() {
        let dir = repo_with_workflow(
            "version: 1\nblocks:\n  - id: worker\n    kind: worker\n\
             intake:\n  labels:\n    ready: go-build-it\n",
        );
        assert_eq!(hold_label(&dir.path().to_string_lossy()), "agent-hold");
    }

    #[test]
    fn issue_edit_args_pairs_each_label_with_its_flag() {
        let args = issue_edit_args(
            42,
            &["agent-ready".to_string()],
            &["agent-managed".to_string()],
        );
        assert_eq!(
            args,
            vec![
                "issue",
                "edit",
                "42",
                "--add-label",
                "agent-ready",
                "--remove-label",
                "agent-managed",
            ]
        );
    }

    #[test]
    fn every_allowed_label_has_a_create_spec() {
        // ensure_labels_exist relies on this: a validated (allow-listed) label
        // must always have a color/description to create it with, or a fresh
        // repo could accept the label past validation yet fail to create it.
        // Asked of `label_spec_for`, because the hold entry's name is now
        // repo-resolved and `label_spec` alone can no longer answer for it.
        let repo = "C:/nonexistent-repo-with-no-workflow";
        for l in default_allowed() {
            let spec = label_spec_for(repo, &l);
            assert!(spec.is_some(), "{l} has no create spec");
            let (color, desc) = spec.unwrap();
            assert_eq!(color.len(), 6, "{l} color must be 6 hex digits: {color:?}");
            assert!(
                color.chars().all(|c| c.is_ascii_hexdigit()),
                "{l} color not hex: {color:?}"
            );
            assert!(!desc.is_empty(), "{l} has empty description");
        }
        // agent-managed keeps the orchestrator template's exact convention so a
        // loomux-created label matches one the orchestrator would create.
        assert_eq!(
            label_spec("agent-managed"),
            Some(("5319e7", "Managed by a loomux orchestrator"))
        );
        // The veto (#778) is created RED, and its description states the rule it
        // enforces: a repo that has never seen the label gets one whose meaning is
        // legible on GitHub without reading the orchestrator contract.
        let red = Some(("b60205", "Held by the human — full-autonomy agents must not start this"));
        assert_eq!(label_spec_for(repo, "agent-hold"), red);
        // Non-allow-listed names have no spec (defense in depth vs. arbitrary
        // label creation) — including a would-be hold spelling this repo did not
        // declare, which is what stops `label_spec_for` from being a create
        // permit for any label at all.
        assert!(label_spec_for(repo, "bug").is_none());
        assert!(label_spec_for(repo, "do-not-touch").is_none());
        assert!(label_spec("agent-hold").is_none(), "the literal is no longer a fixed spec");
    }

    #[test]
    fn label_create_args_keeps_fields_as_data() {
        // A description with spaces / an em-dash / punctuation must remain the
        // value of --description, and the color must not carry a '#'.
        let args = label_create_args(
            "agent-investigation",
            "fbca04",
            "Research only — findings as an issue comment; no code",
        );
        assert_eq!(
            args,
            vec![
                "label",
                "create",
                "agent-investigation",
                "--color",
                "fbca04",
                "--description",
                "Research only — findings as an issue comment; no code",
            ]
        );
    }

    #[test]
    fn parse_label_names_flattens() {
        let json = r#"[{"name":"agent-ready"},{"name":"bug"},{"name":"agent-managed"}]"#;
        assert_eq!(
            parse_label_names(json).unwrap(),
            vec!["agent-ready", "bug", "agent-managed"]
        );
        assert!(parse_label_names("[]").unwrap().is_empty());
        assert!(parse_label_names("not json").is_err());
    }

    #[test]
    fn is_label_exists_error_detects_race() {
        // The success-on-race path: a create that failed only because the label
        // was created concurrently.
        assert!(is_label_exists_error(
            "failed to create label: 'agent-ready' already exists"
        ));
        assert!(is_label_exists_error("Label Already Exists")); // case-insensitive
        // A genuine failure is not swallowed.
        assert!(!is_label_exists_error("HTTP 403: Resource not accessible"));
    }

    #[test]
    fn map_label_create_error_flags_permission_case() {
        // 403 / not-accessible / must-have / permission all read as a perms
        // problem and get the actionable hint.
        for perm in [
            "HTTP 403: Resource not accessible by integration",
            "GraphQL: Must have push access to create a label",
            "you do not have permission to manage labels",
        ] {
            let msg = map_label_create_error("agent-ready", perm);
            assert!(msg.contains("lacks permission"), "got: {msg}");
            assert!(msg.contains("agent-ready"), "got: {msg}");
        }
        // A non-permission failure keeps gh's own text so it stays diagnosable.
        let net = map_label_create_error("agent-ready", "dial tcp: lookup api.github.com: no such host");
        assert!(net.contains("no such host"), "got: {net}");
        assert!(!net.contains("lacks permission"), "got: {net}");
    }

    // ----- ensure_labels_with: a fake `gh` runner records every argv and
    // returns scripted stdout/stderr, so the whole ensure flow is hermetic. -----

    /// Build a runner from a closure and a shared call-log. The closure sees the
    /// The spec lookup these ensure-flow tests pass: the fixed table plus the
    /// built-in veto, i.e. what `label_spec_for` resolves to for a repo with no
    /// workflow file. Keeps the flow tests about the FLOW (list, create, error
    /// handling) rather than about label resolution, which its own tests cover.
    fn builtin_spec(name: &str) -> Option<(&'static str, &'static str)> {
        label_spec_for("C:/nonexistent-repo-with-no-workflow", name)
    }

    /// argv (joined with spaces for easy matching) and returns Ok(stdout)/Err(stderr).
    fn runner<'a>(
        calls: &'a std::cell::RefCell<Vec<String>>,
        mut reply: impl FnMut(&str) -> Result<String, String> + 'a,
    ) -> impl FnMut(&[&str]) -> Result<String, String> + 'a {
        move |args: &[&str]| {
            let joined = args.join(" ");
            calls.borrow_mut().push(joined.clone());
            reply(&joined)
        }
    }

    #[test]
    fn ensure_creates_only_missing_labels() {
        let calls = std::cell::RefCell::new(Vec::new());
        let run = runner(&calls, |argv| {
            if argv.starts_with("label list") {
                // Repo already has agent-ready (only).
                Ok(r#"[{"name":"agent-ready"}]"#.to_string())
            } else {
                Ok(String::new()) // create succeeds
            }
        });
        ensure_labels_with(&["agent-ready".into(), "agent-managed".into()], builtin_spec, run)
            .unwrap();
        let calls = calls.into_inner();
        // agent-ready exists → no create; agent-managed missing → created.
        assert!(calls.iter().any(|c| c.starts_with("label list")));
        assert!(!calls.iter().any(|c| c.contains("create agent-ready")));
        assert!(calls.iter().any(|c| c.contains("create agent-managed")));
    }

    #[test]
    fn ensure_matches_existing_label_case_insensitively() {
        // GitHub label names are case-insensitively unique: an existing
        // "Agent-Ready" satisfies an add of "agent-ready" — no doomed create.
        let calls = std::cell::RefCell::new(Vec::new());
        let run = runner(&calls, |argv| {
            if argv.starts_with("label list") {
                Ok(r#"[{"name":"Agent-Ready"}]"#.to_string())
            } else {
                panic!("must not attempt to create an already-present label");
            }
        });
        ensure_labels_with(&["agent-ready".into()], builtin_spec, run).unwrap();
        assert!(!calls.borrow().iter().any(|c| c.contains("create")));
    }

    #[test]
    fn ensure_proceeds_when_list_fails_and_label_exists() {
        // The regression the reviewer flagged: a transient `gh label list`
        // failure must not abort a toggle on a repo that already has the label.
        // List blips; the fallback create returns "already exists" → success.
        let calls = std::cell::RefCell::new(Vec::new());
        let run = runner(&calls, |argv| {
            if argv.starts_with("label list") {
                Err("HTTP 502: Bad Gateway".to_string())
            } else {
                Err("failed to create label: 'agent-ready' already exists".to_string())
            }
        });
        // Ok, not Err — the toggle proceeds to the edit.
        ensure_labels_with(&["agent-ready".into()], builtin_spec, run).unwrap();
        // We still attempted a best-effort create after the failed list.
        assert!(calls.borrow().iter().any(|c| c.contains("create agent-ready")));
    }

    #[test]
    fn ensure_swallows_create_error_when_list_unreliable() {
        // List failed, so we can't trust that the label is missing. Even a
        // permission-looking create error is swallowed — `gh issue edit` is left
        // to report the real outcome rather than blocking here.
        let run = runner_noop(|argv| {
            if argv.starts_with("label list") {
                Err("network is unreachable".to_string())
            } else {
                Err("HTTP 403: Resource not accessible by integration".to_string())
            }
        });
        assert!(ensure_labels_with(&["agent-managed".into()], builtin_spec, run).is_ok());
    }

    #[test]
    fn ensure_surfaces_create_error_only_when_absence_confirmed() {
        // List succeeded and showed the label absent, then create genuinely
        // failed on permissions → surface the friendly, actionable message.
        let run = runner_noop(|argv| {
            if argv.starts_with("label list") {
                Ok("[]".to_string()) // reliably empty → label really is missing
            } else {
                Err("HTTP 403: Resource not accessible by integration".to_string())
            }
        });
        let err = ensure_labels_with(&["agent-managed".into()], builtin_spec, run).unwrap_err();
        assert!(err.contains("lacks permission"), "got: {err}");
        assert!(err.contains("agent-managed"), "got: {err}");
    }

    /// **A renamed veto must be CREATABLE, not just writable.** `gh issue edit
    /// --add-label` fails outright on a label the repo has never defined, so a
    /// repo that renamed the hold and has never applied it would see its
    /// one-click veto fail on first use — the silent-veto failure again, one
    /// layer down from the allow-list. The spec lookup is repo-resolved for
    /// exactly this, and the created label keeps the red/meaning-stating spec
    /// under whatever name the repo chose.
    #[test]
    fn a_renamed_hold_label_is_created_with_the_veto_spec() {
        let calls = std::cell::RefCell::new(Vec::new());
        let run = runner(&calls, |argv| {
            if argv.starts_with("label list") {
                Ok("[]".to_string()) // fresh repo: nothing defined yet
            } else {
                Ok(String::new())
            }
        });
        // The spec lookup a repo declaring `hold: do-not-touch` would produce.
        let spec = |name: &str| {
            label_spec(name).or_else(|| {
                (name == "do-not-touch")
                    .then_some(("b60205", "Held by the human — full-autonomy agents must not start this"))
            })
        };
        ensure_labels_with(&["do-not-touch".into()], spec, run).unwrap();
        let calls = calls.into_inner();
        assert!(
            calls.iter().any(|c| c.contains("create do-not-touch") && c.contains("b60205")),
            "the renamed veto must be created, with the veto spec: {calls:?}"
        );
    }

    /// A runner with no call-log, for tests that only care about the return value.
    fn runner_noop(
        reply: impl FnMut(&str) -> Result<String, String>,
    ) -> impl FnMut(&[&str]) -> Result<String, String> {
        let mut reply = reply;
        move |args: &[&str]| reply(&args.join(" "))
    }

    #[test]
    fn issue_create_args_keeps_title_and_body_as_data() {
        // A title that starts with '-' must remain the value of --title, never a
        // flag; the arg-vector form guarantees that.
        let args = issue_create_args("-weird title", "body\nwith newline");
        assert_eq!(
            args,
            vec![
                "issue",
                "create",
                "--title",
                "-weird title",
                "--body",
                "body\nwith newline",
            ]
        );
    }

    #[test]
    fn set_labels_rejects_bad_label_before_spawning() {
        // Validation happens before any gh spawn, so this fails fast even with
        // no gh / no repo present — proving the allow-list is the gate.
        let err = gh_issue_set_labels_sync(
            "C:/nonexistent".to_string(),
            1,
            vec!["definitely-not-allowed".to_string()],
            vec![],
        )
        .unwrap_err();
        assert!(err.contains("label not allowed"), "got: {err}");
    }

    #[test]
    fn set_labels_noop_when_no_deltas() {
        // Empty add+remove is a success no-op (must not spawn an interactive
        // editor), regardless of repo validity.
        assert!(gh_issue_set_labels_sync("C:/nonexistent".to_string(), 1, vec![], vec![]).is_ok());
    }

    #[test]
    fn create_rejects_empty_title_before_spawning() {
        let err =
            gh_issue_create_sync("C:/nonexistent".to_string(), "   ".to_string(), "b".to_string())
                .unwrap_err();
        assert!(err.contains("empty issue title"), "got: {err}");
    }

    // ----- gh_activity (#608 progress timeline) -----

    // A faithful `gh issue list --state all --json …` blob for the activity
    // field set: one open issue (closedAt null) and one closed one.
    const ISSUE_ACTIVITY_FIXTURE: &str = r#"[
      {"number":608,"title":"Workflow visualization pane","state":"OPEN",
       "createdAt":"2026-07-31T12:00:00Z","closedAt":null,
       "updatedAt":"2026-08-01T17:00:00Z",
       "url":"https://github.com/willem445/loomux/issues/608"},
      {"number":590,"title":"A pane cannot take a notice","state":"CLOSED",
       "createdAt":"2026-07-30T09:00:00Z","closedAt":"2026-08-01T16:30:00Z",
       "updatedAt":"2026-08-01T16:30:00Z",
       "url":"https://github.com/willem445/loomux/issues/590"}
    ]"#;

    #[test]
    fn parse_issue_activity_keeps_open_and_closed_instants_apart() {
        let issues = parse_issue_activity(ISSUE_ACTIVITY_FIXTURE).unwrap();
        assert_eq!(issues.len(), 2);
        // An open issue contributes exactly one point (opened) — a null
        // closedAt must stay absent, never become a bogus close event.
        assert_eq!(issues[0].number, 608);
        assert_eq!(issues[0].created_at, "2026-07-31T12:00:00Z");
        assert_eq!(issues[0].closed_at, None);
        assert_eq!(issues[0].updated_at, "2026-08-01T17:00:00Z");
        assert_eq!(issues[0].state, "OPEN");
        // A closed issue contributes two points at two different instants.
        assert_eq!(issues[1].created_at, "2026-07-30T09:00:00Z");
        assert_eq!(issues[1].closed_at.as_deref(), Some("2026-08-01T16:30:00Z"));
    }

    // `gh pr list --state all --json …`: an open PR, a merged one, and — the
    // case that matters — one CLOSED WITHOUT MERGING.
    const PR_ACTIVITY_FIXTURE: &str = r#"[
      {"number":643,"title":"Slice A","state":"OPEN",
       "createdAt":"2026-08-01T17:00:00Z","closedAt":null,"mergedAt":null,
       "updatedAt":"2026-08-01T17:51:30Z",
       "url":"https://github.com/willem445/loomux/pull/643","headRefName":"feat/608-viz-slice-a"},
      {"number":638,"title":"multi-row mask","state":"MERGED",
       "createdAt":"2026-08-01T15:00:00Z","closedAt":"2026-08-01T17:31:40Z",
       "mergedAt":"2026-08-01T17:31:40Z","updatedAt":"2026-08-01T17:31:40Z",
       "url":"https://github.com/willem445/loomux/pull/638","headRefName":"harden/632"},
      {"number":639,"title":"abandoned attempt","state":"CLOSED",
       "createdAt":"2026-08-01T14:00:00Z","closedAt":"2026-08-01T17:00:55Z",
       "mergedAt":null,"updatedAt":"2026-08-01T17:01:04Z",
       "url":"https://github.com/willem445/loomux/pull/639","headRefName":"spike/639"}
    ]"#;

    #[test]
    fn parse_pr_activity_never_reads_a_closed_pr_as_merged() {
        let prs = parse_pr_activity(PR_ACTIVITY_FIXTURE).unwrap();
        assert_eq!(prs.len(), 3);
        // Open: neither ending happened yet.
        assert_eq!(prs[0].closed_at, None);
        assert_eq!(prs[0].merged_at, None);
        assert_eq!(prs[0].head_ref, "feat/608-viz-slice-a");
        // Merged: GitHub closes a PR when it merges it, so BOTH are set — the
        // timeline must key "merged" off merged_at, and it is present.
        assert_eq!(prs[1].merged_at.as_deref(), Some("2026-08-01T17:31:40Z"));
        assert_eq!(prs[1].closed_at.as_deref(), Some("2026-08-01T17:31:40Z"));
        // Closed unmerged: closed_at set, merged_at absent. Reading state or
        // closed_at as "merged" would invent a merge that never happened —
        // this is the whole reason mergedAt is in the pinned field set.
        assert_eq!(prs[2].closed_at.as_deref(), Some("2026-08-01T17:00:55Z"));
        assert_eq!(prs[2].merged_at, None);
    }

    #[test]
    fn activity_parsers_treat_placeholder_timestamps_as_absent() {
        // gh has emitted Go's zero time for an unset timestamp instead of null,
        // and an empty string is equally "not a time". Either would plot as a
        // real event at year 1 — a merge that never happened, at the far left
        // of the axis. Both must decode to None.
        let json = r#"[{"number":1,"title":"t","state":"OPEN",
           "createdAt":"2026-08-01T10:00:00Z","closedAt":"0001-01-01T00:00:00Z",
           "mergedAt":"","updatedAt":"2026-08-01T10:00:00Z","url":"u","headRefName":"h"}]"#;
        let prs = parse_pr_activity(json).unwrap();
        assert_eq!(prs[0].closed_at, None);
        assert_eq!(prs[0].merged_at, None);
    }

    #[test]
    fn activity_parsers_survive_a_missing_optional_field() {
        // A gh field rename must cost that one column, not the whole timeline:
        // an entry with no createdAt/updatedAt still parses, leaving empty
        // strings the frontend parks as "undatable" rather than plotting.
        let issues = parse_issue_activity(r#"[{"number":7,"state":"OPEN"}]"#).unwrap();
        assert_eq!(issues[0].number, 7);
        assert_eq!(issues[0].created_at, "");
        assert_eq!(issues[0].closed_at, None);
    }

    #[test]
    fn activity_parsers_handle_empty_and_garbage() {
        assert!(parse_issue_activity("[]").unwrap().is_empty());
        assert!(parse_pr_activity("[]").unwrap().is_empty());
        assert!(parse_issue_activity("not json").is_err());
        assert!(parse_pr_activity("not json").is_err());
    }

    #[test]
    fn activity_args_pin_state_all_and_activity_ordering() {
        // Both pins are load-bearing and neither is gh's default:
        //  - `--state all`: the default `open` contains no close/merge events,
        //    so a timeline built on it can only ever show openings.
        //  - `sort:updated-desc`: gh lists by issue NUMBER descending (newest
        //    CREATED first). Without this, an old issue closed minutes ago
        //    falls off the page as soon as `--limit` newer items exist — it
        //    would silently vanish from the default 12h window.
        for args in [activity_issue_args(100), activity_pr_args(100)] {
            let pos = |flag: &str| args.iter().position(|a| a == flag);
            let value_after = |flag: &str| pos(flag).and_then(|i| args.get(i + 1)).cloned();
            assert_eq!(value_after("--state").as_deref(), Some("all"), "{args:?}");
            assert_eq!(
                value_after("--search").as_deref(),
                Some("sort:updated-desc"),
                "{args:?}"
            );
            assert_eq!(value_after("--limit").as_deref(), Some("100"), "{args:?}");
        }
        // The pinned field sets: the timestamps a time axis plots must be
        // requested, or every event silently decodes to absent.
        let json_fields = |args: Vec<String>| {
            args.iter()
                .position(|a| a == "--json")
                .and_then(|i| args.get(i + 1))
                .expect("--json must carry a field set")
                .clone()
        };
        let issue_fields = json_fields(activity_issue_args(1));
        for f in ["number", "createdAt", "closedAt", "updatedAt", "url"] {
            assert!(issue_fields.contains(f), "issue --json missing {f}");
        }
        let pr_fields = json_fields(activity_pr_args(1));
        for f in ["createdAt", "closedAt", "mergedAt", "updatedAt", "headRefName"] {
            assert!(pr_fields.contains(f), "pr --json missing {f}");
        }
        // Subcommands, so the two argv builders can't be swapped unnoticed.
        assert_eq!(activity_issue_args(1)[0], "issue");
        assert_eq!(activity_pr_args(1)[0], "pr");
    }

    #[test]
    fn assemble_activity_reports_the_cap_it_hit() {
        // Under the cap: nothing is hidden, so nothing is claimed to be.
        let short = assemble_activity(vec![], vec![], 2);
        assert_eq!(short.limit, 2);
        assert!(!short.issues_truncated);
        assert!(!short.prs_truncated);

        // A full page means older activity may exist past it. Reporting that is
        // the whole point — a chart that looks complete when it isn't is the
        // "no silent caps" failure this repo has been burned by.
        let issue = |n: u64| GhIssueActivity {
            number: n,
            title: String::new(),
            state: "OPEN".into(),
            created_at: "2026-08-01T00:00:00Z".into(),
            closed_at: None,
            updated_at: "2026-08-01T00:00:00Z".into(),
            url: String::new(),
        };
        let full = assemble_activity(vec![issue(1), issue(2)], vec![], 2);
        assert!(full.issues_truncated, "a full page must report its boundary");
        // The two lists are bounded independently — a full issue page says
        // nothing about the PR page.
        assert!(!full.prs_truncated);
        assert_eq!(full.issues.len(), 2);
    }

    #[test]
    fn activity_limit_is_the_value_the_frontend_is_told() {
        // The cap crosses the wire (GhActivity::limit) instead of being
        // duplicated as a frontend constant, so the coverage note the view
        // renders can never disagree with the query that produced the data.
        let args = activity_issue_args(ACTIVITY_LIMIT);
        let limit_arg = args
            .iter()
            .position(|a| a == "--limit")
            .and_then(|i| args.get(i + 1))
            .unwrap();
        assert_eq!(limit_arg, &ACTIVITY_LIMIT.to_string());
        assert_eq!(assemble_activity(vec![], vec![], ACTIVITY_LIMIT).limit, ACTIVITY_LIMIT);
    }

    #[test]
    fn parse_auth_login_current_and_legacy_phrasings() {
        let current = "github.com\n  \u{2713} Logged in to github.com account willem445 (keyring)\n  - Active account: true\n";
        assert_eq!(parse_auth_login(current).as_deref(), Some("willem445"));

        let legacy = "\u{2713} Logged in to github.com as octocat (oauth_token)\n";
        assert_eq!(parse_auth_login(legacy).as_deref(), Some("octocat"));

        let logged_out = "You are not logged into any GitHub hosts. Run gh auth login to authenticate.\n";
        assert_eq!(parse_auth_login(logged_out), None);
    }

    // ----- off-the-main-thread dispatch (#716) -----

    #[test]
    fn run_blocking_runs_the_work_on_another_thread() {
        // The whole point of the wrapper: the caller's thread — the webview
        // main thread in production — must not be the one that runs the `gh`
        // spawn. Asserting the closure observes a DIFFERENT thread id is the
        // only direct evidence of that; `async fn` alone proves nothing, since
        // an async command whose body still ran inline would be just as frozen.
        let caller = std::thread::current().id();
        let worker: std::thread::ThreadId =
            tauri::async_runtime::block_on(run_blocking(move || Ok(std::thread::current().id())))
                .unwrap();
        assert_ne!(
            worker, caller,
            "run_blocking executed the closure on the calling thread — the GUI freeze (#716) is back"
        );
        // Errors still propagate through unchanged: the wrapper is transparent
        // to the Result the sync body returns, which is what lets every command
        // keep its exact contract.
        let err: Result<(), String> =
            tauri::async_runtime::block_on(run_blocking(|| Err("boom".to_string())));
        assert_eq!(err.unwrap_err(), "boom");
    }

    #[test]
    fn every_tauri_command_in_this_module_is_async_and_delegates() {
        // #716's claim is about EVERY gh-backed command, and a single sync
        // straggler would keep the freeze while the module's doc claimed
        // otherwise — so this scans this file's own source rather than trusting
        // a hand transcription (the `tests/acl_manifest.rs` precedent, which
        // parses `generate_handler!` out of `src/lib.rs` for the same reason).
        //
        // Bound of the claim, stated rather than implied: this covers the `gh`
        // module only. It is the whole gh-on-the-GUI-thread surface because the
        // two spawn entry points here (`gh_output`, `run_gh`) are private to
        // this module, so no command elsewhere can reach a `gh` spawn through
        // them; the crate's other `gh` spawns (`orchestration`'s `gh_capture`,
        // `mqdriver`'s `ProcessRunner::gh`) run on the poll thread, not a
        // command.
        //
        // Split so the literal never appears as a whole line in this file —
        // otherwise the scan would find its own source and mis-report.
        const ATTR: &str = concat!("#[tauri::", "command]");
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/gh.rs"))
            .expect("read src/gh.rs");

        let lines: Vec<&str> = src.lines().map(str::trim).collect();
        let mut found: Vec<String> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if *line != ATTR {
                continue;
            }
            let at = i + 1
                + lines[i + 1..]
                    .iter()
                    .position(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#'))
                    .unwrap_or_else(|| panic!("{ATTR} at line {} has no function after it", i + 1));
            let sig = lines[at];
            assert!(
                sig.starts_with("pub async fn "),
                "line {}: `{sig}` is a synchronous #[tauri::command] — Tauri dispatches it on the \
                 webview main thread, so its `gh` spawn freezes the GUI for the whole round trip \
                 (#716). Make it a thin `pub async fn` over `run_blocking`.",
                at + 1
            );

            // `async` alone is NOT the property. An `async fn` whose body still
            // called the sync work inline would satisfy the check above and
            // freeze the GUI exactly as before — Tauri polls a command's future
            // on the main thread, so work done before the first real await point
            // runs there. The delegation to `run_blocking` is what actually
            // moves it, so require it in the command's OWN body: from the
            // signature to the first top-level `}` (these wrappers are one
            // expression, so a nested block that stopped this scan early would
            // itself be a shape worth failing on).
            let end = at
                + 1
                + lines[at + 1..]
                    .iter()
                    .position(|l| *l == "}")
                    .unwrap_or_else(|| panic!("no top-level `}}` closing the fn at line {}", at + 1));
            assert!(
                lines[at..end].iter().any(|l| l.contains("run_blocking(")),
                "line {}: `{sig}` is async but its body never calls `run_blocking(` — an async \
                 command that runs its `gh` spawn inline is polled on the webview main thread and \
                 freezes the GUI just as a sync one does (#716). Hand the body to \
                 `run_blocking(move || …_sync(…)).await`.",
                at + 1
            );

            let name = sig["pub async fn ".len()..]
                .split(|c: char| c == '(' || c.is_whitespace())
                .next()
                .unwrap_or_default()
                .to_string();
            found.push(name);
        }

        found.sort();
        let mut expected = vec![
            "gh_activity",
            "gh_auth_status",
            "gh_issue_comment",
            "gh_issue_create",
            "gh_issue_list",
            "gh_issue_set_labels",
            "gh_issue_view",
            // #778: reads the repo's workflow file rather than spawning `gh`,
            // and is still async over `run_blocking` — a file read on the paint
            // thread is the same freeze in miniature, and the module's rule is
            // about the thread, not about which subprocess is involved.
            "gh_label_vocabulary",
            "gh_pr_comment",
            "gh_pr_list",
            "gh_pr_view",
        ];
        expected.sort();
        assert_eq!(
            found, expected,
            "the set of #[tauri::command]s in gh.rs changed — a new one must be async over \
             run_blocking (see the module note) and listed here, so the enumeration this test \
             pins can't silently go stale"
        );
        // NB: this equality is also the scan's own vacuity guard — a marker
        // that stopped matching (a formatting change, a renamed attribute path)
        // yields an empty `found` and fails here, rather than letting the
        // per-command assertions above pass vacuously over nothing.
    }
}
