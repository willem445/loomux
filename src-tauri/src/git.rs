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
    /// Author time, unix seconds.
    pub timestamp: i64,
    pub subject: String,
    pub refs: Vec<RefInfo>,
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
            "--format=%H%x1f%P%x1f%an%x1f%at%x1f%D%x1f%s%x1e",
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

/// Check out a ref. `track` for remote branches: creates a local tracking
/// branch explicitly (dwim misfires when several remotes share the name).
#[tauri::command]
pub fn git_checkout(repo: String, refname: String, track: bool) -> Result<(), String> {
    if track {
        run_git(&repo, &["checkout", "--track", &refname]).map(|_| ())
    } else {
        run_git(&repo, &["checkout", &refname]).map(|_| ())
    }
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

// ---------- parsers ----------

fn parse_log(out: &str) -> Vec<CommitInfo> {
    out.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_matches(['\n', '\r']);
            if rec.is_empty() {
                return None;
            }
            let mut f = rec.splitn(6, '\x1f');
            let hash = f.next()?.to_string();
            let parents = f
                .next()?
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let author = f.next()?.to_string();
            let timestamp = f.next()?.parse::<i64>().ok()?;
            let refs = parse_decorations(f.next()?);
            let subject = f.next()?.to_string();
            Some(CommitInfo {
                hash,
                parents,
                author,
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
        let out = "aaa\x1fbbb ccc\x1fAlice\x1f1700000000\x1fHEAD -> refs/heads/main, refs/remotes/origin/main\x1ffix: a, b\x1e\
                   bbb\x1f\x1fBob\x1f1690000000\x1ftag: refs/tags/v1\x1finit\x1e";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].parents, vec!["bbb", "ccc"]); // merge
        assert_eq!(commits[0].subject, "fix: a, b");
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
}
