//! Git integration for the per-pane git view. Everything shells out to the
//! system `git` CLI so user config, credentials, and hooks behave exactly as
//! they do in a terminal. All output is decoded lossily (git paths and
//! messages are not guaranteed UTF-8).
//!
//! Paths returned by git (status, name-status) are repo-root-relative, so the
//! frontend resolves the root once via `git_repo_root` and passes it as
//! `repo` to every other command.

use serde::Serialize;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Run git in `repo` and capture stdout. Non-zero exit → Err(stderr).
fn run_git(repo: &str, args: &[&str]) -> Result<String, String> {
    if !Path::new(repo).is_dir() {
        return Err(format!("no such directory: {repo}"));
    }
    let mut cmd = Command::new("git");
    cmd.current_dir(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "git-not-found".to_string()
        } else {
            e.to_string()
        }
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

// ---------- types ----------

#[derive(Serialize)]
pub struct RefInfo {
    pub name: String,
    /// "branch" | "remote" | "tag" | "head"
    pub kind: String,
}

#[derive(Serialize)]
pub struct CommitInfo {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    /// Committer name — differs from `author` for rebased / cherry-picked /
    /// applied-patch commits, so the row can label who actually committed.
    pub committer: String,
    /// Author time, unix seconds.
    pub timestamp: i64,
    pub subject: String,
    pub refs: Vec<RefInfo>,
}

#[derive(Serialize)]
pub struct BranchInfo {
    pub name: String,
    /// "local" | "remote"
    pub kind: String,
    /// True for the currently checked-out branch.
    pub current: bool,
}

#[derive(Serialize)]
pub struct FileEntry {
    pub path: String,
    /// Original path for renames/copies.
    pub orig_path: Option<String>,
    /// One-letter status: M A D R C U.
    pub status: String,
}

#[derive(Serialize)]
pub struct GitStatus {
    /// Checked-out branch; None when detached.
    pub branch: Option<String>,
    pub detached: bool,
    /// True when the repo has no commits yet.
    pub empty: bool,
    pub staged: Vec<FileEntry>,
    pub unstaged: Vec<FileEntry>,
    pub untracked: Vec<String>,
}

// ---------- commands ----------

/// Resolve the repository root containing `cwd`, or None if not in a repo.
#[tauri::command]
pub fn git_repo_root(cwd: String) -> Result<Option<String>, String> {
    match run_git(&cwd, &["rev-parse", "--show-toplevel"]) {
        Ok(out) => Ok(Some(out.trim().replace('/', std::path::MAIN_SEPARATOR_STR))),
        Err(e) if e.contains("not a git repository") => Ok(None),
        Err(e) => Err(e),
    }
}

#[tauri::command]
pub fn git_log(repo: String, limit: u32) -> Result<Vec<CommitInfo>, String> {
    let n = limit.to_string();
    let out = run_git(
        &repo,
        &[
            "log",
            "--branches",
            "--remotes",
            "--tags",
            "HEAD",
            "--topo-order",
            "--decorate=full",
            "-n",
            &n,
            // %x1f field / %x1e record separators; %s last since a subject
            // could contain 0x1f (ref names and the rest cannot).
            "--format=%H%x1f%P%x1f%an%x1f%cn%x1f%at%x1f%D%x1f%s%x1e",
        ],
    );
    match out {
        Ok(text) => Ok(parse_log(&text)),
        // A freshly-initialized repo has no HEAD to walk yet.
        Err(e)
            if e.contains("does not have any commits")
                || e.contains("bad revision")
                || e.contains("unknown revision") =>
        {
            Ok(Vec::new())
        }
        Err(e) => Err(e),
    }
}

#[tauri::command]
pub fn git_status(repo: String) -> Result<GitStatus, String> {
    let out = run_git(
        &repo,
        &[
            "--no-optional-locks", // never contend with a user/agent git op
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            "-z",
        ],
    )?;
    Ok(parse_status_v2(&out))
}

/// Unified diff for one file. `mode`: "worktree" | "staged" | "commit" |
/// "untracked".
#[tauri::command]
pub fn git_diff(
    repo: String,
    path: String,
    mode: String,
    hash: Option<String>,
) -> Result<String, String> {
    match mode.as_str() {
        "worktree" => run_git(
            &repo,
            &["-c", "core.quotepath=false", "diff", "--", &path],
        ),
        "staged" => run_git(
            &repo,
            &["-c", "core.quotepath=false", "diff", "--cached", "--", &path],
        ),
        "commit" => {
            let h = hash.ok_or("missing hash")?;
            run_git(
                &repo,
                &[
                    "-c",
                    "core.quotepath=false",
                    "show",
                    "--format=",
                    "--patch",
                    // Merge commits diff against their first parent. (The
                    // clearer --diff-merges=first-parent needs git ≥ 2.31.)
                    "--first-parent",
                    "-m",
                    "--find-renames",
                    &h,
                    "--",
                    &path,
                ],
            )
        }
        "untracked" => synth_untracked_diff(Path::new(&repo), &path),
        other => Err(format!("unknown diff mode: {other}")),
    }
}

/// Files touched by a commit (first-parent diff for merges).
#[tauri::command]
pub fn git_commit_files(repo: String, hash: String) -> Result<Vec<FileEntry>, String> {
    let out = run_git(
        &repo,
        &[
            "-c",
            "core.quotepath=false",
            "show",
            "--format=",
            "--name-status",
            "--first-parent",
            "-m",
            "--find-renames",
            "-z",
            &hash,
        ],
    )?;
    Ok(parse_name_status_z(&out))
}

#[tauri::command]
pub fn git_stage(repo: String, paths: Vec<String>) -> Result<(), String> {
    let mut args = vec!["add", "-A", "--"];
    args.extend(paths.iter().map(String::as_str));
    run_git(&repo, &args).map(|_| ())
}

#[tauri::command]
pub fn git_unstage(repo: String, paths: Vec<String>, empty_repo: bool) -> Result<(), String> {
    // `restore --staged` needs a HEAD; before the first commit fall back to
    // removing from the index.
    let mut args: Vec<&str> = if empty_repo {
        vec!["rm", "-r", "--cached", "-q", "--"]
    } else {
        vec!["restore", "--staged", "--"]
    };
    args.extend(paths.iter().map(String::as_str));
    run_git(&repo, &args).map(|_| ())
}

#[tauri::command]
pub fn git_commit(repo: String, message: String) -> Result<(), String> {
    run_git(&repo, &["commit", "-m", &message]).map(|_| ())
}

/// Check out a ref. With `track` the ref is a remote-tracking branch picked
/// from the branch menu (`origin/topic`): resolve it to a local branch and
/// switch there — reusing an existing local branch of that name, or creating a
/// new tracking branch otherwise. Without `track` it's a plain checkout of a
/// local branch, tag, or commit (detached).
///
/// #96: the old path was `checkout --track origin/topic`, which fatals with "a
/// branch named 'topic' already exists" the moment a local `topic` is present
/// (the common case — you've already worked on it once). Splitting the two
/// cases makes checking out a remote branch idempotent.
#[tauri::command]
pub fn git_checkout(repo: String, refname: String, track: bool) -> Result<(), String> {
    // `--` can't guard this the way it does elsewhere — for checkout it's the
    // pathspec separator — so reject a leading-`-` name outright (see check_name).
    check_name(&refname, "ref")?;
    if !track {
        return run_git(&repo, &["checkout", &refname])
            .map(|_| ())
            .map_err(|e| checkout_error(&refname, &e));
    }
    // `refname` is `<remote>/<branch>`; map it to the local branch to land on.
    let local = local_branch_for_remote_ref(&refname, &list_remotes(&repo))
        .ok_or_else(|| format!("{refname:?} is not a remote-tracking branch"))?;
    // A stripped-prefix suffix can still begin with `-` (e.g. `origin/-x`), so
    // re-guard it before it reaches git as a branch argument.
    check_name(&local, "branch")?;
    if local_branch_exists(&repo, &local) {
        // Already have a local branch of that name — just switch to it;
        // re-creating it is the #96 fatal error.
        run_git(&repo, &["switch", &local])
            .map(|_| ())
            .map_err(|e| checkout_error(&local, &e))
    } else {
        // Create a local branch tracking the remote and switch to it.
        run_git(&repo, &["switch", "-c", &local, "--track", &refname])
            .map(|_| ())
            .map_err(|e| checkout_error(&refname, &e))
    }
}

/// Configured remote names (`git remote`). Empty on any error, so the caller
/// falls back to a plain prefix strip.
fn list_remotes(repo: &str) -> Vec<String> {
    run_git(repo, &["remote"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// True when `refs/heads/<name>` resolves — i.e. a local branch of that name
/// already exists.
fn local_branch_exists(repo: &str, name: &str) -> bool {
    run_git(
        repo,
        &["show-ref", "--verify", "--quiet", &format!("refs/heads/{name}")],
    )
    .is_ok()
}

/// Map a remote-tracking ref (`origin/topic`, `up/feat/x`) to the local branch
/// name to check out — the ref with its remote prefix removed. The first
/// configured remote that prefixes it as `<remote>/…` wins (so a branch whose
/// own name contains slashes survives); when none match (e.g. the remote was
/// since removed) fall back to dropping the first path segment. `None` when
/// there's nothing after the remote to name a branch.
fn local_branch_for_remote_ref(refname: &str, remotes: &[String]) -> Option<String> {
    let after_prefix = |prefix: &str| {
        refname
            .strip_prefix(prefix)
            .and_then(|s| s.strip_prefix('/'))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    remotes.iter().find_map(|r| after_prefix(r)).or_else(|| {
        refname
            .split_once('/')
            .map(|(_, rest)| rest.to_string())
            .filter(|s| !s.is_empty())
    })
}

/// Wrap a raw git failure with the ref we were trying to check out, so the
/// toast is actionable instead of a bare git error (#96).
fn checkout_error(refname: &str, err: &str) -> String {
    format!("could not check out {refname:?}:\n{err}")
}

// ---------- remote & history ops ----------
//
// All of these take user-chosen ref / branch / tag / remote names. Spawns are
// arg-vectors so shell injection is impossible; `check_name` additionally
// blocks a leading `-` so a crafted name can't be parsed as a git option.
// Ops that can stop on a conflict (cherry-pick / revert / merge / rebase) are
// run through `run_sequencer`, which aborts on failure so the working tree is
// left clean — conflicts are surfaced as errors, never auto-resolved.

/// Reject empty names and names that could be read as an option flag.
fn check_name(name: &str, what: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("empty {what}"));
    }
    if name.starts_with('-') {
        return Err(format!("invalid {what} {name:?}: must not start with '-'"));
    }
    Ok(())
}

/// Run a sequencer command; on failure abort it (restoring a clean tree, since
/// this view has no conflict-resolution surface) and return a clear error.
fn run_sequencer(repo: &str, args: &[&str], abort: &[&str], label: &str) -> Result<(), String> {
    match run_git(repo, args) {
        Ok(_) => Ok(()),
        Err(e) => {
            // Best-effort: abort no-ops (and errors, ignored) when nothing was
            // started, and unwinds a real conflict otherwise.
            let _ = run_git(repo, abort);
            Err(format!(
                "{label} failed — working tree left unchanged:\n{e}"
            ))
        }
    }
}

/// Fetch from remotes and prune deleted remote branches. A repo with no remote
/// configured is a no-op success, so the refresh button never errors locally.
#[tauri::command]
pub fn git_fetch(repo: String, remote: Option<String>) -> Result<(), String> {
    if let Some(r) = &remote {
        check_name(r, "remote")?;
        return run_git(&repo, &["fetch", "--prune", r]).map(|_| ());
    }
    if run_git(&repo, &["remote"])?.trim().is_empty() {
        return Ok(());
    }
    run_git(&repo, &["fetch", "--all", "--prune"]).map(|_| ())
}

/// Push the current branch. With `set_upstream`, publish it to the first
/// configured remote and set tracking (`push -u <remote> <branch>`); otherwise
/// a plain `git push`, which needs an upstream already set. Auth / network
/// failures surface verbatim.
#[tauri::command]
pub fn git_push(repo: String, set_upstream: bool) -> Result<(), String> {
    if !set_upstream {
        return run_git(&repo, &["push"]).map(|_| ());
    }
    let branch = run_git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    if branch.is_empty() || branch == "HEAD" {
        return Err("detached HEAD — check out a branch before publishing".to_string());
    }
    let remotes = run_git(&repo, &["remote"])?;
    let remote = remotes
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or("no remote configured to publish to")?;
    run_git(&repo, &["push", "-u", remote, branch]).map(|_| ())
}

/// Pull fast-forward-only — never creates an implicit merge or rebase. A
/// diverged branch fails with git's "not possible to fast-forward" message,
/// surfaced so the user resolves it deliberately.
#[tauri::command]
pub fn git_pull(repo: String) -> Result<(), String> {
    run_git(&repo, &["pull", "--ff-only"]).map(|_| ())
}

/// Create a lightweight tag `name` at `hash`.
#[tauri::command]
pub fn git_tag(repo: String, name: String, hash: String) -> Result<(), String> {
    check_name(&name, "tag name")?;
    check_name(&hash, "commit")?;
    run_git(&repo, &["tag", &name, &hash]).map(|_| ())
}

/// Create branch `name` at `hash`, optionally checking it out.
#[tauri::command]
pub fn git_branch_create(
    repo: String,
    name: String,
    hash: String,
    checkout: bool,
) -> Result<(), String> {
    check_name(&name, "branch name")?;
    check_name(&hash, "commit")?;
    if checkout {
        run_git(&repo, &["checkout", "-b", &name, &hash]).map(|_| ())
    } else {
        run_git(&repo, &["branch", &name, &hash]).map(|_| ())
    }
}

/// Cherry-pick `hash` onto the current branch. Conflicts abort (see module note).
#[tauri::command]
pub fn git_cherry_pick(repo: String, hash: String) -> Result<(), String> {
    check_name(&hash, "commit")?;
    run_sequencer(
        &repo,
        &["cherry-pick", &hash],
        &["cherry-pick", "--abort"],
        "cherry-pick",
    )
}

/// Revert `hash` on the current branch (creates an inverse commit). Conflicts abort.
#[tauri::command]
pub fn git_revert(repo: String, hash: String) -> Result<(), String> {
    check_name(&hash, "commit")?;
    run_sequencer(
        &repo,
        &["revert", "--no-edit", &hash],
        &["revert", "--abort"],
        "revert",
    )
}

/// Merge `refname` into the current branch. Conflicts abort.
#[tauri::command]
pub fn git_merge(repo: String, refname: String) -> Result<(), String> {
    check_name(&refname, "ref")?;
    run_sequencer(
        &repo,
        &["merge", "--no-edit", &refname],
        &["merge", "--abort"],
        "merge",
    )
}

/// Rebase the current branch onto `upstream`. Conflicts abort.
#[tauri::command]
pub fn git_rebase(repo: String, upstream: String) -> Result<(), String> {
    check_name(&upstream, "ref")?;
    run_sequencer(
        &repo,
        &["rebase", &upstream],
        &["rebase", "--abort"],
        "rebase",
    )
}

/// All local and remote-tracking branches, for the checkout menu. (`for-each-ref`
/// has its own format language that does NOT expand `%x1f` like `git log`, so
/// each ref is one line — ref names never contain whitespace — and the current
/// branch is resolved separately.)
#[tauri::command]
pub fn git_branches(repo: String) -> Result<Vec<BranchInfo>, String> {
    let current = run_git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let out = run_git(
        &repo,
        &["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"],
    )?;
    let mut branches = Vec::new();
    for full in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        // Skip the symbolic <remote>/HEAD pointer.
        if full.ends_with("/HEAD") {
            continue;
        }
        if let Some(name) = full.strip_prefix("refs/heads/") {
            branches.push(BranchInfo {
                name: name.to_string(),
                kind: "local".to_string(),
                current: name == current,
            });
        } else if let Some(name) = full.strip_prefix("refs/remotes/") {
            branches.push(BranchInfo {
                name: name.to_string(),
                kind: "remote".to_string(),
                current: false,
            });
        }
    }
    Ok(branches)
}

/// Throw away changes to one file: restore tracked files, delete untracked.
#[tauri::command]
pub fn git_discard(repo: String, path: String, untracked: bool) -> Result<(), String> {
    if untracked {
        let rel = Path::new(&path);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("invalid path".to_string());
        }
        let full: PathBuf = Path::new(&repo).join(rel);
        std::fs::remove_file(&full).map_err(|e| e.to_string())
    } else {
        run_git(&repo, &["restore", "--", &path]).map(|_| ())
    }
}

/// Names must be usable both as a branch name and as a relative directory:
/// letters, digits, `. _ - /`, no leading `-` or `/`, no `..`, no trailing `/`.
fn valid_worktree_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

/// Create a worktree for an agent session at
/// `<repo-parent>/<repo-name>-worktrees/<name>`, checked out on a branch
/// named `name` — a new branch off HEAD, or the existing branch of that name.
/// Returns the worktree's absolute path.
#[tauri::command]
pub fn git_worktree_add(repo: String, name: String) -> Result<String, String> {
    if !valid_worktree_name(&name) {
        return Err(format!(
            "invalid worktree name {name:?} — use letters, digits, and . _ - /"
        ));
    }
    let root = run_git(&repo, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root.trim().replace('/', std::path::MAIN_SEPARATOR_STR));
    let repo_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or("cannot resolve repository name")?;
    let parent = root.parent().ok_or("repository has no parent directory")?;
    let dest = parent.join(format!("{repo_name}-worktrees")).join(&name);
    if dest.exists() {
        return Err(format!("worktree path already exists: {}", dest.display()));
    }
    let dest_str = dest.to_string_lossy().into_owned();
    if let Err(e) = run_git(&repo, &["worktree", "add", "-b", &name, &dest_str]) {
        // `-b` refuses when the branch already exists; check that branch out
        // into the new worktree instead.
        if e.contains("already exists") {
            run_git(&repo, &["worktree", "add", &dest_str, &name])?;
        } else {
            return Err(e);
        }
    }
    Ok(dest_str)
}

/// Remove an agent's worktree during group teardown. `--force` because the
/// worktree may hold uncommitted changes and ending an orchestration is an
/// explicit, human-confirmed destructive action; the checked-out branch is
/// left intact (the work / PR lives on it, only the working copy goes). Not a
/// Tauri command — teardown is driven backend-side by `end_group`, which
/// gathers the paths from its own roster rather than trusting a caller.
pub fn git_worktree_remove(repo: &str, path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("empty worktree path".to_string());
    }
    run_git(repo, &["worktree", "remove", "--force", path]).map(|_| ())
}

