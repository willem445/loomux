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

/// Labels the issues view is permitted to add/remove. These are the durable
/// go-signals the orchestrator's intake poll watches for (see
/// `orchestration/templates/orchestrator.md`): `agent-ready` / `agent-investigation`
/// say *start*, `agent-managed` says *owned*. Anything else is rejected before a
/// spawn — the allow-list is the whole point of routing labels through the
/// backend rather than letting the frontend pass label strings.
///
/// NB: the label that actually exists on the repo (and that `gh issue edit
/// --add-label` therefore accepts) is `agent-investigation`, not the shorter
/// `agent-investigate` the issue-#82 plan text used. We use the real label so
/// the write succeeds and the orchestrator's substring match still picks it up.
const ALLOWED_LABELS: [&str; 3] = ["agent-ready", "agent-investigation", "agent-managed"];

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

// ---------- commands ----------

/// Report whether `gh` is installed and authenticated. Never errors on a
/// missing/unauthenticated `gh` — those are states the UI renders, not faults.
#[tauri::command]
pub fn gh_auth_status() -> Result<GhAuth, String> {
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
pub fn gh_issue_list(repo: String) -> Result<Vec<GhIssue>, String> {
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
pub fn gh_issue_create(repo: String, title: String, body: String) -> Result<GhIssueRef, String> {
    if title.trim().is_empty() {
        return Err("empty issue title".to_string());
    }
    let args = issue_create_args(&title, &body);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_gh(Some(&repo), &argv)?;
    parse_issue_ref(&out)
}

/// Add and/or remove labels on an issue. Every label — add or remove — is
/// validated against `ALLOWED_LABELS` before any spawn, so this can never
/// attach or strip a label outside the agent go-signal set.
#[tauri::command]
pub fn gh_issue_set_labels(
    repo: String,
    number: u64,
    add: Vec<String>,
    remove: Vec<String>,
) -> Result<(), String> {
    validate_labels(&add)?;
    validate_labels(&remove)?;
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

/// Create any allow-listed label in `labels` that the repo doesn't already
/// define, so a following `gh issue edit --add-label` can attach it. Existing
/// labels are left untouched (never re-colored). Callers must have validated
/// `labels` against the allow-list first.
///
/// We list the repo's labels once and create only the genuinely-missing ones —
/// rather than blindly `gh label create`-ing every label — so that a user who
/// *can* toggle labels but *can't* manage them still succeeds when the labels
/// already exist (a blind create would 403 and wrongly block the toggle). The
/// remaining race window (a label created by someone else between our list and
/// our create) is absorbed: an "already exists" create failure is success.
fn ensure_labels_exist(repo: &str, labels: &[String]) -> Result<(), String> {
    let existing = list_label_names(repo)?;
    for name in labels {
        if existing.iter().any(|e| e == name) {
            continue;
        }
        // Unreachable for validated input (every allow-listed label has a spec,
        // asserted by test); guard rather than panic if the two ever drift.
        let (color, description) =
            label_spec(name).ok_or_else(|| format!("no label spec for {name:?}"))?;
        let args = label_create_args(name, color, description);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err(e) = run_gh(Some(repo), &argv) {
            if is_label_exists_error(&e) {
                continue; // lost the race — the label is there now, which is all we want.
            }
            return Err(map_label_create_error(name, &e));
        }
    }
    Ok(())
}

/// List the repo's label names via `gh label list --json name`. The `--limit` is
/// deliberately generous: if an allow-listed label sits past the page we'd only
/// attempt a redundant create and fall back to the already-exists path, but a
/// high limit avoids that wasted round-trip on normally-sized repos.
fn list_label_names(repo: &str) -> Result<Vec<String>, String> {
    let out = run_gh(
        Some(repo),
        &["label", "list", "--json", "name", "--limit", "500"],
    )?;
    parse_label_names(&out)
}

// ---------- pure helpers (unit-tested) ----------

/// Reject any label not in the allow-list.
fn validate_labels(labels: &[String]) -> Result<(), String> {
    for l in labels {
        if !ALLOWED_LABELS.contains(&l.as_str()) {
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

    #[test]
    fn validate_labels_allows_only_go_signals() {
        // Every allow-listed label passes.
        for ok in ALLOWED_LABELS {
            assert!(validate_labels(&[ok.to_string()]).is_ok(), "{ok}");
        }
        // A plausible-but-wrong label (the plan's misspelling) is rejected — it
        // isn't the real repo label, so writing it would fail at gh anyway.
        assert!(validate_labels(&["agent-investigate".to_string()]).is_err());
        // Arbitrary labels are rejected outright.
        assert!(validate_labels(&["bug".to_string()]).is_err());
        // A mixed set fails if any entry is disallowed.
        assert!(validate_labels(&["agent-ready".into(), "wontfix".into()]).is_err());
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
        for l in ALLOWED_LABELS {
            let spec = label_spec(l);
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
        // Non-allow-listed names have no spec (defense in depth vs. arbitrary
        // label creation).
        assert!(label_spec("bug").is_none());
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
        let err = gh_issue_set_labels(
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
        assert!(gh_issue_set_labels("C:/nonexistent".to_string(), 1, vec![], vec![]).is_ok());
    }

    #[test]
    fn create_rejects_empty_title_before_spawning() {
        let err =
            gh_issue_create("C:/nonexistent".to_string(), "   ".to_string(), "b".to_string())
                .unwrap_err();
        assert!(err.contains("empty issue title"), "got: {err}");
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
}