// ---------- parsers ----------

fn parse_log(out: &str) -> Vec<CommitInfo> {
    out.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_matches(['\n', '\r']);
            if rec.is_empty() {
                return None;
            }
            let mut f = rec.splitn(7, '\x1f');
            let hash = f.next()?.to_string();
            let parents = f
                .next()?
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let author = f.next()?.to_string();
            let committer = f.next()?.to_string();
            let timestamp = f.next()?.parse::<i64>().ok()?;
            let refs = parse_decorations(f.next()?);
            let subject = f.next()?.to_string();
            Some(CommitInfo {
                hash,
                parents,
                author,
                committer,
                timestamp,
                subject,
                refs,
            })
        })
        .collect()
}

/// Parse `%D` with `--decorate=full`, e.g.
/// `HEAD -> refs/heads/main, tag: refs/tags/v1, refs/remotes/origin/main`.
fn parse_decorations(d: &str) -> Vec<RefInfo> {
    let mut refs = Vec::new();
    for part in d.split(", ").map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(target) = part.strip_prefix("HEAD -> ") {
            refs.push(RefInfo {
                name: "HEAD".to_string(),
                kind: "head".to_string(),
            });
            if let Some(name) = target.strip_prefix("refs/heads/") {
                refs.push(RefInfo {
                    name: name.to_string(),
                    kind: "branch".to_string(),
                });
            }
        } else if part == "HEAD" {
            // Detached HEAD sits directly on the commit.
            refs.push(RefInfo {
                name: "HEAD".to_string(),
                kind: "head".to_string(),
            });
        } else if let Some(name) = part.strip_prefix("tag: refs/tags/") {
            refs.push(RefInfo {
                name: name.to_string(),
                kind: "tag".to_string(),
            });
        } else if let Some(name) = part.strip_prefix("refs/heads/") {
            refs.push(RefInfo {
                name: name.to_string(),
                kind: "branch".to_string(),
            });
        } else if let Some(name) = part.strip_prefix("refs/remotes/") {
            if !name.ends_with("/HEAD") {
                refs.push(RefInfo {
                    name: name.to_string(),
                    kind: "remote".to_string(),
                });
            }
        }
        // refs/stash, grafted, replaced markers etc. are skipped.
    }
    refs
}

fn parse_status_v2(out: &str) -> GitStatus {
    let mut st = GitStatus {
        branch: None,
        detached: false,
        empty: false,
        staged: Vec::new(),
        unstaged: Vec::new(),
        untracked: Vec::new(),
    };

    let mut tokens = out.split('\0');
    while let Some(tok) = tokens.next() {
        if tok.is_empty() {
            continue;
        }
        if let Some(header) = tok.strip_prefix("# ") {
            if let Some(head) = header.strip_prefix("branch.head ") {
                if head == "(detached)" {
                    st.detached = true;
                } else {
                    st.branch = Some(head.to_string());
                }
            } else if let Some(oid) = header.strip_prefix("branch.oid ") {
                if oid == "(initial)" {
                    st.empty = true;
                }
            }
            continue;
        }
        match tok.as_bytes().first() {
            Some(b'1') => {
                // 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
                let mut f = tok.splitn(9, ' ');
                let (Some(_), Some(xy)) = (f.next(), f.next()) else {
                    continue;
                };
                let Some(path) = f.nth(6) else { continue };
                push_xy(&mut st, xy, path, None);
            }
            Some(b'2') => {
                // 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>
                // followed (in -z mode) by the ORIGINAL path as its own token.
                let mut f = tok.splitn(10, ' ');
                let (Some(_), Some(xy)) = (f.next(), f.next()) else {
                    continue;
                };
                let Some(path) = f.nth(7) else { continue };
                let orig = tokens.next().map(str::to_string);
                push_xy(&mut st, xy, path, orig);
            }
            Some(b'u') => {
                // u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
                let mut f = tok.splitn(11, ' ');
                let Some(path) = f.nth(10) else { continue };
                st.unstaged.push(FileEntry {
                    path: path.to_string(),
                    orig_path: None,
                    status: "U".to_string(),
                });
            }
            Some(b'?') => {
                if let Some(path) = tok.strip_prefix("? ") {
                    st.untracked.push(path.to_string());
                }
            }
            _ => {}
        }
    }
    st
}

/// Route a porcelain XY pair into staged (X) and/or unstaged (Y) lists.
/// A file can appear in both (e.g. `MM`: staged edit + further worktree edit).
fn push_xy(st: &mut GitStatus, xy: &str, path: &str, orig: Option<String>) {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or('.');
    let y = chars.next().unwrap_or('.');
    if x != '.' {
        st.staged.push(FileEntry {
            path: path.to_string(),
            orig_path: orig.clone(),
            status: x.to_string(),
        });
    }
    if y != '.' {
        st.unstaged.push(FileEntry {
            path: path.to_string(),
            orig_path: orig,
            status: y.to_string(),
        });
    }
}

/// Parse `--name-status -z`: tokens alternate STATUS, PATH — except renames
/// and copies (`R###`/`C###`) which take OLD then NEW.
fn parse_name_status_z(out: &str) -> Vec<FileEntry> {
    let mut files = Vec::new();
    let mut tokens = out.split('\0').filter(|t| !t.is_empty());
    while let Some(status) = tokens.next() {
        let code = status.chars().next().unwrap_or('?');
        match code {
            'R' | 'C' => {
                let (Some(old), Some(new)) = (tokens.next(), tokens.next()) else {
                    break;
                };
                files.push(FileEntry {
                    path: new.to_string(),
                    orig_path: Some(old.to_string()),
                    status: code.to_string(),
                });
            }
            _ => {
                let Some(path) = tokens.next() else { break };
                files.push(FileEntry {
                    path: path.to_string(),
                    orig_path: None,
                    status: code.to_string(),
                });
            }
        }
    }
    files
}

/// Synthesize an all-added unified diff for an untracked file, so the diff
/// panel can preview it like any other change.
fn synth_untracked_diff(repo: &Path, rel: &str) -> Result<String, String> {
    const MAX_BYTES: u64 = 1024 * 1024;
    let full = repo.join(rel);
    let meta = std::fs::metadata(&full).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Ok(format!(
            "diff --git a/{rel} b/{rel}\nnew file\nFile too large to preview ({} KB).\n",
            meta.len() / 1024
        ));
    }
    let bytes = std::fs::read(&full).map_err(|e| e.to_string())?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Ok(format!(
            "diff --git a/{rel} b/{rel}\nBinary files /dev/null and b/{rel} differ\n"
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let mut diff = format!(
        "diff --git a/{rel} b/{rel}\nnew file mode 100644\n--- /dev/null\n+++ b/{rel}\n"
    );
    if !lines.is_empty() {
        diff.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
        for line in &lines {
            diff.push('+');
            diff.push_str(line);
            diff.push('\n');
        }
    }
    Ok(diff)
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_log_basic_merge_and_root() {
        let out = "aaa\x1fbbb ccc\x1fAlice\x1fAlice C\x1f1700000000\x1fHEAD -> refs/heads/main, refs/remotes/origin/main\x1ffix: a, b\x1e\
                   bbb\x1f\x1fBob\x1fCarol\x1f1690000000\x1ftag: refs/tags/v1\x1finit\x1e";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].parents, vec!["bbb", "ccc"]); // merge
        assert_eq!(commits[0].author, "Alice");
        assert_eq!(commits[0].committer, "Alice C");
        assert_eq!(commits[0].timestamp, 1700000000);
        assert_eq!(commits[0].subject, "fix: a, b");
        // Author and committer are parsed independently (rebase / cherry-pick).
        assert_eq!(commits[1].author, "Bob");
        assert_eq!(commits[1].committer, "Carol");
        assert_eq!(
            commits[0]
                .refs
                .iter()
                .map(|r| (r.kind.as_str(), r.name.as_str()))
                .collect::<Vec<_>>(),
            vec![("head", "HEAD"), ("branch", "main"), ("remote", "origin/main")]
        );
        assert!(commits[1].parents.is_empty()); // root commit
        assert_eq!(commits[1].refs[0].kind, "tag");
        assert_eq!(commits[1].refs[0].name, "v1");
    }

    #[test]
    fn worktree_name_validation() {
        for ok in ["fix-auth", "feature/api-v2", "wt_1.2"] {
            assert!(valid_worktree_name(ok), "{ok} should be valid");
        }
        for bad in ["", "-x", "/abs", "a/", "a..b", "has space", "back\\slash"] {
            assert!(!valid_worktree_name(bad), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn parse_decorations_detached_and_filtered() {
        let refs = parse_decorations("HEAD, refs/remotes/origin/HEAD, refs/stash, refs/heads/feature/x");
        assert_eq!(
            refs.iter()
                .map(|r| (r.kind.as_str(), r.name.as_str()))
                .collect::<Vec<_>>(),
            vec![("head", "HEAD"), ("branch", "feature/x")]
        );
    }

    #[test]
    fn parse_status_ordinary_and_both_lists() {
        // 1 .M = unstaged only; 1 M. = staged only; 1 MM = both.
        let out = "# branch.oid abc\0# branch.head main\0\
                   1 .M N... 100644 100644 100644 h1 h2 a.txt\0\
                   1 M. N... 100644 100644 100644 h1 h2 b.txt\0\
                   1 MM N... 100644 100644 100644 h1 h2 c.txt\0\
                   ? new.txt\0";
        let st = parse_status_v2(out);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert!(!st.detached && !st.empty);
        assert_eq!(
            st.staged.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["b.txt", "c.txt"]
        );
        assert_eq!(
            st.unstaged.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["a.txt", "c.txt"]
        );
        assert_eq!(st.untracked, vec!["new.txt"]);
    }

    #[test]
    fn parse_status_rename_consumes_orig_token() {
        let out = "# branch.head main\0\
                   2 R. N... 100644 100644 100644 h1 h2 R100 new name.txt\0old name.txt\0\
                   1 .M N... 100644 100644 100644 h1 h2 after.txt\0";
        let st = parse_status_v2(out);
        assert_eq!(st.staged.len(), 1);
        assert_eq!(st.staged[0].path, "new name.txt"); // spaces in path survive
        assert_eq!(st.staged[0].orig_path.as_deref(), Some("old name.txt"));
        // The record after the rename still parses (orig token consumed).
        assert_eq!(st.unstaged[0].path, "after.txt");
    }

    #[test]
    fn parse_status_detached_and_initial() {
        let out = "# branch.oid (initial)\0# branch.head (detached)\0";
        let st = parse_status_v2(out);
        assert!(st.detached);
        assert!(st.empty);
        assert!(st.branch.is_none());
    }

    #[test]
    fn parse_name_status_with_rename() {
        let out = "M\0a.txt\0R100\0old.txt\0new.txt\0A\0added.txt\0";
        let files = parse_name_status_z(out);
        assert_eq!(files.len(), 3);
        assert_eq!(files[1].status, "R");
        assert_eq!(files[1].orig_path.as_deref(), Some("old.txt"));
        assert_eq!(files[1].path, "new.txt");
        assert_eq!(files[2].path, "added.txt");
    }

    #[test]
    fn synth_untracked_counts_lines() {
        let dir = std::env::temp_dir().join("loomux-git-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.txt"), "one\ntwo\n").unwrap();
        let diff = synth_untracked_diff(&dir, "t.txt").unwrap();
        assert!(diff.contains("@@ -0,0 +1,2 @@"));
        assert!(diff.contains("+one\n+two\n"));
    }

    // ---------- git-op integration tests (spawn the real git CLI) ----------
    //
    // Each exercises one command's success path plus the failure paths called
    // out in the issue: dirty-tree checkout, conflicting cherry-pick, and
    // push/pull against a local bare repo.

    use std::process::Command as StdCommand;

    /// Path as a git-friendly string (forward slashes).
    fn p(dir: &Path) -> String {
        dir.to_string_lossy().replace('\\', "/")
    }

    /// Run git in `dir` for test setup; panics on failure.
    fn setup_git(dir: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_GLOBAL", "") // ignore the developer's global config
            .env("GIT_CONFIG_SYSTEM", "")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Fresh work repo on branch `main` with a deterministic identity and no
    /// line-ending rewriting (so content round-trips byte-for-byte on Windows).
    fn new_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        setup_git(d, &["init", "-q"]);
        // Point the unborn HEAD at `main` regardless of git version / config.
        setup_git(d, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        setup_git(d, &["config", "user.name", "Test"]);
        setup_git(d, &["config", "user.email", "test@example.com"]);
        setup_git(d, &["config", "commit.gpgsign", "false"]);
        setup_git(d, &["config", "core.autocrlf", "false"]);
        dir
    }

    /// Write `file`, commit it, and return the new HEAD hash.
    fn commit(dir: &Path, file: &str, content: &str, msg: &str) -> String {
        std::fs::write(dir.join(file), content).unwrap();
        setup_git(dir, &["add", file]);
        setup_git(dir, &["commit", "-q", "-m", msg]);
        run_git(&p(dir), &["rev-parse", "HEAD"]).unwrap().trim().to_string()
    }

    fn read(dir: &Path, file: &str) -> String {
        std::fs::read_to_string(dir.join(file)).unwrap()
    }

    fn is_clean(dir: &Path) -> bool {
        run_git(&p(dir), &["status", "--porcelain"]).unwrap().trim().is_empty()
    }

    #[test]
    fn tag_and_branch_create_and_list() {
        let repo = new_repo();
        let d = repo.path();
        let a = commit(d, "f.txt", "a\n", "A");

        git_tag(p(d), "v1".into(), a.clone()).unwrap();
        assert!(run_git(&p(d), &["tag"]).unwrap().contains("v1"));
        // A name that looks like an option is rejected before spawning git.
        assert!(git_tag(p(d), "-x".into(), a.clone()).is_err());

        git_branch_create(p(d), "topic".into(), a.clone(), false).unwrap();
        let names: Vec<String> = git_branches(p(d)).unwrap().into_iter().map(|b| b.name).collect();
        assert!(names.contains(&"main".to_string()) && names.contains(&"topic".to_string()));
        let current: Vec<String> =
            git_branches(p(d)).unwrap().into_iter().filter(|b| b.current).map(|b| b.name).collect();
        assert_eq!(current, vec!["main"]);
    }

    #[test]
    fn checkout_switches_and_refuses_dirty_overwrite() {
        let repo = new_repo();
        let d = repo.path();
        commit(d, "f.txt", "one\n", "A");
        git_branch_create(p(d), "feat".into(), "HEAD".into(), true).unwrap();
        commit(d, "f.txt", "two\n", "B on feat");

        git_checkout(p(d), "main".into(), false).unwrap();
        assert_eq!(read(d, "f.txt"), "one\n");

        // An uncommitted change that checkout would clobber must be refused.
        std::fs::write(d.join("f.txt"), "dirty\n").unwrap();
        let err = git_checkout(p(d), "feat".into(), false).unwrap_err();
        assert!(err.contains("would be overwritten") || err.contains("overwritten by checkout"));
        // Still on main with the dirty content intact.
        assert_eq!(read(d, "f.txt"), "dirty\n");
    }

    #[test]
    fn checkout_rejects_option_like_refname() {
        let repo = new_repo();
        let d = repo.path();
        commit(d, "f.txt", "a\n", "A");
        // A leading-`-` name is blocked before ever reaching git, so it can't
        // be parsed as an option (checkout can't use `--` to guard it).
        let err = git_checkout(p(d), "-f".into(), false).unwrap_err();
        assert!(err.contains("must not start with '-'"), "got: {err}");
        assert!(git_checkout(p(d), "--track".into(), true).is_err());
    }

    #[test]
    fn resolve_remote_ref_strips_remote_prefix() {
        let origin = vec!["origin".to_string()];
        // Simple case.
        assert_eq!(
            local_branch_for_remote_ref("origin/feature", &origin).as_deref(),
            Some("feature")
        );
        // Branch name with slashes (the #96 ref) keeps every segment after the
        // remote — the remote is matched, not just "first path component".
        assert_eq!(
            local_branch_for_remote_ref("origin/orch/integration-46-65", &origin).as_deref(),
            Some("orch/integration-46-65")
        );
        // The right remote among several wins; a look-alike prefix is not a
        // false match (`orig` must not swallow `origin/…`).
        let many = vec!["orig".to_string(), "origin".to_string(), "up".to_string()];
        assert_eq!(
            local_branch_for_remote_ref("up/feat/x", &many).as_deref(),
            Some("feat/x")
        );
        assert_eq!(
            local_branch_for_remote_ref("origin/x", &many).as_deref(),
            Some("x")
        );
        // No configured remotes → fall back to dropping the first segment.
        assert_eq!(
            local_branch_for_remote_ref("origin/topic", &[]).as_deref(),
            Some("topic")
        );
        // Nothing left to name a branch → None.
        assert_eq!(local_branch_for_remote_ref("origin", &origin), None);
        assert_eq!(local_branch_for_remote_ref("origin/", &origin), None);
    }

    #[test]
    fn checkout_track_reuses_or_creates_local_branch() {
        // Publish `main` + a `topic/nested` branch to a bare remote.
        let bare = tempfile::tempdir().unwrap();
        setup_git(bare.path(), &["init", "-q", "--bare"]);
        setup_git(bare.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
        let up = new_repo();
        commit(up.path(), "f.txt", "one\n", "A");
        setup_git(up.path(), &["branch", "topic/nested"]);
        setup_git(up.path(), &["remote", "add", "origin", &p(bare.path())]);
        git_push(p(up.path()), true).unwrap();
        setup_git(up.path(), &["push", "-q", "origin", "topic/nested"]);

        // Fresh clone: no local `topic/nested` yet → create a tracking branch.
        let clone_dir = tempfile::tempdir().unwrap();
        setup_git(clone_dir.path(), &["clone", "-q", &p(bare.path()), "wc"]);
        let d = clone_dir.path().join("wc");
        setup_git(&d, &["config", "user.name", "Two"]);
        setup_git(&d, &["config", "user.email", "two@example.com"]);

        git_checkout(p(&d), "origin/topic/nested".into(), true).unwrap();
        assert_eq!(
            run_git(&p(&d), &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap().trim(),
            "topic/nested"
        );
        // The new branch tracks the remote.
        assert_eq!(
            run_git(&p(&d), &["rev-parse", "--abbrev-ref", "@{upstream}"]).unwrap().trim(),
            "origin/topic/nested"
        );

        // Switch away, then re-check-out the remote ref. #96: the old
        // `checkout --track` fataled here because `topic/nested` now exists
        // locally; we must just switch back to it.
        git_checkout(p(&d), "main".into(), false).unwrap();
        git_checkout(p(&d), "origin/topic/nested".into(), true).unwrap();
        assert_eq!(
            run_git(&p(&d), &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap().trim(),
            "topic/nested"
        );
    }

    #[test]
    fn cherry_pick_applies_then_aborts_on_conflict() {
        // Clean apply: pick a commit that touches a different region.
        let repo = new_repo();
        let d = repo.path();
        commit(d, "f.txt", "L1\n", "A");
        git_branch_create(p(d), "feature".into(), "HEAD".into(), true).unwrap();
        let b = commit(d, "f.txt", "L1\nL2\n", "add L2");
        git_checkout(p(d), "main".into(), false).unwrap();
        git_cherry_pick(p(d), b).unwrap();
        assert!(read(d, "f.txt").contains("L2"));

        // Conflicting apply: same line changed two ways → abort, tree clean.
        let repo2 = new_repo();
        let d2 = repo2.path();
        commit(d2, "f.txt", "base\n", "A");
        git_branch_create(p(d2), "feature".into(), "HEAD".into(), true).unwrap();
        let fb = commit(d2, "f.txt", "feature\n", "feature edit");
        git_checkout(p(d2), "main".into(), false).unwrap();
        commit(d2, "f.txt", "mainline\n", "main edit");
        let err = git_cherry_pick(p(d2), fb).unwrap_err();
        assert!(err.contains("cherry-pick failed"));
        assert!(is_clean(d2), "conflict must be aborted to a clean tree");
        assert_eq!(read(d2, "f.txt"), "mainline\n");
    }

    #[test]
    fn revert_creates_inverse_commit() {
        let repo = new_repo();
        let d = repo.path();
        commit(d, "f.txt", "a\n", "A");
        let b = commit(d, "f.txt", "a\nb\n", "B adds b");
        git_revert(p(d), b).unwrap();
        assert_eq!(read(d, "f.txt"), "a\n");
        // A revert is a new commit, so the tree is clean afterwards.
        assert!(is_clean(d));
    }

    #[test]
    fn merge_joins_branch_then_aborts_on_conflict() {
        // Clean merge of a branch that adds a new file.
        let repo = new_repo();
        let d = repo.path();
        commit(d, "f.txt", "base\n", "A");
        git_branch_create(p(d), "feature".into(), "HEAD".into(), true).unwrap();
        commit(d, "g.txt", "new\n", "add g");
        git_checkout(p(d), "main".into(), false).unwrap();
        git_merge(p(d), "feature".into()).unwrap();
        assert!(d.join("g.txt").exists());

        // Conflicting merge → abort, tree clean.
        let repo2 = new_repo();
        let d2 = repo2.path();
        commit(d2, "f.txt", "base\n", "A");
        git_branch_create(p(d2), "feature".into(), "HEAD".into(), true).unwrap();
        commit(d2, "f.txt", "feature\n", "feature edit");
        git_checkout(p(d2), "main".into(), false).unwrap();
        commit(d2, "f.txt", "mainline\n", "main edit");
        let err = git_merge(p(d2), "feature".into()).unwrap_err();
        assert!(err.contains("merge failed"));
        assert!(is_clean(d2), "conflicted merge must be aborted");
    }

    #[test]
    fn rebase_replays_onto_upstream() {
        let repo = new_repo();
        let d = repo.path();
        commit(d, "f.txt", "base\n", "A");
        git_branch_create(p(d), "feature".into(), "HEAD".into(), true).unwrap();
        commit(d, "feat.txt", "feature work\n", "feature commit");
        git_checkout(p(d), "main".into(), false).unwrap();
        commit(d, "main.txt", "main work\n", "main commit");
        git_checkout(p(d), "feature".into(), false).unwrap();
        git_rebase(p(d), "main".into()).unwrap();
        // After rebasing onto main, the feature branch sees main's file too.
        assert!(d.join("main.txt").exists());
        assert!(d.join("feat.txt").exists());
    }

    #[test]
    fn fetch_push_pull_against_bare_remote() {
        let bare = tempfile::tempdir().unwrap();
        setup_git(bare.path(), &["init", "-q", "--bare"]);
        // So a later clone checks out `main` (tracking origin/main) instead of
        // landing on the bare's default unborn `master`.
        setup_git(bare.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);

        // Repo 1 publishes main to the bare remote.
        let repo1 = new_repo();
        let d1 = repo1.path();
        commit(d1, "f.txt", "one\n", "A");
        setup_git(d1, &["remote", "add", "origin", &p(bare.path())]);
        git_push(p(d1), true).unwrap(); // set upstream + push
        // A plain push now works because the upstream is set.
        commit(d1, "f.txt", "one\ntwo\n", "B");
        git_push(p(d1), false).unwrap();

        // Repo 2 clones, adds a commit, pushes it.
        let clone_dir = tempfile::tempdir().unwrap();
        setup_git(clone_dir.path(), &["clone", "-q", &p(bare.path()), "wc"]);
        let d2 = clone_dir.path().join("wc");
        setup_git(&d2, &["config", "user.name", "Two"]);
        setup_git(&d2, &["config", "user.email", "two@example.com"]);
        setup_git(&d2, &["config", "core.autocrlf", "false"]);
        commit(&d2, "f.txt", "one\ntwo\nthree\n", "C from clone");
        git_push(p(&d2), false).unwrap();

        // Repo 1 fetches and fast-forwards to C.
        git_fetch(p(d1), None).unwrap();
        git_pull(p(d1)).unwrap();
        assert_eq!(read(d1, "f.txt"), "one\ntwo\nthree\n");

        // Divergence makes a fast-forward pull fail (never an implicit merge).
        commit(d1, "f.txt", "one\ntwo\nthree\nlocal\n", "D local only");
        commit(&d2, "f.txt", "one\ntwo\nthree\nremote\n", "E remote only");
        git_push(p(&d2), false).unwrap();
        git_fetch(p(d1), None).unwrap();
        let err = git_pull(p(d1)).unwrap_err();
        assert!(
            err.contains("fast-forward") || err.contains("Not possible") || err.contains("diverging"),
            "diverged pull should refuse: {err}"
        );
    }

    #[test]
    fn fetch_is_noop_without_remote() {
        let repo = new_repo();
        commit(repo.path(), "f.txt", "a\n", "A");
        // No remote configured — fetch must succeed quietly, not error.
        git_fetch(p(repo.path()), None).unwrap();
    }
}
