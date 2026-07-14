//! Orchestrator/worker agent groups: registry, guardrails, persistence,
//! audit log, and visible prompt delivery.
//!
//! An orchestration *group* is one orchestrator pane plus the worker and
//! reviewer panes it manages, all running `claude` CLIs connected to the
//! loomux MCP server (see `mcp.rs`) with per-agent identity tokens. Panes
//! are frontend-owned, so spawning round-trips: registry emits
//! `orch-spawn-request` → frontend opens the pane → `bind_agent` reports the
//! pty id back and unblocks the spawner.
//!
//! Inter-agent communication is deliberately *typed into the recipient's
//! CLI* (bracketed paste + Enter) rather than delivered out of band: the
//! human sees every prompt exactly as if they had written it, can steer any
//! pane, and the audit log (`audit.jsonl`) records the full text.

pub mod mcp;
pub mod profiles;
pub mod workflow;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

use crate::obs::LockExt;

// doc-hidden `pub` so the integration tests can reconstruct the pre-#222
// rendering of a template and assert loomux still writes exactly that when no
// workflow is active (`the_toggle_off_leaves_every_instruction_file_byte_for_byte_what_it_was`).
#[doc(hidden)]
pub const ORCHESTRATOR_TPL: &str = include_str!("templates/orchestrator.md");
#[doc(hidden)]
pub const WORKER_TPL: &str = include_str!("templates/worker.md");
#[doc(hidden)]
pub const REVIEWER_TPL: &str = include_str!("templates/reviewer.md");
#[doc(hidden)]
pub const PLANNER_TPL: &str = include_str!("templates/planner.md");
/// Workflow-aware fragments (#222), substituted into the role templates above as
/// `{{WORKFLOW}}` (orchestrator) and `{{BLOCK_NOTE}}` (worker/reviewer/planner).
///
/// They are *fragments*, not templates, for one reason: `render_template` is a
/// dumb `{{KEY}}` replace with no conditionals, and it should stay that way. So
/// the conditional lives in Rust (`workflow::roster_is_custom`) and the prose
/// lives in markdown, where the rest of the prose lives and where it can be read
/// and reviewed as prose. **Both placeholders resolve to the empty string for the
/// default roster**, and both sit line-final in their templates, so a group with
/// no workflow gets instruction files that are byte-for-byte the pre-#222 ones.
const WORKFLOW_TPL: &str = include_str!("templates/workflow.md");
const BLOCK_TPL: &str = include_str!("templates/block.md");

/// Read-only containment note handed to a planner at spawn time as its kickoff
/// "branch note". The worktree denial (spawn cwd logic) and the CLI-level
/// write/commit denials (`build_agent_command`, `read_only`) enforce most of
/// this structurally; the note communicates the whole contract to the agent.
/// Exposed (doc-hidden) so tests can pin the exact text.
#[doc(hidden)]
pub const PLANNER_READONLY_NOTE: &str = "You explore the codebase read-only to produce an implementation plan. You never create branches, worktrees, commits, or PRs — your deliverable is a plan written as a GitHub issue comment.";

/// Hard ceiling on `max_agents` regardless of what the launcher asks for.
const MAX_AGENTS_CEILING: u32 = 12;

/// One-line notice delivered to the orchestrator when the live-agent cap
/// changes mid-session, so it re-plans against the new ceiling (its kickoff
/// prompt still carries the old, already-rendered {{MAX_AGENTS}}).
pub fn max_agents_notice(from: u32, to: u32) -> String {
    format!("[loomux] max live agents changed {from}→{to} — re-plan accordingly")
}

/// The idle-tick notice delivered to an autonomous group's orchestrator (#83):
/// names the `[loomux] idle tick` wake source the template documents and tells it
/// to run its monitoring/intake cadence. Kept in one place so the template's wake
/// clause and the delivered text can't drift.
pub fn idle_tick_notice() -> String {
    "[loomux] idle tick: you have been idle and autonomous mode is on. Run your \
     monitoring cadence now — re-sync (list_tasks, list_agents, get_state), poll \
     for labeled intake (agent-ready / agent-investigate) and START that work, and \
     re-check your open PRs (CI + new comments). You will get this at most once per \
     idle window; producing any output resets the clock."
        .to_string()
}

/// One-line notice delivered to the orchestrator when the auto-merge gate is
/// toggled mid-session (#83), so it learns the new merge policy without waiting to
/// re-read its kickoff config.
pub fn auto_merge_notice(on: bool) -> String {
    if on {
        "[loomux] auto-merge ENABLED for this group: you MAY now merge a PR yourself \
         once it has reviewer approval, green CI, and meets the issue's acceptance \
         criteria — audit and announce every merge, and still hold anything risky or \
         ambiguous for the human.".to_string()
    } else {
        "[loomux] auto-merge DISABLED for this group: the human merge gate is absolute \
         again — open the PR, report it, and never merge yourself.".to_string()
    }
}

/// One-line notice delivered to the orchestrator when the auto-release gate is
/// toggled mid-session (#83), so it learns the new release policy without waiting
/// to re-read its kickoff config. Independent of auto-merge.
pub fn auto_release_notice(on: bool) -> String {
    if on {
        "[loomux] auto-release ENABLED for this group: while autonomous you MAY now cut a \
         release yourself (`gh release …`, pushing a v* tag) once it is adequately \
         prepared — audit and announce every release, and still hold anything risky or \
         ambiguous for the human.".to_string()
    } else {
        "[loomux] auto-release DISABLED for this group: publishing a release/tag now \
         requires an explicit human release grant again — do not `gh release` or push a \
         v* tag yourself; ask the human to grant it.".to_string()
    }
}

/// Notice delivered to the orchestrator when supervised dangerous mode is toggled
/// (#83), or force-cleared because autonomous mode was enabled (`by_autonomous`).
pub fn dangerous_mode_notice(on: bool, by_autonomous: bool) -> String {
    if on {
        "[loomux] SUPERVISED DANGEROUS MODE enabled for this group: the human is present and \
         has authorized you to perform merges (to the default branch) and releases/tags \
         yourself, without a per-item grant. Audit and announce every merge/release; still \
         hold anything genuinely risky and flag it. This is a supervised session — the human \
         is watching.".to_string()
    } else if by_autonomous {
        "[loomux] supervised dangerous mode was turned OFF because autonomous mode was enabled \
         (the two are mutually exclusive). Merge/release authority now follows the autonomous \
         auto-merge / auto-release toggles + grants.".to_string()
    } else {
        "[loomux] supervised dangerous mode DISABLED for this group: the human gate is back — \
         open PRs and report, do not merge to the default branch or publish releases/tags \
         yourself unless the human grants it.".to_string()
    }
}

/// The POSIX `gh` shim (#83), with the real gh's absolute path baked in. Mirrors
/// the pure `gh_is_merge_invocation` / `gh_gate_decision` spec in shell: only
/// `gh pr merge` (and cheap `gh api` merge shapes) is gated; a merge onto the
/// default branch is allowed only when both the `autonomous` and `auto_merge`
/// markers are present in `$LOOMUX_GROUP_DIR`; a non-default base passes through;
/// an undeterminable base fails safe (block). Refusals/allows are appended to the
/// group's `audit.jsonl` in the backend's line format. Everything else `exec`s the
/// real gh with no extra work.
///
/// **Two independent gates live here (#222/#197).** The *human* gate above is one.
/// The other is the repo's own **workflow merge gate**: when `.loomux/workflow.yml`
/// declares `gates.merge`, loomux writes a `merge_gate` spec file into the group
/// dir and the shim refuses the merge until the named reviewers' recorded verdicts
/// (`verdicts/pr-<N>/<block>`, written by the `review_verdict` MCP tool) satisfy it.
/// It is checked **first**, so no grant and no autonomous marker can open it — the
/// `workflow::evaluate_merge_gate` spec is the pure mirror of that decision. With
/// no `merge_gate` file the shim behaves exactly as it did before #222.
#[doc(hidden)] // pub so the integration test can pin the security-critical guards
pub fn gh_shim_sh(real_gh: &str) -> String {
    // Template uses a placeholder (not format!) so the shell's own `$`/`{}` stay
    // literal. `date +%s%3N` is plain coreutils (no getrandom).
    const TPL: &str = r#"#!/bin/sh
# loomux gh shim (#83) — enforce the human merge gate. Generated by loomux; do not edit.
REAL_GH="__REAL_GH__"

loomux_audit() { # $1=action $2=detail-json
  ts=$(date +%s%3N 2>/dev/null); [ -z "$ts" ] && ts=0
  if [ -n "$LOOMUX_GROUP_DIR" ]; then
    # ONE printf of the whole line (record + \n) — O_APPEND is atomic per write,
    # and the backend can't lock us out of another process. Splitting this across
    # two printfs/redirections would let a concurrent writer splice the record
    # (#240). Keep it a single append.
    printf '{"ts_ms":%s,"actor":"gh-shim","action":"%s","detail":%s}\n' "$ts" "$1" "$2" \
      >> "$LOOMUX_GROUP_DIR/audit.jsonl" 2>/dev/null || true
  fi
}
loomux_block() { # $1=reason $2=base $3=pr
  printf '%s\n' "loomux: merge to the default branch requires the human gate — enable auto-merge (autonomous mode) or have the human grant this one merge (board Approve). Open the PR and report to the human; do NOT merge." >&2
  loomux_audit "merge-gate-blocked" "{\"reason\":\"$1\",\"base\":\"$2\",\"pr\":\"$3\"}"
  exit 1
}
# The WORKFLOW merge gate (#222/#197), distinct from the human gate above: this is
# the repo's own `gates.merge` clause, and it is an ADDITIONAL necessary condition —
# a human grant, autonomous auto-merge and supervised dangerous mode all sit BELOW
# it and none of them can open it. $1=reason (audit) $2=human-readable detail.
loomux_block_wf() { # $1=reason $2=detail
  printf '%s\n' "loomux: this repo's .loomux/workflow.yml declares a merge gate on PR #$num and it is NOT satisfied — $2. The merge is refused. Reviewers record their outcome with the review_verdict MCP tool (pass | fail | escalate); a fail/escalate from ANY named reviewer refuses the merge whatever the others said. Wait for the reviews, or take it to the human — do NOT work around this." >&2
  loomux_audit "merge-gate-workflow-blocked" "{\"reason\":\"$1\",\"pr\":\"$num\"}"
  exit 1
}
loomux_block_release() { # $1=tag $2=action
  printf '%s\n' "loomux: publishing a release/tag ($1) requires an explicit human grant — releases publish to the world (GitHub release + npm), which autonomous mode does NOT authorize. Ask the human to grant the release; do NOT publish." >&2
  loomux_audit "release-gate-blocked" "{\"tag\":\"$1\",\"action\":\"$2\"}"
  exit 1
}
# Consume a one-time grant file: delete it (one use only) and return 0 iff it was
# present AND unexpired (expiry = unix seconds on line 1). Expired grants are
# cleaned up too. Reading is atomic on the writer side (see atomic_write).
loomux_grant_ok() { # $1=grantfile
  gf="$1"
  [ -f "$gf" ] || return 1
  exp=$(head -n1 "$gf" 2>/dev/null)
  case "$exp" in ''|*[!0-9]*) exp=0 ;; esac
  now=$(date +%s 2>/dev/null); [ -z "$now" ] && now=0
  rm -f "$gf"
  [ "$now" -lt "$exp" ]
}
# The SINGLE release-gate decision (#83/#196): every release-publishing shape —
# `gh release create|edit|delete` AND the raw `gh api`/graphql equivalents (create
# a v* tag ref, create/edit/delete a release, graphql *Release mutation) — routes
# through here, so the api path can never diverge from the subcommand path. Allowed
# by autonomous+auto_release (blanket, not grant-consumed), supervised dangerous
# mode (human present, not autonomous), or a valid one-time per-tag grant; else
# fail-safe block. $1=tag ("" when not cheaply resolvable — then only the blanket
# markers can allow), $2=action label. Returns 0 to allow (caller execs the real
# gh); blocks with a message + exit 1 (never returns) otherwise.
loomux_release_gate() { # $1=tag $2=action
  _tag="$1"; _action="$2"
  if [ -n "$LOOMUX_GROUP_DIR" ] && [ -f "$LOOMUX_GROUP_DIR/autonomous" ] && [ -f "$LOOMUX_GROUP_DIR/auto_release" ]; then
    loomux_audit "release-gate-allowed" "{\"tag\":\"$_tag\",\"action\":\"$_action\"}"; return 0
  fi
  if [ -n "$LOOMUX_GROUP_DIR" ] && [ -f "$LOOMUX_GROUP_DIR/dangerous_mode" ] && [ ! -f "$LOOMUX_GROUP_DIR/autonomous" ]; then
    loomux_audit "release-gate-dangerous" "{\"tag\":\"$_tag\",\"action\":\"$_action\"}"; return 0
  fi
  _safe=$(printf '%s' "$_tag" | tr -c 'A-Za-z0-9._-' '_')
  _gf=""
  [ -n "$LOOMUX_GROUP_DIR" ] && [ -n "$_safe" ] && _gf="$LOOMUX_GROUP_DIR/release_grants/$_safe"
  if [ -n "$_gf" ] && loomux_grant_ok "$_gf"; then
    loomux_audit "release-gate-granted" "{\"tag\":\"$_tag\",\"action\":\"$_action\"}"; return 0
  fi
  loomux_block_release "$_tag" "$_action"
}

# Parse the argv ONCE (mirrors the Rust gh_positionals / gh_repo_flag spec):
# collect the command (cmd), subcommand (sub), the target selector (sel = 3rd
# positional: a PR ref for `pr merge`, a tag for `release …`), and the -R/--repo
# value — skipping flags and consuming the values of value-taking flags. gh accepts
# -R/--repo (and other flags) BEFORE or BETWEEN the command tokens, so scanning for
# positionals — not fixed argv slots — closes the `gh -R o/r pr merge` hole.
cmd=""; sub=""; sel=""; repo=""; want=""
for tok in "$@"; do
  if [ "$want" = "repo" ]; then repo="$tok"; want=""; continue; fi
  if [ "$want" = "skip" ]; then want=""; continue; fi
  case "$tok" in
    -R|--repo) want="repo"; continue ;;
    --repo=*) repo="${tok#--repo=}"; continue ;;
    -R?*) repo="${tok#-R}"; continue ;;
    -b|--body|-t|--subject|--title|-F|--body-file|--author-email|--match-head-commit|-n|--notes|--notes-file|--notes-start-tag|--target|--discussion-category) want="skip"; continue ;;
    --body=*|--subject=*|--title=*|--body-file=*|--author-email=*|--match-head-commit=*|--notes=*|--notes-file=*|--notes-start-tag=*|--target=*|--discussion-category=*) continue ;;
    -*) continue ;;
    *)
      if [ -z "$cmd" ]; then cmd="$tok"
      elif [ -z "$sub" ]; then sub="$tok"
      elif [ -z "$sel" ]; then sel="$tok"
      fi ;;
  esac
done

# RELEASE/TAG publish (#83): create/edit/delete a release is a publish-to-the-world
# action — allowed when the group is autonomous AND has opted in via the auto_release
# marker (parallel to autonomous+auto_merge for merges), OR by an explicit per-tag
# release grant. Read-only release subcommands (view/list/download) pass through.
if [ "$cmd" = "release" ]; then
  case "$sub" in
    create|edit|delete)
      loomux_release_gate "$sel" "$sub"   # allow (return) or block (exit); tag = $sel
      exec "$REAL_GH" "$@" ;;
    *) exec "$REAL_GH" "$@" ;;
  esac
fi

# RELEASE via raw `gh api` / graphql (#196): the `gh release` subcommand above is the
# ergonomic path, but the SAME publish can be driven through `gh api`. Decide by
# LOCUS — the request METHOD, the URL PATH, and the parsed `ref`/`query` field — never
# by substring-anywhere over the argv (a cosmetic `refs/heads/` in a header/jq/sha/
# query string must NOT be able to disguise a `refs/tags/` create; #196 r3). We parse
# gh api's own flags here (the shared positional parser above is tuned for pr/release).
if [ "$cmd" = "api" ]; then
  a_method=""; a_url=""; a_ref=""; a_query=""; a_qopaque=0; a_tagname=""
  a_inputval=""; a_hasparam=0; aw=""; seen_cmd=0
  for tok in "$@"; do
    if [ -n "$aw" ]; then
      case "$aw" in
        method) a_method=$(printf '%s' "$tok" | tr '[:lower:]' '[:upper:]') ;;
        field)
          a_hasparam=1
          case "$tok" in
            ref=*)      a_ref=${tok#ref=} ;;
            tag_name=*) a_tagname=${tok#tag_name=} ;;
            query=*)    q=${tok#query=}; case "$q" in @*) a_qopaque=1 ;; *) a_query=$q ;; esac ;;
          esac ;;
        input) a_hasparam=1; a_inputval="$tok" ;;
        skip) : ;;
      esac
      aw=""; continue
    fi
    case "$tok" in
      -X|--method) aw="method"; continue ;;
      -X?*)        a_method=$(printf '%s' "${tok#-X}" | tr '[:lower:]' '[:upper:]'); continue ;;
      --method=*)  a_method=$(printf '%s' "${tok#--method=}" | tr '[:lower:]' '[:upper:]'); continue ;;
      -f|-F|--field|--raw-field) aw="field"; continue ;;
      --field=*|--raw-field=*)
        a_hasparam=1; v=${tok#*=}
        case "$v" in
          ref=*)      a_ref=${v#ref=} ;;
          tag_name=*) a_tagname=${v#tag_name=} ;;
          query=*)    q=${v#query=}; case "$q" in @*) a_qopaque=1 ;; *) a_query=$q ;; esac ;;
        esac
        continue ;;
      --input) aw="input"; continue ;;
      --input=*) a_hasparam=1; a_inputval=${tok#--input=}; continue ;;
      # Other value-taking gh-api flags: consume the value so it can never be mistaken
      # for the URL, and so a decoy ref string inside it is never part of the locus.
      -H|--header|-q|--jq|-t|--template|--cache|--hostname|-p|--preview) aw="skip"; continue ;;
      --header=*|--jq=*|--template=*|--cache=*|--hostname=*|--preview=*) continue ;;
      -*) continue ;;   # boolean flags (--paginate, -i/--include, --slurp, --silent, …)
      *) # first bare positional is the `api` command token; the next is the endpoint.
         if [ "$seen_cmd" = "0" ]; then seen_cmd=1; elif [ -z "$a_url" ]; then a_url="$tok"; fi ;;
    esac
  done
  [ -z "$a_method" ] && { [ "$a_hasparam" = "1" ] && a_method="POST" || a_method="GET"; }
  # gh reads the ref from a JSON body too (`--input <file>`): parse the file's "ref"
  # so a heads-locus body is provably a branch. `--input -` (stdin) is unparseable →
  # ref stays empty → cannot prove heads → fail-safe gate below.
  if [ -n "$a_inputval" ] && [ "$a_inputval" != "-" ] && [ -z "$a_ref" ] && [ -f "$a_inputval" ]; then
    body=$(cat "$a_inputval" 2>/dev/null)
    case "$body" in
      *'"ref"'*) r=${body#*\"ref\"}; r=${r#*:}; r=${r#*\"}; a_ref=${r%%\"*} ;;
    esac
  fi

  is_write=0; case "$a_method" in GET|HEAD) is_write=0 ;; *) is_write=1 ;; esac
  # URL PATH only (strip any ?query — a decoy `?d=refs/heads/z` must not read as heads).
  a_path=${a_url%%\?*}
  path_low=$(printf '%s' "$a_path" | tr '[:upper:]' '[:lower:]')
  ref_low=$(printf '%s' "$a_ref" | tr '[:upper:]' '[:lower:]')

  is_rel=0; rtag=""
  # Recognize the graphql endpoint by SUFFIX (like the REST URL arms below), not an
  # exact 'graphql' — gh also accepts `/graphql` and the full-URL host form, all sent
  # as a POST of {"query":…} (#196 r4). Any call to that locus is a graphql write.
  is_graphql=0
  case "$path_low" in graphql|/graphql|*/graphql) is_graphql=1 ;; esac
  if [ "$is_graphql" = "1" ]; then
    # If the query is opaque (from --input/stdin or query=@file) we cannot scan it →
    # fail-safe gate. Otherwise: any ref/tag/release-CREATING mutation gates
    # UNCONDITIONALLY — there is NO "prove it's a safe branch from the text" logic in
    # the graphql arm, by design. Every text heuristic we tried (a refs/tags literal, a
    # -F ref= variable, a no-`$`-variables rule) was defeated by the next encoding —
    # graphql variables, comments, aliases, and string escapes (`refs\/tags\/`) each dodge
    # a text scan, and the next encoding would too (#196 r6). A graphql createRef to a
    # BRANCH is a rare corner (agents branch via `git push` or REST `git/refs`, which the
    # REST arm classifies by real locus); gating it fails safe — markers/grant still
    # allow it. A non-mutation read query carries none of these tokens → passes.
    if [ -n "$a_inputval" ] || [ "$a_qopaque" = "1" ]; then
      is_rel=1
    elif [ -n "$a_query" ]; then
      ql=$(printf '%s' "$a_query" | tr '[:upper:]' '[:lower:]')
      # Full create+move+DELETE coverage of refs/tags/releases, matching the REST arm
      # (which gates POST/PATCH/DELETE of git/refs|git/tags and create/edit/delete of
      # releases). deleteRef is destructive — it can drop a published v* tag ref — so it
      # gates like DELETE git/refs/tags/* and deleteRelease. Matched by the field-name
      # token (an unescapable identifier), consistent with the class-closing fix.
      case "$ql" in
        *createref*|*updateref*|*deleteref*|*createtag*|*deletetag*|*createrelease*|*updaterelease*|*deleterelease*) is_rel=1 ;;
      esac
      # resolve the tag for grant-keying: a refs/tags variable, else an inline literal.
      case "$ref_low" in refs/tags/*) rtag=${a_ref#refs/tags/}; rtag=${rtag%% *} ;; esac
      if [ -z "$rtag" ]; then
        case "$a_query" in
          *tagName:*)   rest=${a_query#*tagName:}; rest=$(printf '%s' "$rest" | tr -d ' "'); rtag=${rest%%,*}; rtag=${rtag%%\}*}; rtag=${rtag%%)*} ;;
          *refs/tags/*) rest=${a_query#*refs/tags/}; rest=$(printf '%s' "$rest" | tr -d ' "'); rtag=${rest%%,*}; rtag=${rtag%%\}*}; rtag=${rtag%%)*} ;;
        esac
      fi
    fi
  elif [ "$is_write" = "1" ]; then
    # A non-GET write to the git refs/tags plumbing, decided by the URL path SEGMENT.
    case "$path_low" in
      git/refs|git/refs/*|*/git/refs|*/git/refs/*|git/tags|git/tags/*|*/git/tags|*/git/tags/*)
        # Exempt ONLY when the ref locus is PROVABLY a branch: URL path .../refs/heads/…
        # OR the parsed ref field refs/heads/… — AND refs/tags/ absent from that locus.
        heads=0; tags=0
        case "$path_low" in */refs/heads/*) heads=1 ;; esac
        case "$ref_low"  in refs/heads/*)   heads=1 ;; esac
        case "$path_low" in */refs/tags/*)  tags=1 ;; esac
        case "$ref_low"  in refs/tags/*)    tags=1 ;; esac
        if [ "$heads" = "1" ] && [ "$tags" = "0" ]; then is_rel=0; else is_rel=1; fi ;;
    esac
    # A write to the releases endpoint (read-only GET list/view already excluded above).
    if [ "$is_rel" = "0" ]; then
      case "$path_low" in releases|releases/*|*/releases|*/releases/*) is_rel=1 ;; esac
    fi
    # Resolve the tag for grant-keying from the locus (ref field, URL path, tag_name).
    if [ "$is_rel" = "1" ]; then
      case "$ref_low" in refs/tags/*) rtag=${a_ref#refs/tags/}; rtag=${rtag%% *} ;; esac
      case "$path_low" in */git/refs/tags/*) [ -z "$rtag" ] && { rest=${a_path##*/git/refs/tags/}; rtag=${rest%%/*}; } ;; esac
      [ -z "$rtag" ] && [ -n "$a_tagname" ] && rtag="$a_tagname"
    fi
  fi
  if [ "$is_rel" = "1" ]; then
    loomux_release_gate "$rtag" "api"   # allow (return) or block (exit)
    exec "$REAL_GH" "$@"
  fi
fi

# Is this a merge we must gate? `gh pr merge` (wherever flags land), or an api shape.
is_merge=0
if [ "$cmd" = "pr" ] && [ "$sub" = "merge" ]; then
  is_merge=1
elif [ "$cmd" = "api" ]; then
  all="$*"
  low=$(printf '%s' "$all" | tr '[:upper:]' '[:lower:]')
  case "$low" in *mergepullrequest*) is_merge=1 ;; esac
  case "$all" in *pulls*) case "$all" in *"/merge"*) is_merge=1 ;; esac ;; esac
fi

if [ "$is_merge" = "0" ]; then
  exec "$REAL_GH" "$@"
fi

# A MERGE WITH NO GROUP DIR IS A MERGE loomux CANNOT GATE — refuse it. Every agent
# pane gets `LOOMUX_GROUP_DIR` and the shimmed PATH together (`agent_pane_env`), and
# a human's own shell never has the shim on PATH at all — so reaching the shim
# without the variable means it was unset on the way (`env -u LOOMUX_GROUP_DIR gh pr
# merge …`), which is evasion, not a supported flow. The human gate already fails
# closed on this shape for a default-branch merge (no markers, no grant → blocked);
# without this, an unset variable still slipped a NON-default merge past the workflow
# gate — with nothing in the audit, because there is no audit log to write to.
# Symmetry is the honest fix. (This closes the cheap shape only: an agent with a
# shell can still call the real gh by absolute path. See doc/design/workflows.md →
# "The bypass surface, honestly".)
if [ -z "$LOOMUX_GROUP_DIR" ]; then
  printf '%s\n' "loomux: refusing to merge — LOOMUX_GROUP_DIR is not set, so loomux cannot check this merge against the group's gates. Run gh from your agent pane's normal environment; do NOT unset it." >&2
  exit 1
fi

# A raw `gh api` merge has no cheaply-resolvable base ref → fail-safe block.
if [ "$cmd" = "api" ]; then
  loomux_block "api-merge" "(api)" "?"
fi

# Resolve the PR's base branch AND number via the REAL gh (one call), honoring the
# SAME -R/--repo the user passed (rev-79 F2). The number keys the per-PR grant, so
# a grant for one PR can't authorize merging another.
rf=""
[ -n "$repo" ] && rf="-R $repo"
info=$("$REAL_GH" pr view $rf $sel --json baseRefName,number --jq '.baseRefName+" "+(.number|tostring)' 2>/dev/null)
base=${info%% *}
num=${info##* }
default=$("$REAL_GH" repo view $rf --json defaultBranchRef --jq .defaultBranchRef.name 2>/dev/null)

if [ -z "$base" ] || [ -z "$default" ]; then
  loomux_block "unverifiable-base" "$base" "$sel"
fi

# ── THE WORKFLOW MERGE GATE (#222, closing the loomux half of #197) ───────────
# When the repo declares `gates.merge`, loomux writes a `merge_gate` spec file into
# the group dir, and every reviewer's `review_verdict` lands in
# `verdicts/pr-<N>/<block>` with the verdict word (pass|fail|escalate) as line 1.
#
# THREE properties, in the order they are enforced:
#  1. It runs BEFORE the human-grant / autonomous / dangerous-mode openings below,
#     so none of them can satisfy it. #197 Scope B asks for an auto-merge to be
#     "structurally impossible until every required review verdict is recorded
#     PASS"; a gate that a grant could override would not be that.
#  2. It applies to EVERY merge of the PR, not only to the default branch. The
#     declared reviewers reviewed *this PR*; where it lands doesn't change whether
#     they finished. (The human gate below stays default-branch-only — unchanged.)
#  3. No `merge_gate` file → this whole block is skipped → byte-for-byte the
#     pre-#222 flow. Every group without a workflow file is in that case.
if [ -f "$LOOMUX_GROUP_DIR/merge_gate" ]; then
  gatef="$LOOMUX_GROUP_DIR/merge_gate"
  # Without a PR number no verdict can be attributed to this merge → fail closed.
  [ -n "$num" ] || loomux_block_wf "unresolved-pr" "loomux could not resolve the PR number, so it cannot check the recorded verdicts against it"
  # THE REVISION THIS MERGE WOULD LAND. A verdict binds to a COMMIT, not to a PR
  # number: without this, two reviewers pass #7, the worker pushes "fixed lint",
  # and the gate still reads green over code nobody reviewed — #197's failure class,
  # and the reason GitHub dismisses stale approvals on new commits. Unresolvable →
  # refuse, the same fail-safe an undeterminable base takes.
  cur_head=$("$REAL_GH" pr view $rf "$num" --json headRefOid --jq .headRefOid 2>/dev/null)
  cur_head=$(printf '%s' "$cur_head" | tr '[:upper:]' '[:lower:]')
  [ -n "$cur_head" ] || loomux_block_wf "unresolved-head" "loomux could not resolve the PR's current head commit, so it cannot tell whether the recorded verdicts reviewed the code that would merge"
  # No globbing anywhere below: the gate file's tokens are word-split into `for`
  # loops, and a security shim should not leave the next reader working out whether
  # a `*` could reach a filename. (loomux never writes one — sanitize_id /
  # sanitize_condition reject glob characters — so this is belt, not braces.)
  set -f
  g_req="all-pass"; g_thr=0; g_revs=""; g_also=""
  # `|| [ -n "$g_k" ]` is load-bearing: POSIX `read` returns non-zero at EOF, so a
  # final line with NO trailing newline would otherwise be silently DROPPED — and a
  # dropped `reviewer`/`also` line makes the gate WEAKER, which is the one direction
  # this design says must never happen. A truncated gate file is exactly the case
  # the malformed-gate check below claims to handle.
  while read -r g_k g_v g_w || [ -n "$g_k" ]; do
    case "$g_k" in
      \#*|'')   : ;;   # comment / blank
      require)  g_req="$g_v"; [ -n "$g_w" ] && g_thr="$g_w" ;;
      reviewer) [ -n "$g_v" ] && g_revs="$g_revs $g_v" ;;
      also)     [ -n "$g_v" ] && g_also="$g_also $g_v" ;;
      # An unrecognized key is NOT skipped. loomux writes an `unrepresentable` line
      # when it cannot safely serialize a token (rather than dropping the clause),
      # and a hand edit or a truncation lands here too. Skipping any of them would
      # silently drop a requirement from a gate.
      *) loomux_block_wf "malformed-gate" "the merge gate file contains a line loomux cannot parse ('$g_k') — a gate it cannot read in full is not a gate it will enforce in part. Fix .loomux/workflow.yml and relaunch the group" ;;
    esac
  done < "$gatef"
  # A gate naming nobody, or a threshold with no usable number, is a MALFORMED gate
  # — refuse rather than wave it through. (loomux only ever writes well-formed gate
  # files; this is the hand-edited/truncated case.)
  [ -n "$g_revs" ] || loomux_block_wf "malformed-gate" "the declared merge gate names no reviewers"
  # The gate's RULE, validated up front. An unrecognized `require` is refused, not
  # quietly read as all-pass: `all-pass` happens to be the strict one today, so the
  # silent fallback looked safe — but it means the shim would enforce a rule the file
  # does not state, and the Rust half already calls this file MALFORMED. Two halves of
  # one gate must agree about what it says, not merely land on the same answer by luck.
  case "$g_req" in
    all-pass) : ;;
    threshold) case "$g_thr" in ''|*[!0-9]*) g_thr=0 ;; esac
               [ "$g_thr" -ge 1 ] || loomux_block_wf "malformed-gate" "the declared merge gate says require: threshold but carries no usable threshold number" ;;
    *) loomux_block_wf "malformed-gate" "the merge gate declares an unrecognized require value ('$g_req') — loomux understands 'all-pass' and 'threshold'. A rule it cannot read is not a rule it will guess at" ;;
  esac
  g_pass=0; g_out=""; g_bad=""; g_stale=""
  for g_r in $g_revs; do
    g_vf="$LOOMUX_GROUP_DIR/verdicts/pr-$num/$g_r"
    g_v=""; g_vh=""
    if [ -f "$g_vf" ]; then
      g_v=$(head -n1 "$g_vf" 2>/dev/null)                  # line 1: the verdict word
      g_vh=$(head -n2 "$g_vf" 2>/dev/null | tail -n1)      # line 2: the head it reviewed
    fi
    case "$g_v" in
      # A pass counts ONLY for the revision it reviewed. Recorded against an older
      # head (or against none) → stale: the branch moved, and what that reviewer
      # approved is not what would merge.
      pass)          if [ "$g_vh" = "$cur_head" ]; then g_pass=$((g_pass+1)); else g_stale="$g_stale $g_r"; fi ;;
      # A blocking verdict is revision-INDEPENDENT: "this PR has a defect" does not
      # stop being true because the author pushed more code. Re-review clears it.
      fail|escalate) g_bad="$g_bad $g_r" ;;
      # No verdict recorded — or one this build cannot read (a hand-edited `PASS`,
      # say), which is NOT a pass. The Rust `Verdict::parse` is lowercase-strict for
      # exactly this reason: one token definition, and both halves fail closed on it.
      *)             g_out="$g_out $g_r" ;;
    esac
  done
  # Blockers beat approvals (#197 A.3): one fail/escalate refuses the merge whatever
  # the others recorded and whatever the threshold says. Checked before any counting.
  [ -z "$g_bad" ] || loomux_block_wf "verdict-blocks" "reviewer(s)$g_bad recorded a fail/escalate verdict"
  # Say only what is TRUE: a gate held up purely by stale verdicts must not also claim
  # it is waiting on a verdict from nobody, and vice versa. A refusal message is the
  # only thing the agent reading it has to act on.
  g_why=""
  [ -n "$g_out" ] && g_why="no verdict yet from reviewer(s)$g_out"
  if [ -n "$g_stale" ]; then
    [ -n "$g_why" ] && g_why="$g_why; "
    g_why="${g_why}reviewer(s)$g_stale passed an EARLIER revision and must re-review"
  fi
  case "$g_req" in
    threshold)
      [ "$g_pass" -ge "$g_thr" ] || loomux_block_wf "below-threshold" "only $g_pass of the required $g_thr PASS verdicts cover the PR's current head $cur_head — $g_why" ;;
    *)
      # all-pass — THE #151 CASE: a reviewer that has not recorded anything (or whose
      # pass predates the code that would merge) keeps the gate shut, however loudly
      # the others approved.
      [ -z "$g_why" ] || loomux_block_wf "verdict-outstanding" "the PR is now at $cur_head — $g_why" ;;
  esac
  # `also:` conditions. ci-green is checked against the real gh; anything this build
  # does not know how to check FAILS CLOSED — a clause loomux silently ignored would
  # make a stricter-looking workflow file a weaker one, the worst thing a gate can do.
  for g_c in $g_also; do
    case "$g_c" in
      ci-green)
        if ! "$REAL_GH" pr checks $rf "$num" >/dev/null 2>&1; then
          loomux_block_wf "ci-not-green" "the gate requires ci-green and 'gh pr checks $num' is not all-green (failing, still running, or no checks reported)"
        fi ;;
      *) loomux_block_wf "unknown-condition" "the gate names the condition '$g_c', which this loomux build does not know how to check — an unknown condition fails closed. Remove it from gates.merge.also, or upgrade loomux" ;;
    esac
  done
  set +f
  loomux_audit "merge-gate-workflow-ok" "{\"pr\":\"$num\",\"require\":\"$g_req\",\"passes\":$g_pass,\"head\":\"$cur_head\"}"
fi

if [ "$base" != "$default" ]; then
  exec "$REAL_GH" "$@"   # integration-branch merge — untouched by the HUMAN gate
fi
# base == default: blanket-allowed while autonomous + auto_merge.
if [ -n "$LOOMUX_GROUP_DIR" ] && [ -f "$LOOMUX_GROUP_DIR/autonomous" ] && [ -f "$LOOMUX_GROUP_DIR/auto_merge" ]; then
  loomux_audit "merge-gate-allowed" "{\"base\":\"$default\",\"pr\":\"$num\"}"
  exec "$REAL_GH" "$@"
fi
# Supervised dangerous mode: the human is present and enabled it (only valid while
# NOT autonomous). Distinct audit marker.
if [ -n "$LOOMUX_GROUP_DIR" ] && [ -f "$LOOMUX_GROUP_DIR/dangerous_mode" ] && [ ! -f "$LOOMUX_GROUP_DIR/autonomous" ]; then
  loomux_audit "merge-gate-dangerous" "{\"base\":\"$default\",\"pr\":\"$num\"}"
  exec "$REAL_GH" "$@"
fi
# Otherwise: a one-time human grant for THIS pr authorizes exactly one merge.
gf=""
[ -n "$LOOMUX_GROUP_DIR" ] && [ -n "$num" ] && gf="$LOOMUX_GROUP_DIR/merge_grants/pr-$num"
if [ -n "$gf" ] && loomux_grant_ok "$gf"; then
  loomux_audit "merge-gate-granted" "{\"base\":\"$default\",\"pr\":\"$num\"}"
  exec "$REAL_GH" "$@"
fi
loomux_block "gate-closed" "$default" "$num"
"#;
    // Normalize to LF: the raw-string newlines follow this source file's line
    // endings, which git may check out as CRLF on Windows — but a CRLF `#!/bin/sh`
    // script is broken under POSIX sh. The `.cmd` wrapper (which needs CRLF) is
    // built separately with explicit `\r\n`.
    TPL.replace("__REAL_GH__", real_gh).replace("\r\n", "\n")
}

/// The Windows `gh.cmd` wrapper: delegates to the POSIX shim (single source of
/// gate logic) via `sh`, or runs the real gh when no `sh` is on PATH (a documented
/// bypass). `real_gh` is forward-slashed but valid for `CreateProcess`.
fn gh_shim_cmd(real_gh: &str) -> String {
    let real_bs = real_gh.replace('/', "\\");
    format!(
        "@echo off\r\n\
         rem loomux gh shim (#83) — delegate to the POSIX shim; run real gh if no sh.\r\n\
         setlocal\r\n\
         for %%S in (sh.exe) do set \"LOOMUX_SH=%%~$PATH:S\"\r\n\
         if defined LOOMUX_SH (\r\n\
         \x20 \"%LOOMUX_SH%\" \"%~dp0gh\" %*\r\n\
         ) else (\r\n\
         \x20 \"{real_bs}\" %*\r\n\
         )\r\n\
         exit /b %errorlevel%\r\n"
    )
}

/// The POSIX `git` shim (#83): gates a `git push` that publishes a TAG (a `v*`
/// tag push triggers `release.yml` → GitHub release + npm), requiring an explicit
/// release grant. Local `git tag` is harmless — only the push reaches the world —
/// so only `git push` is inspected, and only when it targets a tag; every other
/// git call (including a plain branch push) `exec`s the real git with no extra
/// work. Mirrors the pure `git_tag_push` spec.
#[doc(hidden)] // pub so the integration test can pin the guards
pub fn git_shim_sh(real_git: &str) -> String {
    const TPL: &str = r#"#!/bin/sh
# loomux git shim (#83) — gate release/tag pushes. Generated by loomux; do not edit.
REAL_GIT="__REAL_GIT__"

loomux_audit() { # $1=action $2=detail-json
  ts=$(date +%s%3N 2>/dev/null); [ -z "$ts" ] && ts=0
  if [ -n "$LOOMUX_GROUP_DIR" ]; then
    # ONE printf of the whole line — see the gh shim's note (#240): cross-process
    # append atomicity is per write syscall, and no backend mutex reaches here.
    printf '{"ts_ms":%s,"actor":"git-shim","action":"%s","detail":%s}\n' "$ts" "$1" "$2" \
      >> "$LOOMUX_GROUP_DIR/audit.jsonl" 2>/dev/null || true
  fi
}
loomux_block_release() { # $1=tag $2=action
  printf '%s\n' "loomux: pushing a release tag ($1) requires an explicit human grant — a v* tag push publishes to the world (GitHub release + npm via release.yml), which autonomous mode does NOT authorize. Ask the human to grant the release; do NOT push the tag." >&2
  loomux_audit "release-gate-blocked" "{\"tag\":\"$1\",\"action\":\"$2\"}"
  exit 1
}
loomux_grant_ok() { # $1=grantfile
  gf="$1"
  [ -f "$gf" ] || return 1
  exp=$(head -n1 "$gf" 2>/dev/null)
  case "$exp" in ''|*[!0-9]*) exp=0 ;; esac
  now=$(date +%s 2>/dev/null); [ -z "$now" ] && now=0
  rm -f "$gf"
  [ "$now" -lt "$exp" ]
}

# Find the git subcommand, skipping value-taking globals. Non-push → exec now.
cmd=""; want=""
for tok in "$@"; do
  if [ "$want" = "1" ]; then want=""; continue; fi
  case "$tok" in
    -C|-c|--git-dir|--work-tree|--namespace|--exec-path) want="1"; continue ;;
    -*) continue ;;
    *) cmd="$tok"; break ;;
  esac
done
if [ "$cmd" != "push" ]; then
  exec "$REAL_GIT" "$@"
fi

# Bulk tag pushes can't be matched to a single-tag grant → block with guidance.
for a in "$@"; do
  case "$a" in
    --tags|--follow-tags|--mirror)
      printf '%s\n' "loomux: a bulk tag push ($a) is not allowed — push the specific approved tag and have the human grant that release." >&2
      loomux_audit "release-gate-blocked" "{\"tag\":\"(bulk)\",\"action\":\"push $a\"}"
      exit 1 ;;
  esac
done

# Scan refspecs after `push` (skip the remote) for a tag ref.
seen=0; got_remote=0; want=""; tag=""; prevtag=0
for tok in "$@"; do
  if [ "$want" = "1" ]; then want=""; continue; fi
  case "$tok" in
    -C|-c|--git-dir|--work-tree|--namespace|--exec-path) want="1"; continue ;;
    -*) continue ;;
    *)
      if [ "$seen" = "0" ]; then [ "$tok" = "push" ] && seen=1; continue; fi
      if [ "$got_remote" = "0" ]; then got_remote=1; continue; fi
      if [ "$prevtag" = "1" ]; then tag="$tok"; break; fi
      if [ "$tok" = "tag" ]; then prevtag=1; continue; fi
      dst=${tok##*:}; dst=${dst#+}
      # Match release.yml's on.push.tags (v*) — MUST track it; a bare `v*` is only
      # a candidate, confirmed a real tag (not a same-named branch) below.
      case "$dst" in
        refs/tags/*) tag=${dst#refs/tags/}; break ;;
        v*)
          if "$REAL_GIT" rev-parse -q --verify "refs/tags/$dst" >/dev/null 2>&1; then tag="$dst"; break; fi ;;
      esac ;;
  esac
done

if [ -z "$tag" ]; then
  exec "$REAL_GIT" "$@"   # branch push — untouched
fi
# Blanket: autonomous + auto_release opt-in (parallel to the gh release path).
if [ -n "$LOOMUX_GROUP_DIR" ] && [ -f "$LOOMUX_GROUP_DIR/autonomous" ] && [ -f "$LOOMUX_GROUP_DIR/auto_release" ]; then
  loomux_audit "release-gate-allowed" "{\"tag\":\"$tag\",\"action\":\"push\"}"
  exec "$REAL_GIT" "$@"
fi
# Supervised dangerous mode (human present, not autonomous). Distinct audit.
if [ -n "$LOOMUX_GROUP_DIR" ] && [ -f "$LOOMUX_GROUP_DIR/dangerous_mode" ] && [ ! -f "$LOOMUX_GROUP_DIR/autonomous" ]; then
  loomux_audit "release-gate-dangerous" "{\"tag\":\"$tag\",\"action\":\"push\"}"
  exec "$REAL_GIT" "$@"
fi
# Otherwise a one-time per-tag grant authorizes exactly this tag push.
safe=$(printf '%s' "$tag" | tr -c 'A-Za-z0-9._-' '_')
gf=""
[ -n "$LOOMUX_GROUP_DIR" ] && [ -n "$safe" ] && gf="$LOOMUX_GROUP_DIR/release_grants/$safe"
if [ -n "$gf" ] && loomux_grant_ok "$gf"; then
  loomux_audit "release-gate-granted" "{\"tag\":\"$tag\",\"action\":\"push\"}"
  exec "$REAL_GIT" "$@"
fi
loomux_block_release "$tag" "push"
"#;
    // Normalize to LF (see gh_shim_sh) — a CRLF POSIX script is broken.
    TPL.replace("__REAL_GIT__", real_git).replace("\r\n", "\n")
}

/// The Windows `git.cmd` wrapper: delegates to the POSIX git shim via `sh`, or runs
/// the real git when no `sh` is on PATH (documented bypass). Same shape as gh.cmd.
fn git_shim_cmd(real_git: &str) -> String {
    let real_bs = real_git.replace('/', "\\");
    format!(
        "@echo off\r\n\
         rem loomux git shim (#83) — delegate to the POSIX shim; run real git if no sh.\r\n\
         setlocal\r\n\
         for %%S in (sh.exe) do set \"LOOMUX_SH=%%~$PATH:S\"\r\n\
         if defined LOOMUX_SH (\r\n\
         \x20 \"%LOOMUX_SH%\" \"%~dp0git\" %*\r\n\
         ) else (\r\n\
         \x20 \"{real_bs}\" %*\r\n\
         )\r\n\
         exit /b %errorlevel%\r\n"
    )
}

/// The notice delivered once when an autonomous group's token budget is exhausted
/// and idle-ticking is suspended (#83). Tokens, not dollars (see `usage.rs`).
pub fn autonomy_budget_notice(spent: u64, budget: u64) -> String {
    format!(
        "[loomux] autonomy budget exhausted ({spent} of {budget} tokens spent since \
         autonomous mode was enabled) — autonomous mode has been SUSPENDED. Stop any \
         autonomous pulls and tell the human: raise the budget or toggle autonomous \
         mode back on to resume (re-enabling is explicit consent and re-anchors the \
         meter)."
    )
}

/// Quiet window a group's cap must fall silent for before its coalesced
/// cap-change notice is delivered. Rapid stepper clicks (#79) each persist,
/// enforce, and audit immediately, but the token-costing orchestrator notice
/// waits out this window and then spans the whole burst (first change's `from`
/// → last change's `to`), so a flurry of clicks is one prompt, not many.
const MAX_NOTICE_DEBOUNCE: Duration = Duration::from_secs(3);
/// How often the flusher loop checks for a debounced cap-change notice whose
/// window has elapsed. Well under `MAX_NOTICE_DEBOUNCE` so the delivered notice
/// lags the last click by at most a tick beyond the debounce.
const MAX_NOTICE_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// A cap-change notice awaiting its debounce window (#79). `from` is the cap
/// before the burst began — preserved across coalesced changes so the notice
/// reads end-to-end; `to` is the latest cap; `due_ms` is the Unix-ms at which,
/// absent any further change, the notice fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingMaxNotice {
    from: u32,
    to: u32,
    due_ms: u64,
}

/// Fold one cap change into the per-group debounce map (#79). A change that
/// lands while a notice is still pending keeps the original `from` (so the
/// coalesced notice spans the whole burst) and only advances `to` and pushes
/// the deadline out; the first change of a burst seeds a fresh entry. Pure, so
/// the coalescing is unit-testable without a clock or a live registry.
fn record_max_notice(
    pending: &mut HashMap<String, PendingMaxNotice>,
    group: &str,
    from: u32,
    to: u32,
    now: u64,
    debounce: Duration,
) {
    let due_ms = now.saturating_add(debounce.as_millis() as u64);
    pending
        .entry(group.to_string())
        .and_modify(|p| {
            p.to = to;
            p.due_ms = due_ms;
        })
        .or_insert(PendingMaxNotice { from, to, due_ms });
}

/// Drain the notices whose debounce window has elapsed (`due_ms <= now`),
/// returning `(group, from, to)` for each that is a real net change. A burst
/// that nets back to where it started (e.g. 4→3→4) is dropped without a notice
/// — no orchestrator tokens spent announcing a no-op. Pure, so the flush
/// decision is unit-testable without sleeping out the debounce.
fn take_due_max_notices(
    pending: &mut HashMap<String, PendingMaxNotice>,
    now: u64,
) -> Vec<(String, u32, u32)> {
    let due: Vec<String> = pending
        .iter()
        .filter(|(_, p)| p.due_ms <= now)
        .map(|(g, _)| g.clone())
        .collect();
    let mut out = Vec::new();
    for g in due {
        if let Some(p) = pending.remove(&g) {
            if p.from != p.to {
                out.push((g, p.from, p.to));
            }
        }
    }
    out
}

/// Upper bound on the idle-worker auto-kill timeout (24h); 0 disables it.
const MAX_IDLE_KILL_MINUTES: u32 = 1440;
/// Upper bound on the spawn-rate guardrail; 0 = unlimited.
const MAX_SPAWNS_PER_HOUR: u32 = 240;
/// Sliding window the spawn-rate guardrail counts spawns over.
const SPAWN_RATE_WINDOW_MS: u64 = 60 * 60 * 1000;
/// How often the idle reaper wakes to look for workers to auto-kill.
const IDLE_REAP_INTERVAL: Duration = Duration::from_secs(30);
/// How often the watchdog wakes to look for stalled working agents.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(30);
/// How often the low-disk backstop samples free space on the workspace drive
/// (#134). Slow on purpose — disk pressure builds over minutes of cargo builds,
/// not seconds — so the sysinfo scan stays negligible.
const DISK_CHECK_INTERVAL: Duration = Duration::from_secs(60);
/// Arm the low-disk notice when free space on the workspace drive drops below
/// this (#134). A disk-full write is what destroyed the board in the incident
/// (#133); 5 GB leaves headroom for one more cold cargo build (~5–7 GB) to be
/// reclaimed before writes start failing at 0.
const LOW_DISK_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Clear the low-disk latch only once free space recovers past this higher mark
/// (arming + 2 GB). The hysteresis stops a disk hovering at the threshold from
/// re-notifying every tick.
const LOW_DISK_CLEAR_BYTES: u64 = LOW_DISK_BYTES + 2 * 1024 * 1024 * 1024;
/// Upper bound on the watchdog stall timeout (24h); 0 disables it.
const MAX_WATCHDOG_STALL_MINUTES: u32 = 1440;
/// Autonomous mode (#83): how often the idle-tick loop wakes to check whether an
/// autonomous group's orchestrator has gone output-quiet long enough to warrant a
/// tick, and to enforce autonomy budgets. Coarser than the watchdog because the
/// gate is measured in minutes, not seconds — a 60s wake is cheap and precise
/// enough. See `start_idle_tick` / `run_idle_tick`.
const IDLE_TICK_INTERVAL: Duration = Duration::from_secs(60);
/// Autonomous mode (#83): default output-quiet window before an idle tick fires,
/// when the group's `idle_tick_minutes` guardrail isn't set. Lowered from the
/// original 15 to **5** after a live test: a human who turns autonomous mode on
/// expects action within a few minutes, and a 15-minute default simply never fired
/// in an 8-minute session. Per-group tunable (`set_idle_tick_minutes`) so the human
/// can drop it to 1–2 min to verify quickly. See `idle_tick_should_fire`.
const DEFAULT_IDLE_TICK_MINUTES: u32 = 5;
/// Upper bound on the idle-tick quiet window (24h); a floor of 1 min is enforced in
/// `clamped()` (0 is treated as "unset" → default, not "disabled": the `autonomous`
/// marker is the on/off switch, so a 0 here must never silently stop ticking).
const MAX_IDLE_TICK_MINUTES: u32 = 1440;
/// Autonomous mode (#83): default per-tick pty-output growth (bytes) at or above
/// which the orchestrator counts as *actively working* — the growth resets the
/// quiet clock and the one-notice latch. Below it, growth is treated as idle
/// **repaint noise** (statusline/spinner frames keep `output_total` creeping while
/// the CLI is parked) and does NOT reset the clock, so an occasional repaint can't
/// starve the tick (the bug where a single stray byte demanded another full quiet
/// window). A coarse burst floor — there is no output-frame classifier — since a
/// real orchestrator turn dumps many KB while an idle repaint is a few hundred
/// bytes. **Default justified by measurement:** a full idle Claude Code input-box
/// render (box-drawing + ANSI) is ~164 bytes (`tests/fixtures/attention/
/// idle-input-box.txt`), so 2048 leaves ~12× headroom over a complete idle
/// repaint. Because this rides the exact wake+spend axis that already failed once
/// (finding 2) and a chattier CLI could exceed it, it is a **live-tunable guardrail
/// knob** (`Guardrails.idle_activity_floor_bytes`), not a bare const — see
/// `set_idle_activity_floor` / `idle_output_is_activity`.
const DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES: u64 = 2048;
/// Upper bound on the activity floor (1 MiB): beyond this no real orchestrator turn
/// would clear it, so ticking would fire even while working. Floor is 1 (any growth
/// = activity, the original behavior) — both enforced in `clamped()`.
const MAX_IDLE_ACTIVITY_FLOOR_BYTES: u64 = 1024 * 1024;
/// Autonomous mode (#83): hard backstop on idle ticks delivered per rolling hour,
/// independent of the one-notice latch — the analogue of `max_spawns_per_hour` for
/// the tick source. With a minutes-scale quiet gate the latch already bounds this
/// near ~one per window; the cap catches any pathological re-arm. 0 would disable it.
const MAX_IDLE_TICKS_PER_HOUR: u32 = 6;
/// How often the attention scan recomputes which panes need the human
/// (idle-with-prompt detection; report/gate signals are event-driven and
/// picked up on the next tick).
const ATTENTION_INTERVAL: Duration = Duration::from_secs(3);
/// A pane's terminal output must be stable (unchanged) at least this long
/// before an idle-with-prompt is asserted — the CLI has stopped painting and
/// is genuinely parked on a prompt, not mid-render. Measured across ticks, so
/// it also debounces (needs a couple of consecutive quiet scans).
const ATTENTION_QUIET_MS: u64 = 4000;
/// If the human typed into a pane within this window it does not "need
/// attention" — they are already at the keyboard on it.
const ATTENTION_RECENT_INPUT_MS: u64 = 6000;
/// How long the frontend gets to open a pane and report its pty id.
const BIND_TIMEOUT: Duration = Duration::from_secs(20);
/// Gap between the bracketed paste and the Enter that submits it.
const PASTE_SUBMIT_DELAY: Duration = Duration::from_millis(500);

// Submission discipline: copilot ignores Enter while its agent is running
// (the pasted text just sits in the input box — observed live with a worker
// report landing mid-turn), so before pressing Enter the pane must be quiet
// (turn finished). Enter on an empty box is a no-op in both CLIs, so a
// couple of spaced blind retries are safe and cover late busy-locks.
/// Output must be idle this long before Enter is pressed.
const SUBMIT_QUIET: Duration = Duration::from_millis(1000);
/// Max time to wait for quiet before pressing Enter anyway.
const SUBMIT_MAX_WAIT: Duration = Duration::from_secs(45);
/// Spaced blind Enter retries after the first (no-ops once submitted).
const SUBMIT_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(2500), Duration::from_millis(4500)];

// Submit confirmation + stranded-text flush (#81/#84). A submit that landed
// clears the input box and the CLI repaints / starts its turn — a burst of
// output. An Enter that was ignored (focus-gated pre-#99, still busy, or an
// empty box) produces effectively none. We watch for that burst in a short
// window after the first Enter and record the outcome, so the NEXT delivery to
// the same pane can tell whether the previous prompt is still stranded in the
// box (and would otherwise merge with the new paste).
/// How long after the first Enter to watch for the submit's output burst.
const SUBMIT_CONFIRM_WINDOW: Duration = Duration::from_millis(600);
/// Output growth (bytes) within the window that counts as a landed submit.
/// Set well above idle cursor-blink noise so confirmation biases against false
/// positives: a false "unconfirmed" only costs a harmless no-op flush next
/// time, whereas a false "confirmed" would let stranded text merge.
const SUBMIT_CONFIRM_MIN_BYTES: u64 = 24;
/// After flushing a previous delivery's stranded text, let the CLI settle
/// (box clears, turn starts) before the new paste lands.
const FLUSH_SETTLE: Duration = Duration::from_millis(400);

// Human-typing backstop (#43, option A): even with the loomux compose strip,
// a human can still type directly into the terminal. Before the paste AND
// before the first Enter, hold delivery while the pane has seen recent
// keystrokes so a report can't land in — or submit — the human's half-typed
// line. Capped so a long compose session can't starve reports forever.
/// Treat the human as "still typing" if they hit a key within this window.
const USER_QUIET_HOLD: Duration = Duration::from_secs(4);
/// Deliver anyway once a single hold has waited this long (never starve).
const USER_QUIET_MAX_HOLD: Duration = Duration::from_secs(90);
/// Poll interval while holding for the human to go quiet.
const USER_QUIET_POLL: Duration = Duration::from_millis(250);

// Human-input paste guard (#111): the quiet backstop above only waits out
// active typing — it does NOT stop a paste landing on top of text a human
// typed and then LEFT sitting in the box (a half-written `/model`, say). Pasting
// there and pressing Enter merge-submits the human's line with the prompt (the
// live `Unknown command: /modelRun ...` collision). So before pasting, if the
// box still holds a human's unsubmitted line (tracked per keystroke as
// `input_pending`), hold for them to submit/clear it, and if it never clears,
// abort rather than blind-merge.
/// Bounded wait for the box to clear before aborting the delivery.
const HUMAN_INPUT_HOLD_MAX: Duration = Duration::from_secs(60);
/// Poll interval while holding for the box to clear.
const HUMAN_INPUT_POLL: Duration = Duration::from_millis(250);

// Kickoff readiness: a fixed boot delay loses the race on a loaded machine
// (a CLI that boots slower than the delay flushes the pasted prompt along
// with its startup stdin buffer — observed live with a reviewer spawned
// while a worker ran cargo test). Instead, watch the pane's output ring and
// paste only once the CLI has painted its UI and gone quiet.
/// Minimum wait before even checking (lets the process start writing).
const READY_MIN_WAIT: Duration = Duration::from_millis(1500);
/// Output must be idle this long (UI finished painting) to count as ready.
const READY_QUIET: Duration = Duration::from_millis(1200);
/// Minimum bytes of output before a CLI can be considered painted.
const READY_MIN_OUTPUT: usize = 512;
/// Give up waiting and paste anyway after this long.
const READY_MAX_WAIT: Duration = Duration::from_secs(25);
/// Poll interval for the readiness check.
const READY_POLL: Duration = Duration::from_millis(250);

// Copilot autopilot consent (#101/#179): a group copilot agent is launched with
// `--autopilot`, which makes copilot open an "Enable autopilot mode" dialog the
// first time a message is submitted (NOT at boot — verified live on 1.0.69: a
// fresh pane paints a normal input box). The kickoff path answers it
// deterministically (Enter on the default "Enable all permissions") right AFTER
// the first submit, which both enables autopilot and lets the just-submitted
// brief proceed. Fail-soft: if the dialog never appears, delivery proceeds.
/// How long to watch for the consent dialog after the kickoff submit before
/// giving up and letting the submit retries carry on.
const AUTOPILOT_DIALOG_WAIT: Duration = Duration::from_secs(12);
/// Poll interval while watching for the consent dialog.
const AUTOPILOT_DIALOG_POLL: Duration = Duration::from_millis(250);
/// Pause after answering so the TUI dismisses the dialog and repaints / starts
/// the turn before the delivery's confirmation window measures the burst.
const AUTOPILOT_DIALOG_SETTLE: Duration = Duration::from_millis(700);
/// Keys that confirm the highlighted menu item in Copilot's consent dialog.
/// Focus-in report (`ESC[I`) + Enter (`\r`) — the SAME transport as
/// [`submit_sequence`]`("copilot")`. The dialog is answered after the kickoff
/// submit, by which point copilot's focus flag is already true (the submit's
/// own `ESC[I` set it); the prefix is kept so this stays consistent with the
/// other pane-write sites and self-sufficient if a stray blur ever intervened
/// (#98). The `\r` selects the default-highlighted "Enable all permissions"
/// (menu `initialIndex` 0, `code==="return"`, verified against the 1.0.69 TUI)
/// — no arrow keys needed.
#[doc(hidden)] // pub for integration tests
pub const COPILOT_AUTOPILOT_CONFIRM_KEYS: &[u8] = b"\x1b[I\r";

// Echo verification: a paste that landed makes the TUI redraw its input box
// (observable as output bytes). A paste that produced no output within the
// window was flushed by a CLI whose stdin reader wasn't attached yet
// (observed live with copilot, whose input attaches well after its UI
// paints) — wait and retype.
/// How long a paste has to produce echo output before it counts as eaten.
const ECHO_WINDOW: Duration = Duration::from_millis(2000);
/// Minimum output growth that counts as the input box echoing the paste.
const ECHO_MIN_BYTES: u64 = 8;
/// Pause before retyping after an eaten paste (input attach may be close).
const ECHO_RETRY_DELAY: Duration = Duration::from_millis(1500);
/// Total attempts before typing blind and letting the human see the result.
const ECHO_ATTEMPTS: u32 = 3;
/// Upper bound for `set_state` payloads.
const MAX_STATE_BYTES: usize = 512 * 1024;

/// Cap on a single steered image attachment (#72), in decoded bytes. Sized to
/// comfortably hold a full-screen PNG screenshot while bounding the per-group
/// `attachments/` scratch dir. The steering strip enforces the same limit and
/// toasts on overflow; this is the backstop against a hostile/oversize IPC.
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Cap on the base64 payload the save-attachment command will decode. Rejecting
/// oversize *before* decode keeps a giant string from ballooning memory — same
/// discipline as the OSC 52 clipboard path. base64 is 4 bytes per 3 input, plus
/// slack for padding/whitespace.
pub const MAX_ATTACHMENT_B64_LEN: usize = MAX_ATTACHMENT_BYTES / 3 * 4 + 16;

/// Monotonic tiebreaker so two images pasted inside the same millisecond get
/// distinct filenames without pulling in a randomness/uuid crate (the Windows
/// `getrandom` backends are banned here — see the build notes).
static ATTACH_SEQ: AtomicU32 = AtomicU32::new(0);

/// Monotonic tiebreaker for grant temp-file names + grant nonces, so a grant
/// write is atomic and each nonce is unique without a randomness/uuid crate
/// (getrandom is banned — see the build notes). Combined with the pid it never
/// collides across concurrent writers.
static GRANT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Enforced-gate grants (#83) are one-time and short-lived: a human sign-off
/// authorizes exactly one privileged action within this window, then the grant is
/// consumed or expires. 30 minutes is long enough for CI to finish and the merge
/// to run, short enough that a forgotten grant can't linger as a standing opening.
const GRANT_TTL_SECS: u64 = 30 * 60;

// Copilot session tracking: unlike Claude, copilot can't be handed a session
// id up front — it mints one and writes `~/.copilot/session-state/<id>/` a
// few seconds into boot. After spawning a copilot pane we poll for the new
// session directory and bind its id to the pane's roster record.
/// How often to poll `session-state` for the pane's new session.
const COPILOT_SESSION_POLL: Duration = Duration::from_millis(1000);
/// Give up watching after this long (copilot never initialized, or crashed).
const COPILOT_SESSION_TIMEOUT: Duration = Duration::from_secs(90);

/// An agent's **capability class** — the closed enum (#222).
///
/// Before the block model this enum *was* an agent's identity: it decided the
/// persona, the template, the model, the CLI and the capabilities all at once.
/// Now identity is a [`workflow::BlockId`] and this enum carries only the part
/// that must stay closed: **what an agent is structurally allowed to do**.
///
/// That closure is the security spine of #222. Personas are unbounded data
/// authored in a repo file; capabilities are not. A workflow file *selects* a
/// class here — it can never define one, and there is no `read_only: false`
/// escape hatch. So a repo can declare five reviewers with five prompts and five
/// models, and it cannot make one of them anything but a reviewer: the deny-flags
/// (`build_agent_command`), the cwd rule (`spawn_agent_ex`) and the MCP tool
/// scope (`mcp::tool_defs`) all key off this enum, and it has exactly four
/// values.
///
/// What each class *is* varies, and the enum should not be read as promising more
/// than it enforces: a planner is structurally read-only ([`Role::is_read_only`] —
/// real CLI-level denials), while a reviewer's "never pushes" is instruction-
/// backed, as it was before #222. The guarantee is over which posture a block
/// gets, not that every posture is a sandbox.
///
/// The name `Role` survives because ~72 call sites and the persisted wire
/// format use it; read it as "capability class".
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Orchestrator,
    Worker,
    Reviewer,
    /// Read-only explorer: investigates the codebase and writes a structured
    /// implementation plan (as a GitHub issue comment), then reports and
    /// exits. A planner NEVER writes code, branches, or PRs. It counts as a
    /// delegate against the live-agent cap, like a worker/reviewer.
    Planner,
}

impl Role {
    /// Agent-id prefix (`w-3`). Reached through [`workflow::Block::prefix`] at
    /// the spawn sites — a block's prefix is derived from its class so ids stay
    /// short and the roster/badge conventions that parse them keep working.
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Role::Orchestrator => "orch",
            Role::Worker => "w",
            Role::Reviewer => "rev",
            Role::Planner => "plan",
        }
    }
    /// The built-in role contract template. A block's persona *layers on* this
    /// (append) or replaces its body (replace) — but never its
    /// [`mechanics_core`].
    pub(crate) fn template(self) -> &'static str {
        match self {
            Role::Orchestrator => ORCHESTRATOR_TPL,
            Role::Worker => WORKER_TPL,
            Role::Reviewer => REVIEWER_TPL,
            Role::Planner => PLANNER_TPL,
        }
    }
    pub(crate) fn instructions_file(self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator.md",
            Role::Worker => "worker.md",
            Role::Reviewer => "reviewer.md",
            Role::Planner => "planner.md",
        }
    }
    /// Lowercase wire/label name (matches the `Serialize` rename).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator",
            Role::Worker => "worker",
            Role::Reviewer => "reviewer",
            Role::Planner => "planner",
        }
    }
    /// The capability that used to be spelled `role == Role::Planner` inline at
    /// the spawn site. Now it is a *property of the class*, which is what makes
    /// "a workflow file can never grant a capability" checkable: there is no
    /// other way to become read-only, and no way to stop being it.
    pub fn is_read_only(self) -> bool {
        matches!(self, Role::Planner)
    }
}

/// The **non-overridable loomux mechanics core** for a capability class
/// (harvested from PR #105, issue #51).
///
/// A persona in `mode: replace` swaps the role's *personality/policy* body — it
/// must NOT be able to strip the functional contract that makes the app work
/// (the loomux MCP tools, the task board, `report()` discipline, the
/// spawn/review/plan flow, the branch→PR git discipline). loomux always injects
/// this core, so a replace persona stays functional no matter what its author
/// left out. In `append` mode the full built-in template already carries these
/// mechanics, so the core is only *written* when a replace persona has dropped
/// the built-in body.
///
/// This is the extracted, always-on subset of the built-in templates; splitting
/// every template into `mechanics + body` files is follow-up work.
pub(crate) fn mechanics_core(kind: Role) -> String {
    // Shared spine for every delegate; the orchestrator gets its own.
    let common = "\
These loomux mechanics are guaranteed by the app and are NOT optional, whatever your \
persona says:\n\
- You act through the loomux MCP tools. `report(status, summary)` (status: progress | \
done | blocked) is your channel to the orchestrator — report `progress` on start, \
`blocked` when stuck (say what you need), and `done` with the PR URL. \
`message_orchestrator(text)` is for questions; `list_agents()` / `get_state()` are \
read-only context. These tools never need approval; use them, don't ask the human to.\n\
- Git discipline: work only in your assigned workspace; create your branch off the \
default branch before changing anything; never commit to the default branch; open a PR \
with `gh` linking the issue. NEVER merge — the human gates merges.\n\
- One task per session. Follow-ups and review fixes for your own task are yours; a \
different task means asking for a fresh agent.";
    match kind {
        Role::Orchestrator => "\
These loomux mechanics are guaranteed by the app and are NOT optional, whatever your \
persona says:\n\
- You drive the group through the loomux MCP tools: `spawn_agent` (worker | reviewer | \
planner, optionally naming a workflow `block`), `send_prompt`, `get_output`, \
`kill_agent`, `focus_agent`, `rename_agent`; the shared task board via `list_tasks` / \
`upsert_task` / `remove_task`; and durable state via `get_state` / `set_state`. \
Guardrails (live-agent cap, per-block CLI + model) are enforced by loomux.\n\
- Maintain the task board: it is the human's view of the work. Record each agent's \
`session` id on its task so finished work can be resumed for follow-ups instead of \
cold-started. Never disturb a busy worker with a new task.\n\
- Drive the flow: plan → spawn workers/reviewers/planners → branch → PR → review → human \
merge gate. You never merge; you surface work at the gate for the human.\n\
- Use `report`/`message_orchestrator` semantics from your delegates as their status \
channel; keep the human oriented with short summaries."
            .to_string(),
        // Red-before-green rides in the core for the same reason the reviewer's duties do
        // (#236): a `mode: replace` worker persona never reads `worker.md`, and "the tests
        // would catch it" is precisely the claim that is worthless unevidenced — the
        // orchestrator is told to treat a `done` without the evidence as not done, so every
        // worker has to have been told to produce it, however its persona was written.
        Role::Worker => format!(
            "{common}\n- Deliverable: a branch → commit → PR with the project's tests green. \
             Add tests that would fail if the feature regressed — and SHOW that they do: run \
             them against the base branch (without your change) first, confirm they fail for \
             the expected reason, and put the command and its failure line in the PR \
             description. A test nobody has seen fail is not evidence of anything. The exemption \
             (it rides here too, or a replace-persona worker could never legally ship a docs PR or \
             a revert): a change with NO new testable behavior — docs/prose-only, a revert, a pure \
             rename/move the suite already pins, a re-blessed golden fixture — owes instead ONE \
             LINE in the PR naming which of those it is and why, with the existing suite green. An \
             unstated absence of evidence is not done; anything else evidences the normal way."
        ),
        // The verdict tool belongs in the CORE, not only in `reviewer.md` (#222/#197):
        // a merge gate names *custom* reviewer blocks, and a custom block with a
        // `mode: replace` persona never sees the built-in reviewer template — this
        // core is its whole loomux contract. A reviewer that didn't know to record a
        // verdict would hold the gate shut forever and nobody would know why.
        //
        // The findings-classification duty rides here for the same reason (#222): a
        // replace persona never reads `reviewer.md`, and a `pass` whose summary hides
        // the findings it left behind is how the gate opens on a change that still
        // contradicts its own rationale. Keep the two in lockstep.
        //
        // The GitHub-facing half rides here too (#239, carried forward from #238's rev-23
        // F1). The recorded verdict below is the GATE's record — it exists only for a group
        // whose workflow declares a gate. The reviewer's other record is the review it POSTS,
        // and there `--request-changes`/`--approve` are both refused by GitHub on a PR opened
        // by your own account (the normal case: one group, one GitHub user, who authors the
        // PRs — every review this repo has received is COMMENTED). A reviewer told only to use
        // a flag it cannot use improvises, and the only other action it was ever shown is
        // `--approve`. So the fallback is NAMED, the bind is on the verdict it STATES, and the
        // refusal may not decay into an approval, a softened verdict, or a `pass`.
        //
        // So do the review LANES (#236). A persona is free to narrow a reviewer to one
        // lane — that is the whole point of a focused roster — but the lanes below are the
        // BASELINE a repo's reviewers must cover between them: a security/dependency/cost
        // defect that no block was told to look for is one that no verdict will ever
        // reflect, and the gate cannot tell the difference between "reviewed and clean" and
        // "never looked at". `reviewer.md` carries the same list; keep them in lockstep.
        Role::Reviewer => format!(
            "{common}\n- You review PRs via `gh` (checking out the PR branch locally is fine); \
             you do NOT create branches or push. Report findings via `report`/`message_orchestrator`.\n\
             - Review lanes, in priority order: **correctness** (a real defect with a concrete \
             failure scenario, verified against the code); **security** — the trust boundaries \
             the change crosses: which inputs are attacker- or agent-controllable (a repo file, \
             a PR title, an MCP argument, anything off the network) and where they land (a path \
             segment, a shell line, rendered HTML, a privileged command); **test quality** — do \
             the tests test intent, or are they tautologies that cannot fail, and is the \
             red-before-green evidence (the new tests failing on the base branch) actually there \
             and actually real (neutralize the change and watch a key test go red — a present \
             claim is still only a claim); **requirement fit** against the issue; **dependency \
             hygiene** — a new dependency is permanent and the whole repo carries it, so it must \
             be argued in the PR and must clear the rules the repo's contributor docs state \
             (a popular package can violate a platform constraint fatally); **algorithmic cost** \
             at the sizes the code will really see (name the input size that hurts); **docs**. \
             If your persona narrows you to one lane, stay in it and say so — but a lane nobody \
             was assigned is a lane nobody reviewed.\n\
             - Label every finding `blocking` or `non-blocking` — the orchestrator dispositions each \
             one before the PR merges and cannot do that from unlabelled prose. A finding that \
             contradicts the change's OWN stated rationale (the guard the issue asked for is \
             bypassable; the error the PR promised to raise never fires) is not a nit, however small \
             the fix: say that the change does not do what it claims. A blocking FINDING means your \
             VERDICT is `fail` (or `escalate`) — never `pass`. The two words are different things: a \
             finding's label is your severity rating, a verdict is what the gate reads, and a `pass` \
             carrying a blocking finding is a contradiction the gate cannot see. It opens, on a \
             change you just said was wrong.\n\
             - Post the review on the PR itself (`gh pr review <n> --request-changes` / `--approve`), \
             and state the verdict in the body. GITHUB REFUSES BOTH FLAGS on a PR opened by your own \
             account — the normal case, since the whole group usually authenticates as one GitHub \
             user. When it does, post with `--comment` and LEAD THE BODY WITH THE VERDICT in those \
             words (\"Verdict: changes requested\" / \"Verdict: approve\"). The flag is only the \
             mechanism: the binding record is the verdict you STATE in the review body and repeat in \
             your `report(...)` — that is what the orchestrator merges on, and an ungated group has \
             no other record. A `--request-changes` that GitHub refused is NEVER a reason to \
             `--approve`, to soften the verdict, or to record a `pass`: the mechanism was \
             unavailable, the finding was not.\n\
             - Record your review outcome with `review_verdict(pr, verdict, summary)` — verdict: \
             pass | fail | escalate. It is durable, attributed STATE (not a notification): when the \
             repo's workflow declares a merge gate, loomux refuses `gh pr merge` until every reviewer \
             it names has recorded a `pass`. `fail` and `escalate` each refuse the merge, and one \
             blocking verdict beats any number of passes — so never record `pass` to be agreeable or \
             to unblock a queue, and record nothing until you have actually finished reviewing. Your \
             verdict is bound to the commit you reviewed: if the author pushes more commits your pass \
             goes stale and the gate reopens until you review the new head and record again.\n\
             - A `pass` recorded with findings still open (they can only be non-blocking ones — see \
             above) must SAY so in its summary (\"pass — 2 non-blocking findings, disposition \
             pending\"). The verdict is the gate's state, and the gate is read by something that will \
             merge on it: a summary that reads like a clean bill of health is how review feedback gets \
             dropped at the merge."
        ),
        Role::Planner => format!(
            "{common}\n- You explore the codebase READ-ONLY and write an implementation plan as a \
             GitHub issue comment, then `report` and exit. You never write code, branches, \
             worktrees, or PRs (loomux also denies those at the CLI level)."
        ),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Starting,
    Running,
    Dead,
}

/// Who last set an agent's display name — the precedence ladder for the pane
/// title / roster name (#95r). A rename applies only when its source ranks at
/// least as high as whoever set the current name: `Human` > `Orchestrator` >
/// `Default`. So the human's manual rename is never clobbered by the
/// orchestrator's `rename_agent` or the id-derived default, while the
/// orchestrator can still relabel an id-default (or its own earlier name).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NameSource {
    /// Minted from the agent id at spawn ("worker 2" for `w-2`).
    Default,
    /// Chosen by the orchestrator (a `spawn_agent` name, or `rename_agent`).
    Orchestrator,
    /// Typed by the human into the pane title (F2 / double-click).
    Human,
}

impl Default for NameSource {
    /// Legacy roster rows (written before the tier was persisted, #95r) carry a
    /// name but no source. Treat them as orchestrator-chosen: their non-empty
    /// name was picked deliberately, so a later `rename_agent` may still relabel
    /// it, and it never sits *below* an id-default. (Pre-95r human renames were
    /// frontend-only and never reached the roster, so none are being demoted.)
    fn default() -> Self {
        NameSource::Orchestrator
    }
}

impl NameSource {
    fn rank(self) -> u8 {
        match self {
            NameSource::Default => 0,
            NameSource::Orchestrator => 1,
            NameSource::Human => 2,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            NameSource::Default => "default",
            NameSource::Orchestrator => "orchestrator",
            NameSource::Human => "human",
        }
    }
}

/// Which agent CLI a group runs. Each needs an adapter in
/// `build_agent_command` + `write_mcp_config`; anything unknown falls back
/// to Claude (explicitly, in `clamped`, never silently at spawn time).
pub const SUPPORTED_CLIS: [&str; 2] = ["claude", "copilot"];

/// Which kind of start a `create_group` call is (#222).
///
/// It exists for exactly one decision — **does the repo's `.loomux/workflow.yml`
/// get read?** — and the answer is "on a fresh launch, yes; on a resume, no".
///
/// The reason is consent, not caching. The roster the advanced orchestrator runs
/// is repo-authored, and the moment the human agrees to it is the launcher preview
/// they saw before hitting Create. A resume is not that moment: nobody is being
/// shown anything. So a `git pull` (or checking out a contributor's branch) between
/// launch and resume must not be able to hand a resumed group a reviewer, or a
/// persona, that its human never approved. The roster that comes back is the one in
/// `group.json` — the one they approved. Drift against the file on disk is audited
/// (`workflow-changed-since-launch`), never applied.
///
/// Note this is *not* the same question as "does `group.json` already exist" — a
/// human relaunching a group on a repo they have orchestrated before is a fresh
/// launch, preview and all, and must pick up a workflow file they have just edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    /// The human is at the launcher and has seen the roster preview.
    Fresh,
    /// A recorded orchestrator session is being reopened (the session browser).
    Resume,
}

#[derive(Clone, Debug, Default)]
pub struct Guardrails {
    pub max_agents: u32,
    /// Group-default agent CLI ("claude" | "copilot", see `SUPPORTED_CLIS`).
    /// A block's own `cli` overrides it; an empty block `cli` inherits this.
    /// Kept as the group default so old group.json (pre per-role CLI) and the
    /// launcher's single-CLI path both keep working (issue #4).
    pub agent_cli: String,
    /// **The agent roster, as data (#222).** This replaced the eight flat
    /// per-role fields (`worker_cli`, `reviewer_model`, …): a group's agents are
    /// now a list of [`workflow::Block`]s, each with its own id, capability
    /// class (`kind`), CLI, model and persona. Read from
    /// `<repo>/.loomux/workflow.yml` when the repo declares one; otherwise
    /// [`workflow::default_roster`] synthesizes today's fixed 4-block roster
    /// from the launcher's per-role picks, so a repo with no workflow file
    /// behaves exactly as it did before blocks existed.
    ///
    /// Empty is legal only transiently: `clamped()` fills it with the built-in
    /// roster, so no code downstream has to handle an agent-less group.
    pub blocks: Vec<workflow::Block>,
    /// **The advanced-orchestrator toggle (#222).** Off — the default, and what
    /// every group.json written before this field existed means — is today's
    /// experience byte for byte: `<repo>/.loomux/workflow.yml` is not read, not
    /// validated and not obeyed, and `blocks` stays the roster the launcher's
    /// per-role picks synthesized. On, the repo's workflow file is loaded in
    /// `create_group` and *its* blocks become the roster.
    ///
    /// The toggle exists because a workflow file is repo-authored input that
    /// arrives with a `git clone`. Letting one take effect merely by being
    /// present would mean cloning a repo could change which agents a human's
    /// group runs, with which personas, before they had ever seen the file — so
    /// the human opts in per launch, having been shown the roster it resolves to.
    ///
    /// A *launch* choice, not a live one: it is persisted with the group so a
    /// resumed orchestration comes back with the roster it was launched with.
    pub advanced_orchestrator: bool,
    /// Additionally pre-approve `git`/`gh` shell commands for the group's
    /// agents. Never maps to `--dangerously-skip-permissions`: bypass mode
    /// shows a confirm dialog whose default answer is "exit", which the
    /// kickoff typing would accept, killing the pane.
    pub auto_ops: bool,
    /// Cost guardrail: auto-kill a worker/reviewer that has sat without a
    /// task for this many minutes (the orchestrator is notified so it can
    /// respawn on demand). 0 disables it. See `idle_should_kill`.
    pub idle_kill_minutes: u32,
    /// Cost guardrail: cap on worker/reviewer spawns per rolling hour, a
    /// runaway-orchestrator backstop. 0 = unlimited. See `spawn_rate_exceeded`.
    pub max_spawns_per_hour: u32,
    /// Recovery guardrail: nudge the orchestrator once when a working agent
    /// produces no terminal output and sends no report for this many minutes
    /// (likely stalled or waiting on input). 0 disables it. See
    /// `watchdog_should_notify`.
    pub watchdog_stall_minutes: u32,
    /// Autonomous mode cost cap (#83): the token budget an autonomous group may
    /// spend *after* autonomous mode is enabled before idle ticking is suspended
    /// and the human is notified. Metered as the delta from the usage snapshot
    /// captured at enable time (the `autonomous` marker's content) — see
    /// `enforce_autonomy_budgets`. Tokens, not dollars: subscription/Max accounts
    /// pay $0 marginal, so tokens are the honest metric (see `usage.rs`). 0 =
    /// no cap. Persisted in group.json, live-settable via `set_autonomy_budget`.
    pub autonomy_budget_tokens: u64,
    /// Autonomous mode idle-tick quiet window in minutes (#83): how long the
    /// orchestrator pane must be output-quiet before an idle tick fires. 0 = unset
    /// → `DEFAULT_IDLE_TICK_MINUTES`; clamped to `1..=MAX_IDLE_TICK_MINUTES` (the
    /// `autonomous` marker, not this, is the on/off switch). Persisted in
    /// group.json, live-settable via `set_idle_tick_minutes` so the human can drop
    /// it to 1–2 min to verify quickly. See `idle_tick_should_fire`.
    pub idle_tick_minutes: u32,
    /// Autonomous mode idle-tick activity floor in bytes (#83): per-tick pty-output
    /// growth at/above which the orchestrator counts as working (resets the quiet
    /// clock); sub-floor growth is idle repaint noise. 0 = unset →
    /// `DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES`; clamped to
    /// `1..=MAX_IDLE_ACTIVITY_FLOOR_BYTES`. Live-settable via
    /// `set_idle_activity_floor` so a chattier CLI whose idle repaints exceed the
    /// default has a runtime remedy (rev-59). See `idle_output_is_activity`.
    pub idle_activity_floor_bytes: u64,
}

impl Guardrails {
    #[doc(hidden)] // pub for integration tests (unit tests can't load the UI stack; see tests/smoke.rs)
    pub fn clamped(mut self) -> Self {
        self.max_agents = self.max_agents.clamp(1, MAX_AGENTS_CEILING);
        // The group default CLI is coerced to a supported value (legacy /
        // single-CLI path). Per-role CLIs are validated at spawn instead of
        // coerced here, so a genuinely unknown per-role type is rejected
        // rather than silently downgraded (issue #4).
        if !SUPPORTED_CLIS.contains(&self.agent_cli.as_str()) {
            self.agent_cli = "claude".into();
        }
        // An empty roster means "nobody said otherwise" — the launcher's plain
        // path, a legacy group.json, a `Guardrails::default()`. Fill it with the
        // built-in 4-block roster so every downstream lookup finds a block.
        if self.blocks.is_empty() {
            self.blocks = workflow::builtin_roster(&self.agent_cli);
        }
        // ── roster normalization, in order; each step depends on the last ──
        //
        // Steps 1-3 are defensive: `parse_workflow` already enforces all of them
        // and *tells the author which line is wrong*. They are re-enforced here
        // (silently — there is no author present) because a roster can also arrive
        // from a hand-edited group.json, which never meets the parser.

        // 1. Ids are shell tokens and file names. An unusable one would mint an
        //    agent id like `w-` and write `.md`. Fall back to the class name
        //    rather than dropping the block — a roster with a hole is worse than
        //    one with a plainly-named block.
        for b in &mut self.blocks {
            b.id = workflow::sanitize_id(&b.id).unwrap_or_else(|| b.kind.as_str().to_string());
        }
        // 2. The four class names are RESERVED as ids for their own class. An
        //    `id: planner, kind: reviewer` block would write its contract to
        //    `reviewer.md` — the real reviewer's file (see
        //    `workflow::Block::instructions_file`) — and clobber it.
        self.blocks
            .retain(|b| workflow::kind_from_str(&b.id).is_none_or(|reserved| reserved == b.kind));
        // 3. Ids are unique. A duplicate makes `block(id)` resolve to whichever
        //    came first and leaves the other permanently unreachable.
        let mut seen: HashSet<String> = HashSet::new();
        self.blocks.retain(|b| seen.insert(b.id.clone()));
        // 4. Every group has exactly one orchestrator, and it is structural — it
        //    is the pane the human talks to. A workflow file that declares only
        //    the agents it cares about (three reviewers, a worker) must not leave
        //    the group without one. This is the only block loomux adds on the
        //    repo's behalf, and it grants nothing the file didn't already have:
        //    a group with no orchestrator cannot run at all.
        //
        //    Step 2 is what makes this safe to prepend — the id `orchestrator` can
        //    only belong to an orchestrator-kind block, so "no orchestrator kind"
        //    implies "no `orchestrator` id", and this cannot mint a duplicate.
        if !self.blocks.iter().any(|b| b.kind == Role::Orchestrator) {
            let mut roster = workflow::default_roster(&[(Role::Orchestrator, &self.agent_cli, "")]);
            roster.append(&mut self.blocks);
            self.blocks = roster;
        }
        for b in &mut self.blocks {
            b.name = workflow::sanitize_display(&b.name);
            if b.name.is_empty() {
                b.name = b.id.clone();
            }
            // A block CLI is validated at spawn rather than coerced here, so a
            // genuinely unknown one is rejected loudly instead of silently
            // downgraded (issue #4). Only the *effective* model is normalized:
            // Copilot picks its own best model with "auto"; Claude needs a tier.
            let cli = if b.cli.trim().is_empty() { self.agent_cli.clone() } else { b.cli.clone() };
            b.model = sanitize_model(&b.model, default_model(&cli, b.kind));
        }
        self.idle_kill_minutes = self.idle_kill_minutes.min(MAX_IDLE_KILL_MINUTES);
        self.max_spawns_per_hour = self.max_spawns_per_hour.min(MAX_SPAWNS_PER_HOUR);
        self.watchdog_stall_minutes = self.watchdog_stall_minutes.min(MAX_WATCHDOG_STALL_MINUTES);
        // 0 = unset → default (not "off"); then floor at 1 so ticking never
        // silently stops while autonomous — the marker is the on/off switch.
        if self.idle_tick_minutes == 0 {
            self.idle_tick_minutes = DEFAULT_IDLE_TICK_MINUTES;
        }
        self.idle_tick_minutes = self.idle_tick_minutes.clamp(1, MAX_IDLE_TICK_MINUTES);
        // 0 = unset → default; floored at 1 (any growth = activity) so it can never
        // be a no-op that treats real bursts as noise.
        if self.idle_activity_floor_bytes == 0 {
            self.idle_activity_floor_bytes = DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES;
        }
        self.idle_activity_floor_bytes =
            self.idle_activity_floor_bytes.clamp(1, MAX_IDLE_ACTIVITY_FLOOR_BYTES);
        self
    }

    /// A block by id. The block *is* the agent's identity (#222) — edges,
    /// gates, `spawn_agent(block:)` and the roster all reference this.
    pub fn block(&self, id: &str) -> Option<&workflow::Block> {
        self.blocks.iter().find(|b| b.id == id)
    }

    /// The **default block for a capability class** — the first block of that
    /// kind in roster order.
    ///
    /// This is the bridge that kept the ~72 `Role::` sites compiling: code that
    /// used to ask "what CLI does the reviewer run?" now asks "what CLI does the
    /// *default reviewer block* run?". With the built-in roster there is exactly
    /// one block per class, so the answer is unchanged. With a custom workflow
    /// declaring three reviewers, this is the one an orchestrator gets when it
    /// spawns `kind: reviewer` without naming a block — the others are opt-in by
    /// id, which is deliberate: a roster must not silently change what a plain
    /// `spawn_agent(kind: reviewer)` does.
    pub fn block_for(&self, kind: Role) -> Option<&workflow::Block> {
        self.blocks.iter().find(|b| b.kind == kind)
    }

    /// The agent CLI a capability class's default block runs: the block's own
    /// `cli`, else the group default `agent_cli`. May return an unsupported
    /// value (a block CLI is not coerced in `clamped`); the spawn paths validate
    /// it.
    pub fn cli_for(&self, role: Role) -> &str {
        match self.block_for(role) {
            Some(b) => workflow::cli_of(b, &self.agent_cli),
            None => &self.agent_cli,
        }
    }

    /// The model the class's default block runs (already normalized by
    /// `clamped`, so never empty for a roster that went through it).
    pub fn model_for(&self, role: Role) -> &str {
        match self.block_for(role) {
            Some(b) => workflow::model_of(b, &self.agent_cli),
            None => default_model(&self.agent_cli, role),
        }
    }
}

/// Default model for a capability class on a given CLI. Copilot picks its own
/// best model ("auto"); on Claude the reasoning-heavy classes (orchestrator,
/// planner) get the strong tier and the executing ones (worker, reviewer) the
/// mid tier.
pub(crate) fn default_model(cli: &str, role: Role) -> &'static str {
    if cli == "copilot" {
        return "auto";
    }
    match role {
        Role::Orchestrator | Role::Planner => "opus",
        Role::Worker | Role::Reviewer => "sonnet",
    }
}

// ── Per-CLI unattended ("autopilot / allow all") permission flags ───────────
//
// The single source of truth for what "unattended" means on each agent CLI.
// Both the orchestration spawn path (`build_agent_command`) and the single-pane
// launcher (`single_pane_autopilot_flags`, exposed as the `agent_autopilot_flags`
// Tauri command) build from these atoms, so the two paths can't drift (#101).

/// Copilot's *single-pane* unattended flags: pre-approve all tools and all
/// paths so the agent runs without per-tool / path-verification confirmation.
///
/// Deliberately NOT `--autopilot` here. That flag boots Copilot into its
/// *autopilot mode*, which opens a blocking interactive "Enable autopilot mode"
/// dialog on startup (Enable all permissions / Continue with limited / Cancel —
/// verified in the CLI bundle's `showAutopilotConfirmation` path). A single-pane
/// agent has a **human at the keyboard**, so interactive framing is correct and
/// no one wants a startup dialog they didn't ask for; `--allow-all-tools` is
/// Copilot's documented non-interactive enabler and carries no dialog.
///
/// The **group** spawn path uses [`COPILOT_GROUP_AUTOPILOT_FLAGS`] instead —
/// see there for why managed workers DO want true autopilot mode.
pub const COPILOT_UNATTENDED_FLAGS: &str = "--allow-all-tools --allow-all-paths";

/// Copilot's *group-spawn* unattended flags: [`COPILOT_UNATTENDED_FLAGS`] plus
/// `--autopilot`, which puts a loomux-managed worker/planner into true autopilot
/// mode. Autopilot mode is not just the idle auto-continue loop — it injects an
/// autonomy directive into the model's system prompt ("persist autonomously …
/// continue executing without waiting for user input … the user may not even be
/// present"), which is exactly right for an unattended, loomux-driven agent.
///
/// `--autopilot` triggers the "Enable autopilot mode" consent dialog at startup.
/// That is SAFE on the group path (and only there) because the kickoff delivery
/// deterministically answers it — [`copilot_autopilot_prompt_detected`] +
/// `deliver_prompt`'s confirm step press Enter on the default "Enable all
/// permissions" BEFORE the brief is pasted, so the brief can never collide with
/// the dialog. The human's loomux-level "auto-ops" choice IS the consent.
///
/// Kept as a derived-but-pinned constant (a `..._reuses_single_pane_atom` test
/// asserts it equals `--autopilot ` + [`COPILOT_UNATTENDED_FLAGS`]) so the two
/// posture strings can't drift apart.
pub const COPILOT_GROUP_AUTOPILOT_FLAGS: &str = "--autopilot --allow-all-tools --allow-all-paths";

/// git + gh pre-approval appended to Claude's `--allowedTools` for an unattended
/// agent, so the branch→commit→PR flow runs without prompts. `Bash(git *)`
/// matches every git subcommand; a planner's denials carve commit/push back out.
pub const CLAUDE_UNATTENDED_ALLOW: &str = "\"Bash(git *)\" \"Bash(gh *)\"";

/// Claude Code's permission mode for an (un)attended agent: its native `auto`
/// preset when unattended (what a human uses interactively), else `acceptEdits`.
pub fn claude_permission_mode(unattended: bool) -> &'static str {
    if unattended {
        "auto"
    } else {
        "acceptEdits"
    }
}

/// Per-CLI flags that put a *standalone* single-pane agent into the same
/// unattended "autopilot / allow all" posture group workers get — minus the
/// MCP/session/workspace wiring only a managed agent needs. Built from the SAME
/// atoms as `build_agent_command` (#101) so the launcher and the orchestration
/// path can't drift.
///
/// Returns an empty string for CLIs with no known unattended flag surface
/// (codex/opencode/gemini/custom): the toggle is a no-op there rather than
/// inventing flags that may not exist.
pub fn single_pane_autopilot_flags(program: &str) -> String {
    match program.trim().to_lowercase().as_str() {
        "claude" => format!(
            "--permission-mode {} --allowedTools {CLAUDE_UNATTENDED_ALLOW}",
            claude_permission_mode(true)
        ),
        "copilot" => COPILOT_UNATTENDED_FLAGS.to_string(),
        _ => String::new(),
    }
}

/// Whether an idle agent has sat long enough to auto-kill. Pure so the
/// threshold logic is testable without threads or wall-clock; the reaper
/// loop lives in `start_idle_reaper`. `idle_since_ms` is `None` for an agent
/// that currently has work (never idle-killed); a `threshold_min` of 0
/// disables the guardrail entirely.
pub fn idle_should_kill(idle_since_ms: Option<u64>, now_ms: u64, threshold_min: u32) -> bool {
    match (threshold_min, idle_since_ms) {
        (0, _) | (_, None) => false,
        (m, Some(t)) => now_ms.saturating_sub(t) >= (m as u64) * 60_000,
    }
}

/// Format the live-delegate roster line for the cap-rejection guardrail message
/// (#203) from `(id, role, idle)` triples, sorted by id for a stable message:
/// `id (role, idle|working), …`. `idle` (`idle_since_ms.is_some()`) is the
/// same signal the idle-reaper kills on, so it genuinely means "safe to
/// reclaim". Pure and free-standing so both `spawn_agent` cap checks can format
/// an identical message — the fast path via [`OrchRegistry::live_delegate_roster`],
/// the race-safe path directly against its already-held `agents` guard (no
/// re-lock). Empty string for no rows (the cap can't be hit then, but stay total).
fn format_delegate_roster(mut rows: Vec<(String, &'static str, bool)>) -> String {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows.into_iter()
        .map(|(id, role, idle)| format!("{id} ({role}, {})", if idle { "idle" } else { "working" }))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether a queued `orch-spawn-request` has expired and must be dropped
/// unserviced (#106). The backend stamps each request with the wall-clock
/// deadline of its own `bind` wait (`now + BIND_TIMEOUT`); a frontend that was
/// stalled past that point would otherwise open a zombie pane against
/// already-torn-down backend state (the config is cleaned and the pending bind
/// is gone). A `deadline_ms` of 0 means "unstamped" (legacy payloads) and never
/// expires. Pure and `pub` so both the stamping backend and the frontend can be
/// tested against one agreed rule, and so this rule lives in exactly one place.
pub fn spawn_request_expired(deadline_ms: u64, now_ms: u64) -> bool {
    deadline_ms != 0 && now_ms > deadline_ms
}

/// Whether the spawn-rate guardrail should reject the next spawn: true when
/// at least `limit` spawns already fall inside the trailing `window_ms`.
/// Pure so the sliding-window arithmetic is testable; `limit` 0 = unlimited.
pub fn spawn_rate_exceeded(times: &[u64], now: u64, limit: u32, window_ms: u64) -> bool {
    if limit == 0 {
        return false;
    }
    let recent = times.iter().filter(|&&t| now.saturating_sub(t) < window_ms).count();
    recent as u32 >= limit
}

/// Whether a working agent has been silent (no terminal output, no report)
/// long enough to warrant one watchdog nudge to the orchestrator. Pure so the
/// stall arithmetic and the anti-nag rule are testable without threads or a
/// real pty; the scan loop lives in `start_watchdog`. `threshold_min` 0
/// disables the guardrail; `already_notified` enforces at-most-one-notice per
/// stall (the caller clears it when the agent produces output/reports again).
pub fn watchdog_should_notify(
    silent_since_ms: u64,
    now_ms: u64,
    threshold_min: u32,
    already_notified: bool,
) -> bool {
    if threshold_min == 0 || already_notified {
        return false;
    }
    now_ms.saturating_sub(silent_since_ms) >= (threshold_min as u64) * 60_000
}

/// Autonomous mode (#83): whether an idle tick should fire for an orchestrator
/// that has been output-quiet since `quiet_since_ms`. Pure so the threshold /
/// latch / per-hour-cap / clock-skew rules are testable without threads or a real
/// pty; the scan loop lives in `idle_tick_tick` / `start_idle_tick`.
///
/// - `threshold_min` 0 disables the tick entirely.
/// - `already_notified` is the one-notice-per-idle-window latch (mirrors
///   `watchdog_should_notify`): once a tick fires, no re-fire until the
///   orchestrator produces output (it acted), which clears the latch and resets
///   the quiet clock. This is the primary self-regulation.
/// - `tick_times` + `per_hour_cap` are the hard runaway backstop, reusing the
///   same sliding-window rule as the spawn-rate guardrail (`spawn_rate_exceeded`);
///   `per_hour_cap` 0 = uncapped.
/// - `saturating_sub` tolerates a `now` before `quiet_since_ms` (clock skew /
///   a freshly-stamped clock) as "no elapsed silence", never a giant interval.
pub fn idle_tick_should_fire(
    quiet_since_ms: u64,
    now_ms: u64,
    threshold_min: u32,
    already_notified: bool,
    tick_times: &[u64],
    per_hour_cap: u32,
) -> bool {
    if threshold_min == 0 || already_notified {
        return false;
    }
    if now_ms.saturating_sub(quiet_since_ms) < (threshold_min as u64) * 60_000 {
        return false;
    }
    // Under the per-hour backstop → fire. Reuses the spawn-rate window rule so the
    // "N events per rolling hour" arithmetic lives in exactly one place.
    !spawn_rate_exceeded(tick_times, now_ms, per_hour_cap, SPAWN_RATE_WINDOW_MS)
}

/// Autonomous mode (#83): whether pty-output growth between two idle-tick
/// observations counts as the orchestrator *actively working* (so it resets the
/// quiet clock and the one-notice latch) rather than idle **repaint noise**.
/// Pure so the burst-floor rule is fixture-testable.
///
/// `output_total` counts every byte the pane emits, including statusline/spinner
/// repaints that keep creeping while the CLI is parked at its prompt — and there
/// is no output-frame classifier to strip them (the #112 work classifies human
/// *input*, not output). Treating *any* growth as activity (as the watchdog does)
/// means a single stray repaint byte resets the whole quiet window, so an
/// orchestrator that repaints even occasionally could never accumulate a full
/// window and would never tick. So we discriminate by size: a real turn dumps
/// `floor`+ bytes at once, an idle repaint far fewer. Growth `>= floor` is
/// activity; sub-floor growth is noise and leaves the quiet clock running.
pub fn idle_output_is_activity(prev_total: u64, cur_total: u64, floor: u64) -> bool {
    cur_total.saturating_sub(prev_total) >= floor
}

/// Autonomous mode (#83): whether autonomous-era spend has crossed the group's
/// token budget and idle ticking must be suspended. Pure so the metering rule is
/// unit-testable. `spend_since_enable` is the delta from the enable-time usage
/// anchor (autonomous mode meters spend *after* it was turned on, not lifetime
/// history — see `enforce_autonomy_budgets`); `budget_tokens` 0 = no cap.
pub fn autonomy_budget_exhausted(spend_since_enable: u64, budget_tokens: u64) -> bool {
    budget_tokens != 0 && spend_since_enable >= budget_tokens
}

// ---------- enforced merge gate (#83): gh-shim decision logic ----------

/// What the `gh` shim should do with an intercepted invocation. The shim mirrors
/// this in shell; the logic is pinned here as a pure, unit-tested Rust function so
/// the security-critical decision has one authoritative specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GhGate {
    /// Not a merge (or a merge onto a non-default base): run the real gh unchanged.
    PassThrough,
    /// A merge onto the default branch, and both consent markers are present:
    /// autonomous mode is on AND auto-merge is enabled — allow it.
    AllowMerge,
    /// A privileged action (default-branch merge, or a release/tag publish)
    /// authorized by a valid one-time human grant. The shim CONSUMES the grant and
    /// allows exactly this one action.
    AllowGrant,
    /// Allowed by **supervised dangerous mode** (`dangerous_mode` marker, while NOT
    /// autonomous): the human is present and told the agent to just do merges/
    /// releases. Distinct from the autonomous blanket and from a grant so the audit
    /// records which gate path allowed it. Not consumed (it's a standing mode).
    AllowDangerous,
    /// A merge onto the default branch (or a release) without authorization: block
    /// (the human gate — the human can grant a one-time exception).
    Block,
    /// A merge whose base branch couldn't be determined: block, fail-safe — we must
    /// never let an unverifiable merge reach the default branch.
    BlockUnverifiable,
}

/// gh flags (across the subcommands we gate — `pr merge`, `release create/edit/
/// delete`, `-R/--repo`) that take a SEPARATE value token, so a positional scan
/// must consume that value and not mistake it for the command/subcommand/target.
/// Missing one mis-parses the target: e.g. `gh release create --title "X" v1`
/// would read the tag as `X` and fail-safe-block a legitimately granted release
/// (rev-86 LOW). `=`/glued forms are single tokens and handled separately. Keep
/// this in sync with the shim's shell scanner value-flag list.
const GH_VALUE_FLAGS: &[&str] = &[
    "-R", "--repo", "-b", "--body", "-t", "--subject", "--title", "-F", "--body-file",
    "--author-email", "--match-head-commit", "-n", "--notes", "--notes-file",
    "--notes-start-tag", "--target", "--discussion-category",
];

/// The positional (non-flag) tokens of a gh argv, in order, skipping flags and
/// consuming the values of `GH_VALUE_FLAGS` — crucially the global `-R/--repo` that
/// gh accepts BEFORE or BETWEEN the command tokens (rev-79 F1), and the release
/// value-flags before the tag positional (rev-86). `--flag=x` / `-Rx` are single
/// tokens, skipped as flags. Excludes the leading `gh`. So `positionals[0]`/`[1]`
/// are the command + subcommand and `[2]` is the target (PR ref / tag) wherever
/// the flags land.
pub fn gh_positionals(args: &[&str]) -> Vec<String> {
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let t = args[i];
        if GH_VALUE_FLAGS.contains(&t) {
            i += 2; // flag + its value
            continue;
        }
        if t.starts_with('-') {
            i += 1; // boolean flag, or `--flag=…` / `-R…` glued (single token)
            continue;
        }
        pos.push(t.to_string());
        i += 1;
    }
    pos
}

/// The `-R/--repo` value from a gh argv, if present, in any accepted form
/// (`-R x`, `--repo x`, `--repo=x`, `-Rx`). The shim passes this to its base /
/// default-branch lookups so they resolve for the SAME repo the user targeted
/// (rev-79 F2), not always the cwd repo. Pure/testable.
pub fn gh_repo_flag(args: &[&str]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let t = args[i];
        if t == "-R" || t == "--repo" {
            return args.get(i + 1).map(|s| s.to_string());
        }
        if let Some(v) = t.strip_prefix("--repo=") {
            return Some(v.to_string());
        }
        if t.len() > 2 {
            if let Some(v) = t.strip_prefix("-R") {
                return Some(v.to_string());
            }
        }
        i += 1;
    }
    None
}

/// Whether an argv is a GitHub *merge* invocation the shim must gate: `gh pr merge`
/// (in ANY flag arrangement, including `-R/--repo` before or between the command
/// tokens — rev-79 F1) or a raw `gh api` call to a pull-request merge endpoint (the
/// cheap-to-catch API bypass). Pure so both the shim's shell mirror and the tests
/// agree on exactly what counts as a merge. `args` excludes the leading `gh`. The
/// api-graphql `mergePullRequest` mutation is also caught.
pub fn gh_is_merge_invocation(args: &[String]) -> bool {
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let pos = gh_positionals(&a);
    let cmd = pos.first().map(String::as_str);
    let sub = pos.get(1).map(String::as_str);
    // `gh [globals] pr [flags] merge …` — command+subcommand wherever the flags land.
    if cmd == Some("pr") && sub == Some("merge") {
        return true;
    }
    // `gh api …` touching a pulls/<n>/merge REST endpoint or the graphql
    // mergePullRequest mutation. Conservative substring match over the args.
    if cmd == Some("api") {
        let joined = a.join(" ");
        let low = joined.to_ascii_lowercase();
        if low.contains("mergepullrequest") {
            return true;
        }
        // REST: .../pulls/<something>/merge  (also /merge as the tail)
        if joined.contains("/merge") && joined.contains("pulls") {
            return true;
        }
    }
    false
}

/// The gate decision (pure spec for the shim), the SINGLE decision point every
/// merge form routes through. `is_merge` is [`gh_is_merge_invocation`];
/// `base`/`default` are the PR base branch and the repo default branch as resolved
/// by the *real* gh (`None` = couldn't determine); `autonomous`/`auto_merge`/
/// `dangerous`/`grant_valid` are the group's live marker states. A merge onto the
/// default branch is allowed when **`(autonomous && auto_merge)`** (autonomous
/// blanket) OR **`(dangerous && !autonomous)`** (supervised dangerous mode — the
/// human is present) OR a valid one-time grant; a non-default base passes; an
/// undeterminable base fails safe (block). `dangerous` is a no-op while autonomous
/// (the two are mutually exclusive, enforced at the setters; the `!autonomous`
/// guard is defensive).
pub fn gh_gate_decision(
    is_merge: bool,
    base: Option<&str>,
    default: Option<&str>,
    autonomous: bool,
    auto_merge: bool,
    dangerous: bool,
    grant_valid: bool,
) -> GhGate {
    if !is_merge {
        return GhGate::PassThrough;
    }
    match (base, default) {
        (Some(b), Some(d)) if !b.is_empty() && !d.is_empty() => {
            if b != d {
                GhGate::PassThrough // integration-branch flow is untouched
            } else if autonomous && auto_merge {
                GhGate::AllowMerge // blanket opening while in autonomous auto-merge
            } else if dangerous && !autonomous {
                GhGate::AllowDangerous // supervised: human present, told it to merge
            } else if grant_valid {
                GhGate::AllowGrant // one-time human grant for THIS pr — consumed
            } else {
                GhGate::Block
            }
        }
        // A raw `gh api` merge has no cheaply-resolvable base ref → block it as an
        // unverifiable default-branch merge (the api path is a documented bypass
        // surface; blocking the cheap-to-catch shape is the safe default).
        _ => GhGate::BlockUnverifiable,
    }
}

/// A release-publishing gh action the shim must gate (`gh release create|edit|
/// delete <tag>`) and the tag it targets. `None` for any other gh (incl. read-only
/// `release view`/`list`/`download`). Pure over the parsed positionals so the shim
/// and tests agree. Uses `gh_positionals` so `-R/--repo` before/between tokens is
/// handled like the merge path.
pub fn gh_release_action(args: &[String]) -> Option<(String, String)> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let pos = gh_positionals(&a);
    if pos.first().map(String::as_str) != Some("release") {
        return None;
    }
    let sub = pos.get(1).map(String::as_str)?;
    if !matches!(sub, "create" | "edit" | "delete") {
        return None; // view/list/download/upload/download → not a publish action
    }
    // `gh release <sub> <tag>` — the tag is the next positional.
    let tag = pos.get(2)?.to_string();
    Some((sub.to_string(), tag))
}

/// The gate decision for a release/tag publish (#83). Parallel to
/// `gh_gate_decision` for merges: allowed when **`(autonomous && auto_release)`**
/// (the blanket opening) OR a valid one-time grant for that tag (consumed). Because
/// publishing to the world (GitHub release + npm via a `v*` tag → release.yml) is a
/// bigger blast radius than a merge, releases get their **own independent** toggle
/// (`auto_release`, default OFF) — turning on autonomous never surprise-publishes;
/// the human opts in separately, or grants each release one at a time.
pub fn release_gate_decision(
    autonomous: bool,
    auto_release: bool,
    dangerous: bool,
    grant_valid: bool,
) -> GhGate {
    if autonomous && auto_release {
        GhGate::AllowMerge // blanket opening (reusing the "allowed, not grant-consumed" variant)
    } else if dangerous && !autonomous {
        GhGate::AllowDangerous // supervised: human present, told it to release
    } else if grant_valid {
        GhGate::AllowGrant
    } else {
        GhGate::Block
    }
}

/// What a `git push` publishes with respect to tags (#83). Local `git tag` is
/// harmless — only the PUSH reaches the world — so the git shim gates pushes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitTagPush {
    /// Not a tag push (a branch push, or not `git push` at all): pass through.
    None,
    /// Pushes ALL/annotated tags in bulk (`--tags`/`--follow-tags`/`--mirror`) —
    /// can't be matched to a single tag grant, so block with guidance to push the
    /// one approved tag instead.
    Bulk,
    /// An explicit tag ref (`refs/tags/<t>`, `tag <t>`, or a bare `v*` refspec) →
    /// gate on a release grant for `<t>`.
    Tag(String),
}

/// Classify a `git` argv for tag-push gating (#83). Pure over the args (git global
/// options like `-C <dir>` / `-c <k=v>` are skipped to find the `push` command).
/// A bare refspec is treated as a tag only when it matches the release pattern
/// (`v*` — the `release.yml` `on.push.tags` trigger; these MUST stay in sync);
/// the shim confirms ambiguous cases against the real git, but the classification
/// here is the testable spec.
pub fn git_tag_push(args: &[String]) -> GitTagPush {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    // Locate the git subcommand, skipping value-taking globals.
    let mut i = 0;
    let mut cmd: Option<&str> = None;
    while i < a.len() {
        let t = a[i];
        if matches!(t, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path") {
            i += 2;
            continue;
        }
        if t.starts_with('-') {
            i += 1;
            continue;
        }
        cmd = Some(t);
        break;
    }
    if cmd != Some("push") {
        return GitTagPush::None;
    }
    let rest = &a[i + 1..];
    // Bulk tag pushes.
    if rest.iter().any(|t| matches!(*t, "--tags" | "--follow-tags" | "--mirror")) {
        return GitTagPush::Bulk;
    }
    // Positional refspecs after `push`: the first non-flag is the remote; the rest
    // are refspecs. Also handle the `git push <remote> tag <name>` form.
    let mut positionals = rest.iter().filter(|t| !t.starts_with('-'));
    let _remote = positionals.next();
    let mut prev_tag_kw = false;
    for spec in positionals {
        if *spec == "tag" {
            prev_tag_kw = true;
            continue;
        }
        if prev_tag_kw {
            return GitTagPush::Tag(grant_segment(spec));
        }
        // `src:dst` — the destination ref is what lands on the remote.
        let dst = spec.rsplit(':').next().unwrap_or(spec);
        if let Some(t) = dst.strip_prefix("refs/tags/") {
            return GitTagPush::Tag(grant_segment(t));
        }
        // A bare refspec matching the RELEASE TRIGGER pattern. This MUST track
        // `.github/workflows/release.yml`'s `on.push.tags` (currently `v*` — ANY
        // ref starting with `v`, not just `v<digit>`), or a `vbeta`/`vRelease` tag
        // push would publish to the world yet slip the gate (rev-86). It's only a
        // *candidate* — the shim confirms it's actually a tag (not a same-prefixed
        // branch) against the real git before gating, so a branch like `vfeature`
        // still passes.
        let name = dst.trim_start_matches('+');
        if name.starts_with('v') {
            return GitTagPush::Tag(grant_segment(name));
        }
    }
    GitTagPush::None
}

/// Whether an unexpired grant authorizes an action right now: a grant exists and
/// its expiry (unix seconds) is in the future. Pure so the TTL rule is testable;
/// the shim reads the expiry from the grant file. `None` = no grant file.
pub fn grant_unexpired(expires_secs: Option<u64>, now_secs: u64) -> bool {
    matches!(expires_secs, Some(exp) if now_secs < exp)
}

/// Pure latch transition for the low-disk backstop (#134). Given current free
/// bytes on the workspace drive, the arming threshold `low`, the higher clear
/// threshold `clear` (hysteresis), and whether a notice is already latched,
/// return `(new_latched, fire_now)`. `fire_now` is the one-per-episode edge:
/// true only on the tick that first crosses below `low`. Split out so the
/// arm/clear hysteresis is unit-testable without a real disk.
pub fn low_disk_transition(free: u64, low: u64, clear: u64, latched: bool) -> (bool, bool) {
    if !latched && free < low {
        (true, true) // crossed below → arm and fire this tick
    } else if latched && free >= clear {
        (false, false) // recovered past the hysteresis mark → reset the latch
    } else {
        (latched, false) // no edge — hold state, stay quiet
    }
}

/// The one-per-episode low-disk notice delivered to a group's orchestrator.
pub fn low_disk_notice(free_bytes: u64) -> String {
    let free_gb = free_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    format!(
        "[loomux] disk space low: only {free_gb:.1} GB free on the workspace drive. \
         At 0 bytes, backend builds (cargo) fail machine-wide and durable writes fail — \
         a full disk previously destroyed a live task board. Reclaim space now: end merged \
         worktrees (end_group with cleanup), `cargo clean` in idle worktrees, or clear temp \
         files. You will get this notice at most once per low-disk episode."
    )
}

/// Attention routing (#6): does a pane's ANSI-stripped output tail look like a
/// CLI parked on a prompt only the human can answer — a permission dialog, a
/// yes/no confirmation, or a numbered/selection menu? This is the "last output"
/// half of idle-with-prompt detection; the caller pairs it with an
/// output-quiet check (this alone can't tell a live prompt from the same words
/// scrolled past). So it errs toward recognizable interactive-prompt structure
/// rather than any mention of a question. Case-insensitive.
///
/// Two tiers of signal, by how prose-safe each is (#40 review):
/// - *Structured* signals (numbered y/n menu, explicit y/n tokens, stock
///   permission phrasings) don't occur in ordinary prose, so they're honored
///   across the last ~12 lines.
/// - *Prose-like* signals — a bare selection pointer and the plain-English menu
///   footer ("use arrow keys", "enter to select") — DO appear in finished-turn
///   agent output (agents describe keyboard UIs, paste shell prompts, echo
///   `a › b` breadcrumbs). A *live* menu paints these as the last thing on
///   screen, with its pointer *leading* an option line (after any box frame);
///   prose does neither. So the pointer must lead a de-framed line, and the
///   footer is only read from the last few non-empty lines — once the CLI
///   redraws its idle input box below the prose, the phrase falls out of range.
pub fn prompt_wait_detected(tail: &str) -> bool {
    let lines: Vec<String> = tail
        .lines()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return false;
    }
    let recent = &lines[lines.len().saturating_sub(12)..];
    let joined = recent.join("\n");

    // Strip a line's leading box border / bullet / indent so a menu pointer
    // inside a bordered dialog (`│ ❯ Yes`) is seen to *lead* its content.
    fn deframe(l: &str) -> &str {
        l.trim_start_matches(|c: char| {
            c == '│' || c == '┃' || c == '|' || c == '*' || c == '●' || c == '•' || c == '◆'
                || c.is_whitespace()
        })
    }

    // The last few non-empty lines — "the last thing the CLI painted". Both
    // prose-like signals (pointer, footer) are read only from here (#40 review):
    // a live menu paints its pointer/footer last, whereas finished-turn prose
    // that happens to lead a line with `❯`/`›`/`→` (a `❯ npm run dev` shell
    // example, a fenced repro block) is followed by the CLI's redrawn idle input
    // box, which pushes it out of this window.
    let last_painted = &recent[recent.len().saturating_sub(3)..];

    // Selection pointer marking the highlighted choice. A `❯`/`›`/`→` that
    // *leads* a line's content (after any box frame) is menu-shaped; the same
    // glyph mid-line is pervasive in ordinary output — pasted shell prompts
    // (`demo ❯ npm run dev`), UI breadcrumbs (`Home › Prefs`), diff/log arrows.
    // Requiring it to lead rules those out; requiring it in the last painted
    // lines also rules out a *leading* glyph in finished prose above the idle box.
    let has_pointer_option = last_painted.iter().any(|l| {
        let d = deframe(l);
        d.starts_with('❯') || d.starts_with('›') || d.starts_with('→')
    });
    // A numbered yes/no menu even without the pointer glyph.
    let has_numbered_menu = joined.contains("1. yes") || joined.contains("❯ 1.");
    // Explicit yes/no confirmation tokens.
    let has_yes_no = joined.contains("(y/n)")
        || joined.contains("[y/n]")
        || joined.contains("y/n)")
        || joined.contains("[y/n]?")
        || joined.contains("yes/no");
    // Stock permission / trust / continue phrasings from Claude Code & Copilot.
    let has_permission_phrase = joined.contains("do you want to proceed")
        || joined.contains("do you want to make this edit")
        || joined.contains("do you want to create")
        || joined.contains("do you want to run")
        || joined.contains("do you trust")
        || joined.contains("trust the files")
        || joined.contains("allow this")
        || joined.contains("allow command")
        || joined.contains("grant access")
        || joined.contains("press enter to continue")
        || joined.contains("waiting for your");
    // Interactive selection-menu footer (AskUserQuestion / Copilot / inquirer).
    // Claude Code's AskUserQuestion highlights the active option with reverse
    // video (an ANSI attribute stripped before we see it), so no glyph survives
    // and this footer is the only durable signal (#40). Like the pointer it's
    // read only from the last painted lines. NOTE: matched on single lines, so a
    // footer wrapped across rows in a very narrow pane, or a localized / reworded
    // footer, won't match — a known gap (see design doc).
    let footer = last_painted.join("\n");
    let has_menu_footer = footer.contains("enter to select")
        || footer.contains("enter to confirm")
        || footer.contains("use arrow")
        || footer.contains("arrow keys")
        || footer.contains("↑↓")
        || footer.contains("↑/↓");
    has_pointer_option || has_numbered_menu || has_yes_no || has_permission_phrase || has_menu_footer
}

/// Does the pane's ANSI-stripped output tail show Copilot's "Enable autopilot
/// mode" consent dialog? (#101). Copilot opens this dialog at startup when
/// launched with `--autopilot` (the group posture); the kickoff path answers it
/// deterministically before pasting the brief so the two can't collide.
///
/// Anchored on BOTH the dialog title and its enable option — the exact strings
/// the 1.0.68 TUI paints (`title:"Enable autopilot mode"`, item label
/// `"Enable all permissions (recommended)"`). Requiring both rules out ordinary
/// prose that merely mentions autopilot or permissions, so a false positive
/// can't make loomux fire a stray Enter into a working pane. Case-insensitive;
/// tolerant of the parenthetical and of line wrapping (each substring is matched
/// independently against the whole tail).
pub fn copilot_autopilot_prompt_detected(tail: &str) -> bool {
    let t = tail.to_lowercase();
    t.contains("enable autopilot mode") && t.contains("enable all permissions")
}

/// How a `deliver_prompt` call relates to the pane's lifecycle. Governs the boot
/// readiness wait AND the one-time copilot autopilot-consent confirm (#101).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Delivery {
    /// First prompt to a freshly *booted* pane (a fresh spawn's kickoff): wait
    /// for the CLI to paint, and — for an autopilot copilot agent — answer the
    /// "Enable autopilot mode" consent dialog before pasting.
    FreshKickoff,
    /// First prompt to a *resumed* pane: still wait for the CLI to paint, but a
    /// resume restores allow-all/autopilot from the session event log, so the
    /// consent dialog does not reappear — skip the confirm (and its fail-soft
    /// wait) rather than watch for a dialog that will never show.
    ResumeKickoff,
    /// A mid-session delivery to an already-running pane (a follow-up / steer):
    /// no readiness wait, no dialog.
    MidSession,
}

impl Delivery {
    /// Whether to hold the paste until the CLI has painted its UI — true for
    /// either kickoff (the CLI has just been launched), false mid-session.
    fn wait_ready(self) -> bool {
        matches!(self, Delivery::FreshKickoff | Delivery::ResumeKickoff)
    }
    /// Whether this delivery follows a fresh CLI boot (the only time copilot
    /// shows its autopilot consent dialog).
    fn is_fresh_boot(self) -> bool {
        matches!(self, Delivery::FreshKickoff)
    }
}

/// Whether a delivery should attempt the copilot autopilot-consent confirm.
/// Only a *fresh boot* of an *unattended copilot* agent shows the "Enable
/// autopilot mode" dialog — resume restores the consent from the session log,
/// and mid-session deliveries are long past boot — so both of those skip the
/// (fail-soft, up to `AUTOPILOT_DIALOG_WAIT`) watch. Pure so the gate is
/// unit-testable without a live pty.
pub fn should_confirm_copilot_autopilot(cli: &str, unattended: bool, fresh_boot: bool) -> bool {
    fresh_boot && unattended && cli == "copilot"
}

/// Distinct agent working directories to remove when a group is torn down
/// with worktree cleanup: dedup (case/separator-insensitively), and never the
/// repo root itself — the orchestrator and any repo-mode workers run there, so
/// removing it would delete the user's own checkout. Pure so the path
/// filtering is testable without a real git tree; the actual removal is
/// `git::git_worktree_remove`, which git refuses on a non-worktree anyway.
pub fn worktree_cleanup_targets(repo: &str, cwds: &[String]) -> Vec<String> {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let repo_n = norm(repo);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for c in cwds {
        if c.trim().is_empty() {
            continue;
        }
        let cn = norm(c);
        if cn == repo_n {
            continue; // repo root — the orchestrator's cwd, never a worktree
        }
        if seen.insert(cn) {
            out.push(c.clone());
        }
    }
    out
}

/// Best-effort extraction of a session's dollar cost from a pane's
/// ANSI-stripped terminal tail. Claude Code renders running cost in its
/// in-pane statusline (bottom of the screen), so scan lines bottom-up and
/// return the dollar amount from the lowest line that carries one — that is
/// the freshest statusline render. Thousands separators are tolerated.
/// Returns `None` when no `$<amount>` token is present.
pub fn parse_session_cost(text: &str) -> Option<f64> {
    for line in text.lines().rev() {
        if let Some(cost) = line
            .match_indices('$')
            .find_map(|(i, _)| parse_dollar_amount(&line[i + 1..]))
        {
            return Some(cost);
        }
    }
    None
}

/// Parse a leading `1,234.56`-style number (optionally after the `$` already
/// consumed by the caller), returning `None` if the text does not start with
/// a digit. Commas are dropped; a single decimal point is honored.
fn parse_dollar_amount(after_dollar: &str) -> Option<f64> {
    let mut digits = String::new();
    let mut seen_dot = false;
    for c in after_dollar.chars() {
        match c {
            '0'..='9' => digits.push(c),
            ',' if !seen_dot => {} // thousands separator
            '.' if !seen_dot => {
                seen_dot = true;
                digits.push('.');
            }
            _ => break,
        }
    }
    // Reject a bare "." or empty (a lone `$` or `$.`); require a real digit.
    if digits.is_empty() || digits == "." {
        return None;
    }
    digits.parse::<f64>().ok()
}

/// Models are interpolated into a shell command line; restrict them to
/// identifier-ish characters so a crafted "model" can't smuggle arguments.
fn sanitize_model(m: &str, fallback: &str) -> String {
    let cleaned: String = m
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

/// `sanitize_model` with no fallback: an empty/unusable model stays empty, which
/// a block reads as "inherit the class default for my CLI" (`workflow::model_of`).
/// The workflow parser needs this because a block's *effective* CLI isn't known
/// until the group default is in hand.
pub(crate) fn sanitize_model_opt(m: &str) -> String {
    m.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect()
}

/// Read the block roster out of a group.json `guardrails` object (#222).
///
/// **Back-compat is the whole job here.** A group.json written before the block
/// model has no `blocks` array — it has the eight flat per-role fields
/// (`worker_cli`, `reviewer_model`, …). Reconstruct the same four blocks from
/// those, so a group launched on 0.8.0 rejoins on this build with exactly the
/// CLIs and models it had. An empty result is fine: `clamped()` fills it with
/// the built-in roster.
fn read_blocks(g: &Value) -> Vec<workflow::Block> {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    if let Some(arr) = g["blocks"].as_array() {
        return arr
            .iter()
            .filter_map(|b| {
                let id = workflow::sanitize_id(&s(b, "id"))?;
                // An unrecognized kind is DROPPED, never coerced to worker — the
                // same rule the workflow parser enforces, applied to persisted
                // state, because a hand-edited group.json is the other way an
                // unknown kind could reach a spawn.
                let kind = workflow::kind_from_str(&s(b, "kind"))?;
                let name = workflow::sanitize_display(&s(b, "name"));
                Some(workflow::Block {
                    name: if name.is_empty() { id.clone() } else { name },
                    id,
                    kind,
                    cli: s(b, "cli"),
                    model: s(b, "model"),
                    prompt: b["prompt"].as_str().map(workflow::sanitize_persona),
                    profile: b["profile"].as_str().map(|p| p.trim().to_string()),
                    allow: b["allow"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str())
                                .filter_map(profiles::sanitize_allow)
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            })
            .collect();
    }
    // Legacy shape (pre-#222).
    let pins = [
        (Role::Orchestrator, s(g, "orchestrator_cli"), s(g, "orchestrator_model")),
        (Role::Worker, s(g, "worker_cli"), s(g, "worker_model")),
        (Role::Reviewer, s(g, "reviewer_cli"), s(g, "reviewer_model")),
        (Role::Planner, s(g, "planner_cli"), s(g, "planner_model")),
    ];
    if pins.iter().all(|(_, cli, model)| cli.is_empty() && model.is_empty()) {
        return Vec::new(); // not a legacy file either — clamped() supplies the roster
    }
    workflow::default_roster(
        &pins.iter().map(|(k, c, m)| (*k, c.as_str(), m.as_str())).collect::<Vec<_>>(),
    )
}

/// Serialize the block roster for group.json. The inverse of [`read_blocks`];
/// `block_map_round_trips_through_group_json` pins the pair.
fn blocks_json(blocks: &[workflow::Block]) -> Value {
    Value::Array(
        blocks
            .iter()
            .map(|b| {
                json!({
                    "id": b.id,
                    "name": b.name,
                    "kind": b.kind.as_str(),
                    "cli": b.cli,
                    "model": b.model,
                    "prompt": b.prompt,
                    "profile": b.profile,
                    "allow": b.allow,
                })
            })
            .collect(),
    )
}

#[derive(Clone)]
pub struct GroupInfo {
    pub id: String,
    pub repo: String,
    pub guardrails: Guardrails,
}

#[derive(Clone, Debug)]
pub struct AgentEntry {
    pub id: String,
    pub group: String,
    pub name: String,
    /// Who set `name` — the precedence tier for renames (#95r). See
    /// [`NameSource`] and [`OrchRegistry::rename_agent`].
    pub name_source: NameSource,
    /// The workflow block this agent was spawned from (#222) — its *identity*.
    /// `worker` for the built-in roster; `rev-security` for a declared block.
    /// This is what a gate, an edge or a `spawn_agent(block:)` names.
    pub block: workflow::BlockId,
    /// The agent's **capability class**, derived from its block's `kind`. Every
    /// structural guarantee (deny-flags, cwd rule, MCP tool scope) keys off
    /// this, and it can only ever be one of four values — see [`Role`].
    pub role: Role,
    pub token: String,
    pub status: AgentStatus,
    pub pty_id: Option<u32>,
    pub task: String,
    /// The agent CLI's conversation session id. For Claude, loomux assigns
    /// it at spawn (`--session-id`), so a finished worker's session can be
    /// resumed later for follow-ups on its task without a cold start.
    pub session_id: Option<String>,
    /// Working directory the pane runs in; resume must reuse it so the
    /// resumed session's file operations land where the work happened.
    pub cwd: String,
    /// Unix-ms this worker/reviewer became idle (spawned without a task, or
    /// reported done/blocked); `None` while it has work or for the
    /// orchestrator. The idle reaper (`idle_kill_minutes`) reads this.
    pub idle_since_ms: Option<u64>,
    /// Unix-ms this agent was registered (spawn time). Drives the per-agent
    /// and group uptime shown in the lifecycle summary; unaffected by idle.
    pub started_ms: u64,
    /// Watchdog: Unix-ms of this agent's last observed activity — terminal
    /// output growth or a report/message. Silence is measured from here.
    /// Seeded at spawn and whenever work is (re)assigned. See
    /// `watchdog_should_notify`.
    pub last_progress_ms: u64,
    /// Watchdog: last observed value of the pane's monotonic pty output
    /// counter, so a tick can tell whether the CLI has emitted anything since
    /// the previous one even when the output ring is saturated.
    pub last_output_total: u64,
    /// Watchdog anti-nag latch: set once a stall notice has been delivered for
    /// the current stall, cleared when the agent produces output/reports again.
    pub watchdog_notified: bool,
    /// Autonomous idle-tick latch (#83), meaningful only for the orchestrator:
    /// set when an idle-tick notice is delivered, cleared when the pane produces
    /// output again (the orchestrator acted on the tick). Mirrors
    /// `watchdog_notified` — one notice per idle window. `last_output_total` /
    /// `last_progress_ms` above double as the idle-tick output counter / quiet
    /// clock for the orchestrator, which the watchdog never touches.
    pub idle_tick_notified: bool,
}

/// One pane that needs the human, pushed to the frontend as an `orch-attention`
/// event (the full current set each scan; the frontend badges panes by
/// `pty_id`). `reason`, most- to least-urgent:
/// - `blocked` — a worker reported it is blocked
/// - `waiting` — the pane is parked on a prompt (idle-with-prompt)
/// - `report`  — a worker reported done (awaiting the human's review/merge)
/// - `gate`    — this agent's task sits at a human merge gate on the board
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AttentionItem {
    /// Empty for a plain (non-orchestration) pane, which is keyed only by
    /// `pty_id` — the human's hand-opened shells have no agent identity (#40).
    pub agent_id: String,
    pub group: String,
    pub name: String,
    /// `None` for a plain pane (no orchestration role).
    pub role: Option<Role>,
    pub pty_id: Option<u32>,
    pub reason: &'static str,
    /// Short human phrase for the badge tooltip and the toast body.
    pub detail: String,
}

/// Work-item statuses shown on the task board. Kept as strings (not an
/// enum) so the wire/JSON forms stay obvious; validated on every write.
pub const TASK_STATUSES: [&str; 8] = [
    "queued",        // planned, not started
    "in-progress",   // a worker is on it
    "review",        // reviewer agent engaged
    "pr",            // PR open, review loop finished
    "prototype",     // demo-gated draft awaiting the human's promote/scrap verdict (#147)
    "human-testing", // done pending the human's validation
    "done",          // merged / accepted by the human
    "blocked",
];

/// Statuses where the human's merge-gate actions (approve / request changes)
/// apply: the PR is open and awaiting the human's decision.
pub const MERGE_GATE_STATUSES: [&str; 2] = ["pr", "human-testing"];

/// The demo-gate status (#147): a prototype the human is evaluating before
/// deciding whether to promote it to a full production build. Its board action
/// is **Proceed** (not the merge-gate approve/changes) — see `proceed_task`.
pub const PROTOTYPE_STATUS: &str = "prototype";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskNote {
    pub ts_ms: u64,
    pub author: String,
    pub text: String,
}

/// One work item on a group's task board (`tasks.json`, array order =
/// priority). Maintained by the orchestrator via MCP tools and by the human
/// via the pane's task-board overlay; each side is notified of the other's
/// edits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub issue: Option<String>,
    #[serde(default)]
    pub pr: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    /// Agent CLI session that did/does this work; lets the orchestrator
    /// resume it for follow-ups instead of cold-starting or disturbing a
    /// busy worker.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub notes: Vec<TaskNote>,
    #[serde(default)]
    pub updated_ms: u64,
}

/// Field edits for `upsert_task`; `None` leaves a field untouched.
#[derive(Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub status: Option<String>,
    pub issue: Option<String>,
    pub pr: Option<String>,
    pub assignee: Option<String>,
    pub session: Option<String>,
    pub note: Option<String>,
}

/// Durable roster entry (`agents.json` per group): which sessions belonged
/// to which role. This is what lets the session browser mark orchestrator/
/// worker sessions and restore a whole orchestration after loomux restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub role: String,
    /// The workflow block this agent was spawned from (#222). Persisted so a
    /// session rejoin restores the agent's *identity* (its persona, CLI and
    /// model), not merely its capability class. Additive: a roster row written
    /// before blocks deserializes to empty, and the rejoin falls back to the
    /// class's default block.
    #[serde(default)]
    pub block: String,
    pub name: String,
    /// Precedence tier of `name` (#95r). Persisted so a session rejoin restores
    /// the human's rename AND its "human beats orchestrator" tier, not just the
    /// text. Additive: legacy rows without it deserialize to `Default::default`.
    #[serde(default)]
    pub name_source: NameSource,
    pub session: Option<String>,
    pub cwd: String,
    pub status: String,
    pub updated_ms: u64,
}

/// Durable per-agent usage snapshot (`usage.json` per group). Keyed by the CLI
/// session id when known (so a resumed session updates one row instead of
/// double-counting), else `agent:<id>`. Snapshots survive `kill_agent`/exit —
/// captured in `mark_dead` — so a group's lifetime cost keeps counting
/// recycled panes (issue #42).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageSnapshot {
    /// Stable identity: the CLI session id, or `agent:<id>` when there is none.
    pub key: String,
    pub agent_id: String,
    pub name: String,
    pub role: String,
    /// Where the figures came from: `transcript` (token-derived, exact tokens),
    /// `statusline` (last-resort parse of the CLI's own dollar figure), or
    /// `none` (nothing available yet).
    pub source: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// Dollar cost, or `None` when only tokens are known (unknown model, or a
    /// transcript-less agent whose statusline shows nothing).
    pub cost_usd: Option<f64>,
    /// true = dollars estimated from the price table; false = reported by the
    /// CLI's statusline (which reads $0.00 on subscription/Max accounts).
    pub estimated: bool,
    pub model: Option<String>,
    pub updated_ms: u64,
}

/// A recorded session's orchestration identity, for the session browser.
#[derive(Clone, Serialize)]
pub struct SessionRole {
    pub session_id: String,
    pub group_id: String,
    pub role: String,
    pub agent_name: String,
    /// Whether that group currently has live agents in this app instance.
    pub group_live: bool,
}

/// Identity resolved from an MCP request's token header.
#[derive(Clone, Debug)]
pub struct Caller {
    pub agent_id: String,
    pub group: String,
    pub role: Role,
}

/// A workflow block's persona, compiled down to what each agent CLI can
/// actually consume (#222).
///
/// The investigation's load-bearing asymmetry: **Claude takes a persona
/// inline** (`--agents '<json>' --agent <id>`), so loomux can synthesize one
/// with zero repo files. **Copilot cannot** — its `--agent` resolves a *name*
/// against `.github/agents/`, so it can only ever engage a file the user
/// already wrote. Hence exactly three outcomes, one per row below:
///
/// | block persona | claude | copilot |
/// |---|---|---|
/// | none | nothing (pre-#222 command, byte for byte) | nothing |
/// | `prompt:` (inline) | `--agents` + `--agent` | **kickoff-prompt injection** |
/// | `profile: .github/agents/x.md` | file body → `--agents` + `--agent` | `--agent x` (native) |
///
/// A persona-less block passes `PersonaInject::default()` and every field is
/// `None`/empty, so no flag is added at all. That is the mechanism behind the
/// "a repo with no workflow file behaves exactly as before" guarantee.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonaInject {
    /// Claude `--agents '<json>'`: the inline block definition. Already
    /// persona-sanitized and ASCII-escaped, so it is safe inside the
    /// single-quoted shell token `build_agent_command` wraps it in.
    pub claude_agents_json: Option<String>,
    /// Claude `--agent <id>`: activates the block defined above. Always set
    /// together with `claude_agents_json`.
    pub claude_agent: Option<String>,
    /// Copilot `--agent <name>`. Set **only** for a user-authored
    /// `.github/agents/*.md` — loomux never generates a file there to make this
    /// flag reachable (see `profiles::is_copilot_native`).
    pub copilot_agent: Option<String>,
    /// Extra pre-approved tool patterns from the persona's `allow:`. Widens
    /// only *within* the capability class: deny rules beat allow rules on both
    /// CLIs, so this can never re-grant what the block's `kind` denies.
    pub extra_allow: Vec<String>,
    /// Persona body for the **kickoff-prompt fallback**: the CLI has no inline
    /// persona flag and no user-authored file to point at (copilot + an inline
    /// `prompt:`). Delivered as text in the kickoff, which every CLI reads.
    pub kickoff: Option<String>,
}

/// A block's persona after the `prompt:` / `profile:` sources have been
/// resolved to one body — the shared input to both the CLI flags
/// ([`PersonaInject`]) and the block's role-instruction file.
#[derive(Clone, Debug)]
#[doc(hidden)] // pub for integration tests: they compile a block exactly as spawn does
pub struct ResolvedPersona {
    /// The persona body (sanitized for a shell line).
    pub text: String,
    /// The handle a native `--agent` flag names.
    pub name: String,
    /// One-line description for the `--agents` JSON (Claude requires it).
    pub description: String,
    pub mode: profiles::ProfileMode,
    pub allow: Vec<String>,
    /// Set when the persona came from a user-authored `.github/agents/*.md`,
    /// which is the only thing Copilot's native `--agent` can resolve.
    pub copilot_native: bool,
}

/// Payload asking the frontend to open a pane for an agent. Also the return
/// value of `create_orchestration` (the orchestrator's own pane).
#[derive(Clone, Debug, Serialize)]
pub struct SpawnRequest {
    pub group_id: String,
    pub agent_id: String,
    pub role: Role,
    pub name: String,
    pub cwd: String,
    /// Shell command line (the historical form). Still emitted as the fallback
    /// the pane runs through a shell when the direct spawn can't apply.
    pub command: String,
    /// Wall-clock Unix-ms after which a still-queued request must be dropped
    /// unserviced (#106) — set to the deadline of the backend's own `bind`
    /// wait (`now + BIND_TIMEOUT`). A frontend that recovers from a stall past
    /// this point drops the request instead of opening a zombie pane against
    /// state the bind-timeout has already torn down. See `spawn_request_expired`.
    pub deadline_ms: u64,
    /// Structured invocation (program + literal args) for direct-CLI spawn
    /// (issue #78): when its program resolves to a native executable the pane
    /// spawns it as the ConPTY child with no wrapper shell. Mirrors `command`.
    pub argv: Vec<String>,
    /// Extra environment variables to set on this pane's child, on TOP of the
    /// shared pane env (#83). For *agent* panes this carries the gh-shim PATH
    /// prefix + `LOOMUX_GROUP_DIR` so the merge gate is enforced; empty for a
    /// plain human shell, so the human's own terminals are untouched.
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

pub struct OrchRegistry {
    /// Root of persistent state: `<root>/<group>/{group.json,state.json,audit.jsonl,configs/}`.
    root: PathBuf,
    /// Absent in unit tests: spawning then skips the pane round-trip.
    app: Mutex<Option<AppHandle>>,
    groups: Mutex<HashMap<String, GroupInfo>>,
    agents: Mutex<HashMap<String, AgentEntry>>,
    by_token: Mutex<HashMap<String, String>>,
    by_pty: Mutex<HashMap<u32, String>>,
    pending_binds: Mutex<HashMap<String, mpsc::Sender<u32>>>,
    port: AtomicU16,
    seq: AtomicU32,
    /// Per-pane delivery locks so two prompts to the SAME pane can't
    /// interleave keystrokes, while a slow delivery (waiting out a busy
    /// CLI) doesn't block deliveries to other panes.
    delivery: Mutex<HashMap<u32, Arc<Mutex<()>>>>,
    /// Outcome of the most recent delivery to each pane (keyed by pty id), so a
    /// delivery can flush a previous prompt still stranded in the input box
    /// before pasting (#81/#84). An `Arc` so a delivery thread can record its
    /// outcome without holding `&self`.
    last_delivery: Arc<Mutex<HashMap<u32, DeliveryOutcome>>>,
    /// Serializes task-board read-modify-write cycles (MCP threads and the
    /// human UI mutate the same tasks.json).
    tasks_lock: Mutex<()>,
    /// Serializes group creation + orchestrator registration: the group id
    /// is chosen by liveness, and a group only becomes live once its
    /// orchestrator is registered — without this, two concurrent launches
    /// on one repo would share an id.
    creation: Mutex<()>,
    /// Test seam (#222): when set, `pr_head` returns this instead of shelling out
    /// to `gh pr view --json headRefOid`. The verdict↔revision binding has to be
    /// exercised through the real MCP dispatch against a repo that isn't on GitHub;
    /// mirrors `claude_projects_dir`. `None` in the app, always.
    pr_head_override: Mutex<Option<String>>,
    /// Groups the human has paused: loomux stops delivering prompts/kickoffs
    /// to them so their agents idle out (see `deliver_prompt`). Mirrored to a
    /// `paused` marker file per group so it survives restarts.
    paused: Mutex<HashSet<String>>,
    /// Per-group spawn timestamps (Unix-ms) for the spawn-rate guardrail;
    /// pruned to the trailing hour on each check.
    spawn_times: Mutex<HashMap<String, Vec<u64>>>,
    /// Weak handle to our own `Arc`, set once at startup (`set_self_arc`), so
    /// `&self` methods can hand an owned registry to background threads (e.g.
    /// the copilot session watcher). `Weak` avoids a self-referential `Arc`
    /// cycle that would leak the registry.
    self_arc: Mutex<Weak<OrchRegistry>>,
    /// Attention routing (#6): latched worker reports awaiting the human's
    /// eyes — agent id → "done" | "blocked". Set by the report tool, cleared on
    /// ack (the human focused the pane) or reassignment.
    attn_reports: Mutex<HashMap<String, &'static str>>,
    /// Attention routing: per-agent output-quiet tracking, agent id → (last pty
    /// output total, Unix-ms that total last changed). Kept separate from the
    /// watchdog's counter so the two features never clobber each other's clocks.
    attn_quiet: Mutex<HashMap<String, (u64, u64)>>,
    /// Attention routing: agents whose live `waiting` badge the human has acked
    /// (focused the pane) while the prompt is still on screen. Unlike
    /// `blocked`/`report`, `waiting` is recomputed every scan, so without this it
    /// would re-light ~3s after focus. Cleared when the pane's output next
    /// changes (the menu was answered / the CLI repainted) so a genuinely new
    /// prompt flags again. See `attention_tick`.
    attn_waiting_ack: Mutex<HashSet<String>>,
    /// Attention routing: the agent → reason set last emitted, so a scan fires a
    /// desktop toast only once per attention onset (the event itself is
    /// re-emitted every tick and the frontend badges idempotently).
    attn_emitted: Mutex<HashMap<String, String>>,
    /// Groups with desktop notifications enabled (durable `notify` marker file).
    notify_groups: Mutex<HashSet<String>>,
    /// Autonomous mode (#83): groups whose orchestrator is idle-ticked to run its
    /// monitoring/intake cadence unattended. Durable via an `autonomous` marker
    /// file whose *content* is the enable-time usage-token anchor (see
    /// `set_autonomous` / `autonomy_anchor`), so budget metering survives restarts.
    autonomous_groups: Mutex<HashSet<String>>,
    /// Autonomous mode (#83): groups where the orchestrator may merge an
    /// adequately-tested PR itself instead of holding at the human merge gate.
    /// Default OFF (absent) = today's behavior (human merges). Durable
    /// `auto_merge` marker file, mirroring `notify`/`paused`. The behavior lives
    /// in the orchestrator template; the backend stores/exposes the flag and
    /// mirrors it into the orchestrator's kickoff config.
    auto_merge_groups: Mutex<HashSet<String>>,
    /// Autonomous mode (#83): groups where the orchestrator may publish a
    /// release/tag itself (`gh release …`, pushing a `v*` tag) instead of needing a
    /// per-tag human grant. **Independent of `auto_merge`** — the human can allow
    /// auto-merge while keeping releases manual, or opt into both. Default OFF
    /// (absent), so turning autonomous on never surprise-publishes. Durable
    /// `auto_release` marker; gated behind autonomous exactly like `auto_merge`.
    auto_release_groups: Mutex<HashSet<String>>,
    /// Supervised dangerous mode (#83): groups where the human — present and
    /// supervising — has authorized the orchestrator to merge/release itself
    /// WITHOUT being autonomous. Default OFF. **Mutually exclusive with
    /// `autonomous`**: enabling autonomous force-clears this, and enabling this is
    /// rejected while autonomous. Durable `dangerous_mode` marker; the gate's single
    /// decision point allows a privileged action via `(dangerous && !autonomous)`.
    dangerous_groups: Mutex<HashSet<String>>,
    /// Autonomous mode (#83): per-group idle-tick delivery timestamps (Unix-ms)
    /// for the `MAX_IDLE_TICKS_PER_HOUR` backstop; pruned to the trailing hour on
    /// each check. The runaway analogue of `spawn_times`.
    idle_tick_times: Mutex<HashMap<String, Vec<u64>>>,
    /// Debounced cap-change notices (#79): group → its pending, not-yet-
    /// delivered `PendingMaxNotice`. `set_max_agents` folds rapid stepper
    /// clicks in here (persist/enforce/audit stay per-click); the
    /// `start_max_notice_flusher` loop delivers one coalesced notice per burst
    /// once the group falls quiet.
    pending_max_notice: Mutex<HashMap<String, PendingMaxNotice>>,
    /// Test-only override of the Claude transcript root (`~/.claude/projects`).
    /// `None` in production. Set via `set_claude_projects_dir` so the usage
    /// reader can be pointed at a fixture tree without touching global env —
    /// safe under parallel test execution.
    claude_projects_dir: Mutex<Option<PathBuf>>,
    /// Low-disk backstop latch (#134): true once the one-per-episode disk-space
    /// notice has been delivered, cleared when free space recovers past
    /// `LOW_DISK_CLEAR_BYTES`. Machine-wide (the disk is shared across groups).
    low_disk_notified: Mutex<bool>,
    /// Per-group count of unreadable audit lines already breadcrumbed (#240).
    /// The viewer re-polls `audit_log` in follow mode, so a log that already
    /// carries torn lines — every log written before the append fix — would
    /// otherwise emit a breadcrumb per poll and flood out the crash-forensics
    /// history it shares the file with. Report only when the count *changes*.
    audit_skips_notified: Mutex<HashMap<String, usize>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Remove a durable consent marker (autonomous / auto_merge), treating "already
/// gone" as success but a real IO failure as an error the caller MUST propagate
/// (#83). A marker that survives a failed *disable* would silently re-enable the
/// feature on the next restart's re-seed — the one failure direction this
/// consent-boundary feature must never have — so the toggle fails loudly and
/// leaves the in-memory flag matching the surviving marker rather than reporting a
/// disable that didn't durably happen.
fn remove_marker(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("failed to remove marker {}: {e}", path.display())),
    }
}

/// Sanitize a grant target (PR ref / tag) into a safe single path segment: keep
/// only `[A-Za-z0-9._-]`, everything else → `_`. Prevents a `/` or `..` in a tag
/// from escaping the grant dir, and MUST match the shim's `tr -c` sanitizer so the
/// backend and shim agree on the grant filename. Pure/testable.
pub fn grant_segment(target: &str) -> String {
    let s: String = target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    if s.is_empty() { "_".to_string() } else { s }
}

/// Extract the numeric PR id from a board task's `pr` field, which may be a bare
/// number (`7`), a `#7`, or a full PR URL (`…/pull/7`). `None` if no number is
/// found. Pure so the normalization is testable; the grant file is keyed `pr-<N>`.
pub fn pr_number(pr: &str) -> Option<u64> {
    // A PR URL ends in `/pull/<n>`; otherwise take the last run of digits.
    let tail = pr.rsplit(['/', '#', ' ']).find(|s| !s.is_empty()).unwrap_or(pr);
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Map a caller-supplied image extension to a vetted one, rejecting anything
/// outside the allowlist (#72). A pasted image's extension is attacker-influenced
/// (it rides in from the browser clipboard), so we never echo it into a filename
/// verbatim: only these known raster/image types are accepted, which both blocks
/// path-traversal / executable extensions and matches what the agent CLIs open.
/// Pure and `pub` so the mapping is unit-testable.
pub fn sanitize_attachment_ext(ext: &str) -> Option<&'static str> {
    match ext.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        "bmp" => Some("bmp"),
        _ => None,
    }
}

/// Should prompt delivery keep holding for the human to stop typing? (#43,
/// option A). Returns true to keep waiting, false to proceed. Pure so the
/// hold/deadline decision is unit-testable without a live PTY.
///
/// - `last_input_ms` is the pane's last-keystroke time (0 = none recorded).
/// - `held` is how long THIS hold has already waited; once it reaches
///   `max_hold` we deliver anyway so a long compose session can't starve the
///   report queue.
fn should_hold_for_user(
    last_input_ms: u64,
    now_ms: u64,
    held: Duration,
    quiet_window: Duration,
    max_hold: Duration,
) -> bool {
    if held >= max_hold {
        return false; // cap reached — deliver anyway
    }
    if last_input_ms == 0 {
        return false; // nobody has typed in this pane
    }
    let since = now_ms.saturating_sub(last_input_ms);
    since < quiet_window.as_millis() as u64
}

/// Poll-and-hold loop that drives `should_hold_for_user`: block while
/// `last_input_ms()` reports recent keystrokes, until quiet or the hold hits
/// `max_hold`. Returns `Some(held_ms)` when it actually waited (so the caller
/// can audit the held duration), `None` when it was already quiet on entry.
///
/// Generic over the keystroke source and timings so the wiring — that the
/// loop consults the decision every `poll` and honours the starvation cap —
/// is integration-testable without a live PTY (see the #40 twice-bitten
/// lesson: the pure decision alone isn't enough; the loop that calls it must
/// be exercised too).
#[doc(hidden)] // pub for integration tests
pub fn hold_until_quiet<F: Fn() -> u64>(
    last_input_ms: F,
    quiet_window: Duration,
    max_hold: Duration,
    poll: Duration,
) -> Option<u64> {
    let start = std::time::Instant::now();
    let mut held = false;
    while should_hold_for_user(last_input_ms(), now_ms(), start.elapsed(), quiet_window, max_hold) {
        held = true;
        std::thread::sleep(poll);
    }
    held.then(|| start.elapsed().as_millis() as u64)
}

/// Production wrapper: hold delivery to `pty_id` while its human is typing,
/// using the shipped window/cap/poll timings.
fn wait_for_user_quiet(ptys: &crate::pty::PtyManager, pty_id: u32) -> Option<u64> {
    hold_until_quiet(
        || ptys.last_user_input_ms(pty_id).unwrap_or(0),
        USER_QUIET_HOLD,
        USER_QUIET_MAX_HOLD,
        USER_QUIET_POLL,
    )
}

/// For a freshly spawned group copilot pane (launched with `--autopilot`): watch
/// for the "Enable autopilot mode" consent dialog and answer it (Enter selects
/// the default "Enable all permissions"). Copilot 1.0.69 opens this dialog in
/// response to the FIRST message submit, not at boot (#179), so the caller runs
/// this right AFTER the kickoff Enter — selecting the default both enables
/// autopilot and lets the just-submitted brief proceed (the pending message is
/// not discarded). Fail-soft: returns `false` without acting if the dialog does
/// not appear within `AUTOPILOT_DIALOG_WAIT` (e.g. copilot changed the flow, or
/// consent was already recorded), and the caller's submit retries carry on.
///
/// Returns `true` iff it detected and answered the dialog. Times/keys come from
/// module constants so the wiring is testable; the pure recognizer is
/// [`copilot_autopilot_prompt_detected`].
fn confirm_copilot_autopilot_dialog(
    ptys: &crate::pty::PtyManager,
    pty_id: u32,
    root: &Path,
    group: &str,
    agent: &str,
) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < AUTOPILOT_DIALOG_WAIT {
        let Some(out) = ptys.output_tail(pty_id) else {
            return false; // terminal closed — let the caller's own checks report
        };
        if copilot_autopilot_prompt_detected(&strip_ansi(&out)) {
            let _ = ptys.write_bytes(pty_id, COPILOT_AUTOPILOT_CONFIRM_KEYS);
            append_audit(root, group, "loomux", "copilot-autopilot-confirmed", json!({
                "to": agent,
                "waited_ms": start.elapsed().as_millis() as u64,
            }));
            // Let the TUI dismiss the dialog and repaint before the brief pastes.
            std::thread::sleep(AUTOPILOT_DIALOG_SETTLE);
            return true;
        }
        std::thread::sleep(AUTOPILOT_DIALOG_POLL);
    }
    false
}

/// 128-bit hex token from std's OS-seeded `RandomState` (each instance draws
/// fresh OS entropy) mixed with time. Deliberately not getrandom-based: see
/// the Cargo.toml note on bcryptprimitives/ProcessPrng. Tokens authenticate
/// same-user localhost agents; that adversary can read the config files
/// anyway, so this strength is proportionate.
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(32);
    for i in 0..2u64 {
        let mut h = std::hash::RandomState::new().build_hasher();
        h.write_u64(now_ms());
        h.write_u64(i);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// UUIDv4-format session id from the same entropy source as `new_token`
/// (Claude's `--session-id` requires a valid UUID).
fn new_session_uuid() -> String {
    let hex = new_token(); // 32 hex chars
    let b = hex.as_bytes();
    let s = |r: std::ops::Range<usize>| std::str::from_utf8(&b[r]).unwrap();
    // Stamp version (4) and variant (8) nibbles per RFC 4122.
    format!(
        "{}-{}-4{}-8{}-{}",
        s(0..8),
        s(8..12),
        s(13..16),
        s(17..20),
        s(20..32)
    )
}

/// Session ids get interpolated into a shell command line; validate (not
/// filter — a mangled id would silently resume the wrong session).
fn sanitize_session(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty() && t.len() <= 64 && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .then(|| t.to_string())
}

/// Normalize a caller-supplied pane name (#95r): trim, drop control characters
/// (so a pasted name can't smuggle newlines/escape codes into the pane title or
/// the roster JSON), and cap the length. Not a security boundary — the title is
/// rendered via `textContent`, never HTML — just hygiene. May return empty (an
/// all-control/whitespace name); callers decide what an empty result means.
fn sanitize_agent_name(name: &str) -> String {
    name.trim().chars().filter(|c| !c.is_control()).take(40).collect()
}

/// Add a folder to copilot's `trustedFolders` config, returning the new
/// file content — or None when nothing should be written (already trusted,
/// or the existing config is unparseable and must not be clobbered). The
/// file is JSONC-ish: leading `//` comment lines before a JSON object;
/// comments and unknown fields are preserved.
pub fn add_trusted_folder(config_text: &str, folder: &str) -> Option<String> {
    let mut comment_len = 0;
    for line in config_text.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("//") || t.is_empty() {
            comment_len += line.len();
        } else {
            break;
        }
    }
    let (comments, body) = config_text.split_at(comment_len);
    let mut v: Value = if body.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(body).ok()?
    };
    let arr = v
        .as_object_mut()?
        .entry("trustedFolders")
        .or_insert_with(|| json!([]))
        .as_array_mut()?;
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    if arr.iter().any(|e| e.as_str().is_some_and(|s| norm(s) == norm(folder))) {
        return None;
    }
    arr.push(json!(folder));
    Some(format!("{comments}{}\n", serde_json::to_string_pretty(&v).ok()?))
}

/// Pre-trust an agent's workspace in copilot's config so its pane doesn't
/// boot into a folder-trust dialog — which eats the kickoff paste and gets
/// blind-answered by the submit retries. Best-effort: on any failure the
/// dialog simply appears as before.
fn pre_trust_copilot_folder(folder: &str) {
    let home = std::env::var("COPILOT_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".copilot")))
        .unwrap_or_default();
    if home.as_os_str().is_empty() {
        return;
    }
    let path = home.join("config.json");
    let text = fs::read_to_string(&path).unwrap_or_default();
    if let Some(updated) = add_trusted_folder(&text, folder) {
        let _ = fs::create_dir_all(&home);
        let _ = fs::write(&path, updated);
    }
}

/// Stable, filesystem-safe group id for a repo path, so relaunching an
/// orchestrator on the same repo reattaches to the same state directory.
pub(crate) fn group_id_for_repo(repo: &str) -> String {
    let norm = repo.replace('\\', "/").to_lowercase();
    let norm = norm.trim_end_matches('/');
    // FNV-1a 64
    let mut h: u64 = 0xcbf29ce484222325;
    for b in norm.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let slug: String = norm
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();
    let slug = if slug.is_empty() { "repo".into() } else { slug };
    format!("{slug}-{:08x}", (h >> 32) as u32 ^ h as u32)
}

/// Size cap after which the audit log rolls over to `audit.1.jsonl` (one
/// generation kept). Full prompt texts land in the audit, so it grows fast.
const AUDIT_ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// Serializes every in-process audit writer — appends *and* rotation — against
/// each other (#240). Two guarantees hang off it: no thread holds an append
/// handle across another thread's rotation rename, and two threads can't both
/// decide to rotate (the second rename would discard the generation the first
/// just created). Uncontended in practice — an append is a few hundred bytes
/// every few seconds — and held only for the open+write, never across
/// orchestration work, so it can't meaningfully block a pane. `lock_safe`
/// keeps a poisoned lock from turning best-effort auditing into a panic
/// cascade (see `obs::LockExt`).
static AUDIT_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// Test-only seam (#240): how long *this thread* pauses between rotation's
    /// size check and its rename. Rotation is check-then-rename, and the window
    /// between the two is a few instructions wide — too narrow for a test to
    /// force a second rotator into it, which is why the lock's rotation-race
    /// protection would otherwise ship unverified. Widening the window on demand
    /// makes the race a real reproducer (see
    /// `concurrent_rotations_keep_the_retained_generation`).
    ///
    /// Zero in production, and read only when a rotation actually fires (an 8 MB
    /// rollover), so the production path pays one thread-local read per rollover
    /// and nothing else. Thread-local rather than a global so it can't leak into
    /// the other tests cargo runs in parallel in this process. Mirrors the
    /// existing `set_claude_projects_dir` test seam.
    static ROTATE_CHECK_PAUSE: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

/// Widen this thread's rotation check-to-rename window. Test-only (see
/// `ROTATE_CHECK_PAUSE`); production never calls it, so the window stays as
/// narrow as the code makes it.
#[doc(hidden)] // pub for integration tests
pub fn set_rotate_check_pause_for_test(pause: Duration) {
    ROTATE_CHECK_PAUSE.with(|p| p.set(pause));
}

/// Roll `audit.jsonl` over to `audit.1.jsonl` once it exceeds `cap`.
/// Factored out so the threshold behavior is testable with a tiny cap.
#[doc(hidden)] // pub for integration tests
pub fn rotate_audit_if_needed(dir: &Path, cap: u64) {
    let _guard = AUDIT_LOCK.lock_safe();
    rotate_audit_locked(dir, cap);
}

/// Rotation body. Callers must already hold `AUDIT_LOCK` — `append_audit` takes
/// it once and covers rotate+append with a single acquisition (the lock is not
/// reentrant).
///
/// A *cross-process* writer (the gh/git shims' `>>`) can still open the log a
/// moment before this rename and write through the handle afterwards. That's
/// accepted, not a defect: the handle keeps pointing at the same file, so the
/// line lands at the tail of `audit.1.jsonl` instead of the fresh `audit.jsonl`
/// — never lost, and the viewer reads both generations (`audit_log`). Only its
/// position in the timeline shifts, and only for a record that raced an 8 MB
/// rollover.
fn rotate_audit_locked(dir: &Path, cap: u64) {
    let path = dir.join("audit.jsonl");
    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > cap {
        // Check-then-rename: the size we just read is only still true because
        // `AUDIT_LOCK` is held. Without it a second rotator could pass this same
        // check, wait out the first one's rename, and then rename the *fresh*
        // log over `audit.1.jsonl` — discarding the generation the first just
        // retained. `ROTATE_CHECK_PAUSE` (zero outside tests) widens exactly
        // this window so that race can be reproduced rather than argued.
        let pause = ROTATE_CHECK_PAUSE.with(|p| p.get());
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
        let _ = fs::rename(&path, dir.join("audit.1.jsonl")); // replaces the old generation
    }
}

/// Monotonic counter that makes every temp filename unique, so two concurrent
/// writers to the same durable file never share a `.tmp` sibling. Some of the
/// files written through `atomic_write` (state.json, group.json) are not
/// serialized under a lock, so distinct temp names are what keeps a concurrent
/// pair from corrupting each other's scratch file. A std atomic keeps us clear
/// of the getrandom-based crates the Windows 10 baseline can't load (see the
/// Cargo.toml notes) — no `tempfile` needed for a unique name.
static ATOMIC_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Durably replace `path` with `bytes`: write a same-directory temp file, flush
/// it to disk, then atomically rename it over the destination. A failure or
/// crash mid-write leaves the previous good file intact (at worst an orphaned
/// `.tmp` sibling) — never the truncated/empty destination that plain
/// `fs::write` produces. This is the #133 fix: a disk-full `fs::write` had
/// truncated tasks.json and destroyed the live board.
///
/// Same-directory temp is required for rename atomicity on Windows — a rename
/// across volumes falls back to a non-atomic copy. `fs::rename` on Windows maps
/// to `MoveFileExW` with `REPLACE_EXISTING`, which atomically replaces the
/// destination on the same volume, so the primary path already does the right
/// thing; the fallback only covers the rare case where the destination is
/// briefly locked (antivirus, an open reader). The temp is fsync'd before the
/// rename so a rename can't expose a metadata-only file whose data blocks never
/// reached disk — exactly the disk-full failure mode.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    // Ensure the destination dir exists — group state dirs always do, but the #83
    // grant subdirs (`merge_grants/`, `release_grants/`) may be fresh.
    fs::create_dir_all(dir)?;
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let seq = ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{stem}.{}.{seq}.tmp", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?; // durable before the rename — the disk-full guard
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Rename can fail if the destination is momentarily locked. Fall
            // back to a direct write so the update isn't lost; keep the temp on
            // failure so the new contents remain recoverable.
            let r = fs::write(path, bytes);
            if r.is_ok() {
                let _ = fs::remove_file(&tmp);
            }
            r
        }
    }
}

/// Audit-log writer usable from background threads (delivery outcomes)
/// without holding a registry reference.
///
/// Appends are atomic *per record*, which the sibling `atomic_write` does not
/// give you — that one makes whole-file *replaces* crash-safe (#133), a
/// different failure mode. Two rules keep a record whole (#240):
///
/// 1. **One buffer, one `write_all`.** The record and its newline are serialized
///    up front and handed to the OS in a single call. Append-mode atomicity is
///    per write *syscall*, so a record emitted as many writes is a record other
///    writers can be scheduled into the middle of. The old code wrote
///    `writeln!(f, "{line}")` with `line` a `serde_json::Value`: `Display` walks
///    the tree and emits a write per token, and concurrent writers (mass
///    agent-exit at shutdown, delivery threads) spliced each other character by
///    character — real logs ended up with
///    `{{""actionaction""::""agent-exitagent-exit""`.
///
///    Precisely: `write_all` *loops* on a short write, and each iteration is its
///    own append — so the atomicity rests on the file not short-writing, not on
///    a contract. For a regular file on our baselines (Windows, Linux) a
///    blocking write of a record-sized buffer is issued as one write and returns
///    complete or fails; short writes are a pipe/socket/`ENOSPC` behavior. That
///    is the practice this relies on, and it is worth restating rather than
///    claiming a guarantee the API doesn't make: audit records can be large
///    (full prompt texts land here).
/// 2. **`AUDIT_LOCK` for in-process writers**, so appends don't race rotation.
///
/// The *other* writers are the gh/git shims (`gh_shim_sh`, `git_shim_sh`), in
/// other processes and beyond any mutex of ours. They rely on rule 1 alone, and
/// satisfy it the same way: one `printf` of one whole line, appended with `>>`.
/// Any shim audit line must stay a single `printf`; building a line across two
/// redirections would reintroduce exactly this bug across processes.
fn append_audit(root: &Path, group: &str, actor: &str, action: &str, detail: Value) {
    let dir = root.join(group);
    let record = json!({ "ts_ms": now_ms(), "actor": actor, "action": action, "detail": detail });
    let mut line = record.to_string();
    line.push('\n'); // newline in the same buffer — a separate write could be split off
    // Serialize before taking the lock: JSON formatting is the expensive part
    // and no other writer cares about it.
    let _ = fs::create_dir_all(&dir);
    let _guard = AUDIT_LOCK.lock_safe(); // covers rotate + append as one unit
    rotate_audit_locked(&dir, AUDIT_ROTATE_BYTES);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join("audit.jsonl")) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// One parsed audit-log line, for the in-app timeline viewer. Mirrors the
/// shape written by `append_audit`; `detail` stays an opaque JSON value so the
/// frontend can render per-action without the backend knowing every schema.
#[derive(Clone, Debug, Serialize)]
pub struct AuditEntry {
    pub ts_ms: u64,
    pub actor: String,
    pub action: String,
    pub detail: Value,
}

/// Parse audit JSONL text into entries, in file order (oldest first), skipping
/// malformed lines. Pure so ordering/robustness is testable without touching
/// the filesystem or a registry.
#[doc(hidden)] // pub for integration tests
pub fn parse_audit_lines(text: &str) -> Vec<AuditEntry> {
    parse_audit_lines_counted(text).0
}

/// Same, but also reports how many non-blank lines failed to parse. Skipping
/// silently is how #240 stayed invisible for so long: a corrupt log read as a
/// slightly shorter timeline, with nothing anywhere saying lines had been
/// dropped. Blank lines don't count — a torn tail or a trailing newline is
/// normal; unparseable *content* is not.
#[doc(hidden)] // pub for integration tests
pub fn parse_audit_lines_counted(text: &str) -> (Vec<AuditEntry>, usize) {
    let mut skipped = 0usize;
    let entries = text
        .lines()
        .filter_map(|line| {
            if line.trim().is_empty() {
                return None;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                skipped += 1;
                return None;
            };
            Some(AuditEntry {
                ts_ms: v["ts_ms"].as_u64().unwrap_or(0),
                actor: v["actor"].as_str().unwrap_or("").to_string(),
                action: v["action"].as_str().unwrap_or("").to_string(),
                detail: v.get("detail").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    (entries, skipped)
}

/// Upper bound on entries returned to the viewer: the audit grows fast (full
/// prompt texts) and only the most recent slice is worth rendering. Keeps the
/// payload bounded even against a rotated + current pair near the 8 MB cap.
const AUDIT_VIEW_LIMIT: usize = 5000;

fn render_template(tpl: &str, vars: &[(&str, &str)]) -> String {
    let mut out = tpl.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Strip ANSI escape sequences (CSI, OSC, two-byte ESC) and carriage
/// returns so `get_output` returns readable text from raw terminal bytes.
pub fn strip_ansi(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            i += 1;
            match bytes.get(i) {
                Some(b'[') => {
                    // CSI: parameters/intermediates until a final byte 0x40-0x7E.
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                Some(b']') => {
                    // OSC: until BEL or ESC \.
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                Some(_) => i += 2 - 1, // two-byte escape: skip the introducer
                None => {}
            }
            continue;
        }
        if b == b'\r' || (b < 0x20 && b != b'\n' && b != b'\t') {
            i += 1;
            continue;
        }
        // Decode this UTF-8 unit; fall back to skipping the byte.
        let len = match b {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        if let Ok(s) = std::str::from_utf8(&bytes[i..(i + len).min(bytes.len())]) {
            out.push_str(s);
        }
        i += len;
    }
    out
}

/// Decide whether a freshly spawned CLI is ready to receive typed input,
/// from its output volume and how long that output has been stable. Pure so
/// the thresholds are testable; the polling loop lives in `deliver_prompt`.
pub fn cli_ready(output_len: usize, quiet_for: Duration, elapsed: Duration) -> bool {
    elapsed >= READY_MIN_WAIT && output_len >= READY_MIN_OUTPUT && quiet_for >= READY_QUIET
}

/// Wrap prompt text in a bracketed paste so multi-line prompts land in the
/// CLI's input box instead of submitting at the first newline. The Enter is
/// sent separately after `PASTE_SUBMIT_DELAY`.
pub fn bracketed_paste(text: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(text.len() + 12);
    v.extend_from_slice(b"\x1b[200~");
    v.extend_from_slice(text.replace("\r\n", "\n").as_bytes());
    v.extend_from_slice(b"\x1b[201~");
    v
}

/// The byte sequence loomux writes to submit a delivered prompt, chosen per
/// CLI. Kept pure and `&'static` so the exact bytes are unit-assertable.
///
/// Claude Code submits on a bare CR (`\r`).
///
/// GitHub Copilot's TUI (#98) gates *keyboard* input on terminal focus: it
/// enables DEC mode 1004 focus reporting (`ESC[?1004h`) and, in its editor's
/// key handler, drops every non-paste keystroke while its focus flag is false
/// (`if (!focused && !key.paste && code != backspace/delete) return`). A
/// *paste* bypasses that guard — which is why a delivered prompt's text lands
/// in the input box — but the Enter that follows is a plain key, so on a pane
/// that isn't the focused one (the normal case when an agent delivers to
/// another agent's pane) it is ignored and the prompt just sits there until a
/// human clicks in (whereupon the terminal emits focus-in and their Enter
/// works). Prefixing the CR with a focus-in report (`ESC[I`, which Copilot
/// parses to a focus event that flips its flag true) makes the very next key —
/// our Enter — accepted, so the prompt submits without a human. Copilot leaves
/// its flag true afterward, so the spaced retry Enters need no re-prefix, but
/// they carry it too so each retry is self-sufficient if a stray blur arrives.
pub fn submit_sequence(cli: &str) -> &'static [u8] {
    match cli {
        "copilot" => b"\x1b[I\r",
        _ => b"\r",
    }
}

/// The outcome of the most recent delivery to a pane, kept in-memory per pty so
/// the next delivery can detect a previous prompt still stranded in the input
/// box (#81/#84).
#[derive(Clone, Copy, Debug)]
struct DeliveryOutcome {
    /// Whether that delivery's Enter was observed to submit (box cleared / turn
    /// started). `false` means the text may still be sitting unsubmitted.
    confirmed: bool,
    /// Unix-ms the final Enter was sent — the reference point for deciding
    /// whether a human has since typed into the pane.
    submit_sent_ms: u64,
}

/// Whether output growth after the submit Enter counts as the submit landing.
/// A successful submit clears the box and the CLI repaints / starts a turn (a
/// burst of output); an ignored Enter produces effectively none.
///
/// Only trustworthy when the pane reached quiet *before* the Enter. If the
/// submit-wait hit `SUBMIT_MAX_WAIT` while output was still streaming
/// (`reached_quiet == false`), the Enter landed mid-stream and the window's
/// growth is that stream, not the submit's — which would false-confirm and
/// strand a prompt recorded as confirmed (rev-32). So a cap-hit-without-quiet
/// is never confirmed; a false "unconfirmed" is just a harmless flush next
/// time. Pure so the rule is testable; the polling loop lives in
/// `deliver_prompt`.
pub fn submit_confirmed(reached_quiet: bool, baseline_total: u64, observed_total: u64) -> bool {
    reached_quiet && observed_total.saturating_sub(baseline_total) >= SUBMIT_CONFIRM_MIN_BYTES
}

/// Whether to flush a previous delivery's stranded text (a single submit press)
/// before pasting the next prompt (#81/#84).
///
/// Flush only on the exact stranded-text signature: the previous delivery to
/// this pane was NOT confirmed as submitted, AND no human has typed into the
/// pane since (so the box holds the earlier *agent* prompt, not a person's
/// half-written line — which must never be blind-submitted). Never flushes on
/// the first delivery to a pane (`prev_confirmed == None`) or after a confirmed
/// one. A false "unconfirmed" here is safe: the flush Enter lands on an already
/// empty box and is a no-op.
pub fn should_flush_before_paste(prev_confirmed: Option<bool>, human_typed_since: bool) -> bool {
    matches!(prev_confirmed, Some(false)) && !human_typed_since
}

/// How a single human write into a pane's input changes box occupancy (#111).
/// Classified from the keystroke's *content*, which is what tells a line still
/// sitting in the box from one already submitted — an output-byte heuristic
/// can't (one keystroke's input-line redraw, or ambient agent streaming, can
/// exceed any fixed burst floor, and a sub-floor submit never clears it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HumanInput {
    /// Printable text was entered — a line now sits unsubmitted in the box.
    Content,
    /// The line was submitted (Enter) or explicitly cleared — the box is empty.
    Submit,
    /// Navigation/editing that neither adds visible text nor submits (arrows,
    /// backspace, bare escape sequences) — box occupancy is unchanged.
    Neutral,
}

/// Classify one human write for the delivery paste guard (#111). Pure so the
/// rule is testable; `write_pty` calls it to maintain the per-pane
/// `input_pending` flag.
///
/// - A write carrying **bracketed-paste markers** (`ESC[200~` / `ESC[201~`) is
///   pasted text held UNSUBMITTED in the box → `Content`, even if it ends in a
///   newline: under bracketed-paste mode (Claude Code and most modern TUIs) a
///   pasted newline is literal, not a submit — the human's separate Enter
///   afterwards is the submit. Checked first so an interior/trailing newline
///   can't misread the paste as submitted (the #111 loss otherwise).
/// - Otherwise a carriage return / newline submits the current line, UNLESS
///   printable text follows the last newline (then that trailing text is a fresh
///   unsubmitted line → `Content`).
/// - Ctrl-U (kill-line) / Ctrl-C (interrupt) empty the box → `Submit`.
/// - Any remaining printable/graphic character (after skipping escape sequences)
///   → `Content`.
/// - Otherwise (arrows, backspace, lone escape sequences) → `Neutral`.
///
/// Erring toward `Content`/`Neutral` on ambiguous input keeps the guard biased
/// to the safe hold: a real sitting line is never misread as empty. Residual
/// clears that leave the flag stuck (bounded by the 60s abort) — Esc-to-clear,
/// Ctrl-W/Ctrl-K, backspace-to-empty, and soft-newline editors — need true
/// box-occupancy detection, which is issue #112.
pub fn classify_human_input(data: &str) -> HumanInput {
    // Bracketed paste: the text lands in the box unsubmitted regardless of any
    // newline it contains, so never read it as a submit.
    if data.contains(BRACKETED_PASTE_START) || data.contains(BRACKETED_PASTE_END) {
        return HumanInput::Content;
    }
    if let Some(pos) = data.rfind(['\r', '\n']) {
        // `\r`/`\n` are single-byte, so `pos + 1` is a valid char boundary.
        let after = &data[pos + 1..];
        return if input_has_printable(after) { HumanInput::Content } else { HumanInput::Submit };
    }
    // Line-clear controls empty the box even without a newline.
    const KILL_LINE: char = '\u{15}'; // Ctrl-U
    const INTERRUPT: char = '\u{03}'; // Ctrl-C
    if !data.is_empty() && data.chars().all(|c| c == KILL_LINE || c == INTERRUPT) {
        return HumanInput::Submit;
    }
    if input_has_printable(data) {
        HumanInput::Content
    } else {
        HumanInput::Neutral
    }
}

/// xterm bracketed-paste bracket sequences: the terminal wraps pasted text in
/// these so an app can tell a paste from typing (and hold pasted newlines soft).
const BRACKETED_PASTE_START: &str = "\u{1b}[200~";
const BRACKETED_PASTE_END: &str = "\u{1b}[201~";

/// Whether `s` contains a graphic character once terminal escape sequences are
/// skipped — the test for "this write put visible text in the box". Skips CSI
/// (`ESC [ … final`, e.g. arrow keys, bracketed-paste markers) AND the string
/// sequences a terminal emits in *reply* to a program's query — OSC (`ESC ]`)
/// and DCS/SOS/PM/APC (`ESC P`/`X`/`^`/`_`) — plus other short `ESC`-led
/// sequences, so none of their printable bytes read as typed content.
///
/// The OSC/DCS skip is #179: GitHub Copilot queries the terminal's colors
/// (`ESC]10;?`, `ESC]11;?`, `ESC]4;n;?`) and version (`ESC[>q`) at boot; the
/// webview's xterm auto-answers, and those answers reach us through `write_pty`
/// exactly like a keystroke. Their bodies are printable (`11;rgb:0d0d/1111/1717`),
/// so without skipping the whole string they were misread as a human's line,
/// wedging `input_pending` true and stalling the fresh-copilot kickoff paste in
/// the #111 box-clear hold (up to its 60s abort) — the "prompt never delivered"
/// symptom. Claude Code issues no such query, so only copilot tripped it.
fn input_has_printable(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b {
            i += 1;
            match b.get(i) {
                // CSI: `ESC [` … final byte in 0x40..=0x7e.
                Some(b'[') => {
                    i += 1;
                    while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
                        i += 1;
                    }
                    i += 1; // consume the CSI final byte
                }
                // OSC / DCS / SOS / PM / APC: a string sequence whose body is
                // arbitrary (often printable) text, terminated by BEL (0x07) or
                // ST (`ESC \`). Skip the whole thing — it's a query reply, not
                // typed input (#179).
                Some(b']') | Some(b'P') | Some(b'X') | Some(b'^') | Some(b'_') => {
                    i += 1;
                    while i < b.len() {
                        if b[i] == 0x07 {
                            i += 1; // BEL terminator
                            break;
                        }
                        if b[i] == 0x1b && b.get(i + 1) == Some(&b'\\') {
                            i += 2; // ST terminator (ESC \)
                            break;
                        }
                        i += 1;
                    }
                }
                // Any other 2-byte / lone ESC sequence (charset select, `ESC=`, …).
                _ => {
                    i += 1;
                }
            }
            continue;
        }
        // Printable ASCII (space..~) or any UTF-8 multibyte lead/continuation.
        if (0x20..0x7f).contains(&b[i]) || b[i] >= 0x80 {
            return true;
        }
        i += 1; // C0 control (tab, backspace/DEL handled below, etc.)
    }
    false
}

/// One tick of the pre-paste human-input hold (#111): given whether a human's
/// line is still sitting in the box, decide whether to paste, keep holding, or
/// abort. Pure so the hold/abort rule is testable without a live PTY;
/// `hold_for_human_input` drives it. Mirrors `should_flush_before_paste` — a
/// small, total gate.
///
/// - `box_pending`: does the box still hold a human's unsubmitted line?
/// - `held` / `max_hold`: the bounded wait; at the cap we abort rather than
///   paste onto a line the human never cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteGate {
    /// Box is clear (or was never dirty) — paste the prompt.
    Paste,
    /// Human's line is still in the box — keep waiting.
    Hold,
    /// The box never cleared within the bound — do not paste; notify instead.
    Abort,
}

pub fn resolve_paste_gate(box_pending: bool, held: Duration, max_hold: Duration) -> PasteGate {
    if !box_pending {
        return PasteGate::Paste; // box is empty — paste the prompt
    }
    if held >= max_hold {
        return PasteGate::Abort; // bounded wait elapsed and the line never cleared
    }
    PasteGate::Hold
}

/// Outcome of the pre-paste human-input hold (#111): either the box is clear and
/// delivery may paste, or it never cleared and the delivery must abort. Carries
/// the held duration so the caller can audit how long it waited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteDecision {
    Paste { held_ms: u64 },
    Abort { held_ms: u64 },
}

/// Poll-and-hold loop that drives `resolve_paste_gate`: if a human's line is
/// sitting in the box (`box_pending`), block until they submit/clear it (the
/// flag flips false) or the bounded wait elapses, then return `Paste`/`Abort`.
/// Returns `Paste { held_ms: 0 }` immediately when the box is already clear.
///
/// Generic over the occupancy source and timings so the wiring — that the loop
/// re-reads the flag each poll and honours the abort cap — is integration-
/// testable without a live PTY (the #40 lesson: exercise the loop, not just the
/// pure decision).
#[doc(hidden)] // pub for integration tests
pub fn hold_for_human_input<P: Fn() -> bool>(
    box_pending: P,
    max_hold: Duration,
    poll: Duration,
) -> PasteDecision {
    if !box_pending() {
        return PasteDecision::Paste { held_ms: 0 };
    }
    let start = std::time::Instant::now();
    loop {
        let held = start.elapsed();
        match resolve_paste_gate(box_pending(), held, max_hold) {
            PasteGate::Paste => return PasteDecision::Paste { held_ms: held.as_millis() as u64 },
            PasteGate::Abort => return PasteDecision::Abort { held_ms: held.as_millis() as u64 },
            PasteGate::Hold => std::thread::sleep(poll),
        }
    }
}

/// Production wrapper: hold prompt delivery to `pty_id` while a human's line is
/// sitting in its input box, using the shipped cap / poll. A closed pty reads as
/// "not pending" so a dead pane never blocks the thread.
fn wait_for_box_clear(ptys: &crate::pty::PtyManager, pty_id: u32) -> PasteDecision {
    hold_for_human_input(
        || ptys.input_pending(pty_id).unwrap_or(false),
        HUMAN_INPUT_HOLD_MAX,
        HUMAN_INPUT_POLL,
    )
}

/// Whether an unconfirmed delivery should raise a one-shot notice to the group's
/// orchestrator so it can close the loop (#103). Fires only for a delivery to a
/// NON-orchestrator agent whose submit went unconfirmed: the prompt may be
/// sitting unsubmitted in the pane while the orchestrator, believing it landed,
/// is none the wiser. Suppressed when the target IS the orchestrator — a notice
/// about a delivery to the orchestrator would itself be a delivery to the
/// orchestrator, an endless loop; those rely on #99's stranded-text flush on the
/// next delivery instead. Suppressed when confirmed: the prompt landed, nothing
/// to chase. Pure so the gate is testable; emission (exactly once per delivery,
/// past the submit retries) lives in `deliver_prompt`'s delivery thread.
pub fn should_notify_unconfirmed(target_is_orchestrator: bool, confirmed: bool) -> bool {
    !target_is_orchestrator && !confirmed
}

/// The one-shot notice delivered to the orchestrator for an unconfirmed delivery
/// to `agent_id` (#103). Points it at the recovery move: read the pane back and
/// re-send if the prompt is stuck.
pub fn unconfirmed_delivery_notice(agent_id: &str) -> String {
    format!(
        "[loomux] delivery to {agent_id} unconfirmed — the prompt may be sitting \
         unsubmitted in its pane; get_output it and re-send if needed"
    )
}

/// Whether a held-for-human-input delivery should raise a notice to the group's
/// orchestrator (#111). Fires for a non-orchestrator target: the prompt was NOT
/// delivered (the box held a human's line, so pasting was aborted rather than
/// merge-submitting it), and the orchestrator — believing it landed — must know
/// to re-send once the pane is clear. Suppressed when the target IS the
/// orchestrator, exactly like the unconfirmed notice: a notice about a delivery
/// to the orchestrator is itself a delivery to the orchestrator, an endless
/// loop. Pure so the gate is testable; the paused-group skip and one-per-abort
/// emission live in `notify_delivery_held`.
pub fn should_notify_paste_held(target_is_orchestrator: bool) -> bool {
    !target_is_orchestrator
}

/// The notice delivered to the orchestrator when a delivery to `agent_id` was
/// held and aborted because the pane holds a human's unsubmitted line (#111).
/// Distinct from `unconfirmed_delivery_notice`: nothing was pasted, so the move
/// is to wait for the box to clear and re-send — not to read back a stranded
/// prompt.
pub fn paste_held_notice(agent_id: &str) -> String {
    format!(
        "[loomux] delivery to {agent_id} held: pane has human input — re-send when clear"
    )
}

impl OrchRegistry {
    pub fn new(root: PathBuf) -> Self {
        let _ = fs::create_dir_all(&root);
        Self {
            root,
            app: Mutex::new(None),
            groups: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            by_token: Mutex::new(HashMap::new()),
            by_pty: Mutex::new(HashMap::new()),
            pending_binds: Mutex::new(HashMap::new()),
            port: AtomicU16::new(0),
            seq: AtomicU32::new(0),
            delivery: Mutex::new(HashMap::new()),
            last_delivery: Arc::new(Mutex::new(HashMap::new())),
            tasks_lock: Mutex::new(()),
            creation: Mutex::new(()),
            pr_head_override: Mutex::new(None),
            paused: Mutex::new(HashSet::new()),
            spawn_times: Mutex::new(HashMap::new()),
            self_arc: Mutex::new(Weak::new()),
            attn_reports: Mutex::new(HashMap::new()),
            attn_quiet: Mutex::new(HashMap::new()),
            attn_waiting_ack: Mutex::new(HashSet::new()),
            attn_emitted: Mutex::new(HashMap::new()),
            notify_groups: Mutex::new(HashSet::new()),
            autonomous_groups: Mutex::new(HashSet::new()),
            auto_merge_groups: Mutex::new(HashSet::new()),
            auto_release_groups: Mutex::new(HashSet::new()),
            dangerous_groups: Mutex::new(HashSet::new()),
            idle_tick_times: Mutex::new(HashMap::new()),
            pending_max_notice: Mutex::new(HashMap::new()),
            claude_projects_dir: Mutex::new(None),
            low_disk_notified: Mutex::new(false),
            audit_skips_notified: Mutex::new(HashMap::new()),
        }
    }

    /// Point the usage reader at a specific Claude transcript root, instead of
    /// `~/.claude/projects`. Test-only seam (see `claude_projects_dir`).
    #[doc(hidden)]
    pub fn set_claude_projects_dir(&self, dir: PathBuf) {
        *self.claude_projects_dir.lock_safe() = Some(dir);
    }

    /// Record the `Arc` the registry is stored behind so `&self` methods can
    /// spawn background work that outlives the current call. Call once, right
    /// after wrapping the registry in an `Arc`.
    pub fn set_self_arc(self: &Arc<Self>) {
        *self.self_arc.lock_safe() = Arc::downgrade(self);
    }

    /// Upgrade the stored weak self-handle. `None` in unit tests that build a
    /// bare registry without calling `set_self_arc` — background helpers then
    /// simply don't run.
    fn arc(&self) -> Option<Arc<Self>> {
        self.self_arc.lock_safe().upgrade()
    }

    /// Default persistent root: `<user data dir>/loomux/orchestration`.
    pub fn default_root() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("loomux")
            .join("orchestration")
    }

    pub fn set_app(&self, app: AppHandle) {
        *self.app.lock_safe() = Some(app);
    }

    pub fn set_port(&self, port: u16) {
        self.port.store(port, Ordering::SeqCst);
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    fn group_dir(&self, group: &str) -> PathBuf {
        self.root.join(group)
    }

    /// Scratch dir holding images pasted/attached into the steering strip (#72).
    /// A subdir of the group state dir, so it's naturally per-group and swept
    /// on group end alongside the worktrees.
    fn attachments_dir(&self, group: &str) -> PathBuf {
        self.group_dir(group).join("attachments")
    }

    /// The CLI the group's orchestrator runs (`claude`/`copilot`/…), resolving
    /// per-role overrides through `cli_for`. Used to format image references the
    /// way that CLI consumes them (#72). Falls back to the default `claude`
    /// wording if the group isn't loaded (a save always follows a live steer, so
    /// this is just a safety net).
    pub fn orchestrator_cli(&self, group: &str) -> String {
        self.group(group)
            .map(|g| g.guardrails.cli_for(Role::Orchestrator).to_string())
            .unwrap_or_else(|| "claude".into())
    }

    /// Persist a steered image to the group's `attachments/` scratch dir and
    /// return its absolute path (#72). The steering strip can't hand binary to
    /// a CLI prompt, but Claude Code and Copilot both *read image files from
    /// paths* — so a pasted screenshot is written here and the steer text gains
    /// an "Attached image: <path>" line pointing at it. Bytes are written
    /// verbatim: the image arrives as a browser Blob and we never decode it
    /// (no image crate, no `getrandom` deps) — only size and extension are
    /// validated. Files are reclaimed when the group ends (see `end_group`).
    pub fn save_attachment(&self, group: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        // Membership guard: only ever write under a real, known group id (#72
        // review). The dir is `root.join(group)`, so without this a caller could
        // steer `group` to a traversal component; requiring the group to exist
        // pins it to a generated group token. Cheap hardening on top of the
        // pre-existing trusted-webview model (see the orch-command notes).
        if self.group(group).is_none() {
            return Err("unknown group".into());
        }
        if bytes.is_empty() {
            return Err("empty attachment".into());
        }
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "attachment too large ({} bytes, max {MAX_ATTACHMENT_BYTES})",
                bytes.len()
            ));
        }
        let ext = sanitize_attachment_ext(ext)
            .ok_or_else(|| format!("unsupported attachment type: {ext:?}"))?;
        let dir = self.attachments_dir(group);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // `<ms>-<seq>.<ext>`: wall-clock time keeps names sortable/legible while
        // the process-local sequence disambiguates a same-millisecond burst.
        let name = format!("{}-{}.{ext}", now_ms(), ATTACH_SEQ.fetch_add(1, Ordering::Relaxed));
        let path = dir.join(name);
        fs::write(&path, bytes).map_err(|e| e.to_string())?;
        self.audit(group, "human", "attachment-save",
            json!({ "path": path.display().to_string(), "bytes": bytes.len() }));
        Ok(path)
    }

    // ---------- audit ----------

    /// Append one JSON line to the group's audit log. Best-effort: auditing
    /// must never take the orchestration down.
    pub fn audit(&self, group: &str, actor: &str, action: &str, detail: Value) {
        append_audit(&self.root, group, actor, action, detail);
    }

    /// Read a group's audit timeline for the in-app viewer, oldest first.
    /// Reads the rotated generation (`audit.1.jsonl`) before the current one
    /// so a rotation doesn't drop history mid-session, then keeps only the
    /// most recent `AUDIT_VIEW_LIMIT` entries. Missing files read as empty.
    ///
    /// Unreadable lines are still skipped — a log with a torn record must not
    /// blank the viewer — but they are no longer skipped *silently*: the count
    /// goes to the breadcrumb log (#240). A non-zero count now means a writer
    /// is not appending whole lines, which is a bug worth seeing rather than a
    /// timeline that quietly comes up short.
    pub fn audit_log(&self, group: &str) -> Vec<AuditEntry> {
        let dir = self.group_dir(group);
        let mut text = String::new();
        for name in ["audit.1.jsonl", "audit.jsonl"] {
            if let Ok(t) = fs::read_to_string(dir.join(name)) {
                text.push_str(&t);
                if !text.ends_with('\n') {
                    text.push('\n'); // guard against a rotated file with no trailing newline
                }
            }
        }
        let (mut entries, skipped) = parse_audit_lines_counted(&text);
        if skipped > 0 && self.audit_skips_notified.lock_safe().insert(group.to_string(), skipped) != Some(skipped) {
            // Only on a change: follow mode re-polls this, and a pre-fix log
            // keeps its torn lines forever (see `audit_skips_notified`).
            crate::obs::breadcrumb("audit-lines-unreadable", &format!("group={group} skipped={skipped}"));
        }
        if entries.len() > AUDIT_VIEW_LIMIT {
            entries.drain(0..entries.len() - AUDIT_VIEW_LIMIT);
        }
        entries
    }

    // ---------- durable state ----------

    pub fn get_state(&self, group: &str) -> String {
        fs::read_to_string(self.group_dir(group).join("state.json"))
            .unwrap_or_else(|_| "{}".to_string())
    }

    pub fn set_state(&self, group: &str, state: &str) -> Result<(), String> {
        if state.len() > MAX_STATE_BYTES {
            return Err(format!("state too large ({} bytes, max {MAX_STATE_BYTES})", state.len()));
        }
        serde_json::from_str::<Value>(state).map_err(|e| format!("state must be valid JSON: {e}"))?;
        let dir = self.group_dir(group);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Atomic replace so a failed write (disk-full, crash) leaves the last
        // good snapshot intact (#133). set_state holds no lock — the unique
        // temp name in `atomic_write` keeps concurrent writers from clobbering
        // one another's scratch file, and the rename makes it last-writer-wins
        // rather than a torn file.
        atomic_write(&dir.join("state.json"), state.as_bytes()).map_err(|e| e.to_string())?;
        self.audit(group, "loomux", "state-write", json!({ "bytes": state.len() }));
        Ok(())
    }

    // ---------- task board ----------

    pub fn tasks(&self, group: &str) -> Vec<Task> {
        fs::read_to_string(self.group_dir(group).join("tasks.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write_tasks(&self, group: &str, tasks: &[Task]) -> Result<(), String> {
        let dir = self.group_dir(group);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Atomic replace: the incident that filed #133 was a disk-full
        // `fs::write` here that truncated tasks.json and destroyed the live
        // board. All callers hold `tasks_lock`, so writes are serialized.
        let body = serde_json::to_string_pretty(tasks).unwrap();
        atomic_write(&dir.join("tasks.json"), body.as_bytes()).map_err(|e| e.to_string())?;
        self.emit_tasks_changed(group);
        Ok(())
    }

    fn emit_tasks_changed(&self, group: &str) {
        if let Some(app) = self.app.lock_safe().clone() {
            let _ = app.emit("orch-tasks-changed", json!({ "group_id": group }));
        }
    }

    /// Create (id = None, title required) or edit a task. Notes append; all
    /// other patch fields replace. Returns the resulting task.
    pub fn upsert_task(
        &self,
        group: &str,
        actor: &str,
        id: Option<&str>,
        patch: TaskPatch,
    ) -> Result<Task, String> {
        if let Some(s) = patch.status.as_deref() {
            if !TASK_STATUSES.contains(&s) {
                return Err(format!("invalid status {s:?} — use one of {}", TASK_STATUSES.join(" | ")));
            }
        }
        let _guard = self.tasks_lock.lock_safe();
        let mut tasks = self.tasks(group);
        let idx = match id {
            Some(id) => Some(
                tasks
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or_else(|| format!("unknown task: {id}"))?,
            ),
            None => None,
        };
        let task = match idx {
            Some(i) => &mut tasks[i],
            None => {
                let title = patch
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .ok_or("a new task needs a title")?;
                let max: u32 = tasks
                    .iter()
                    .filter_map(|t| t.id.strip_prefix("t-").and_then(|n| n.parse().ok()))
                    .max()
                    .unwrap_or(0);
                tasks.push(Task {
                    id: format!("t-{}", max + 1),
                    title: title.to_string(),
                    status: "queued".into(),
                    issue: None,
                    pr: None,
                    assignee: None,
                    session: None,
                    notes: vec![],
                    updated_ms: 0,
                });
                tasks.last_mut().unwrap()
            }
        };
        if let Some(t) = patch.title {
            let t = t.trim();
            if !t.is_empty() {
                task.title = t.to_string();
            }
        }
        if let Some(s) = patch.status {
            task.status = s;
        }
        if patch.issue.is_some() {
            task.issue = patch.issue.filter(|s| !s.trim().is_empty());
        }
        if patch.pr.is_some() {
            task.pr = patch.pr.filter(|s| !s.trim().is_empty());
        }
        if patch.assignee.is_some() {
            task.assignee = patch.assignee.filter(|s| !s.trim().is_empty());
        }
        if patch.session.is_some() {
            task.session = patch.session.filter(|s| !s.trim().is_empty());
        }
        if let Some(text) = patch.note {
            let text = text.trim().to_string();
            if !text.is_empty() {
                task.notes.push(TaskNote { ts_ms: now_ms(), author: actor.to_string(), text });
            }
        }
        task.updated_ms = now_ms();
        let snapshot = task.clone();
        self.write_tasks(group, &tasks)?;
        self.audit(group, actor, "task-upsert", serde_json::to_value(&snapshot).unwrap());
        Ok(snapshot)
    }

    pub fn delete_task(&self, group: &str, actor: &str, id: &str) -> Result<(), String> {
        let _guard = self.tasks_lock.lock_safe();
        let mut tasks = self.tasks(group);
        let before = tasks.len();
        tasks.retain(|t| t.id != id);
        if tasks.len() == before {
            return Err(format!("unknown task: {id}"));
        }
        self.write_tasks(group, &tasks)?;
        self.audit(group, actor, "task-delete", json!({ "id": id }));
        Ok(())
    }

    /// Delete every task in the terminal `done` status in a single board write,
    /// returning the ids removed (empty if none were done). The board's "delete
    /// all done" action routes through here so the batch surfaces to the
    /// orchestrator as ONE board-change notice, not one per task (#120) — the
    /// coalesced notice is emitted here (best-effort), so callers must not fan
    /// out per-task notices. A no-op (nothing done) writes nothing and notifies
    /// nothing.
    pub fn delete_done_tasks(&self, group: &str, actor: &str) -> Result<Vec<String>, String> {
        let removed = {
            let _guard = self.tasks_lock.lock_safe();
            let mut tasks = self.tasks(group);
            let removed: Vec<String> =
                tasks.iter().filter(|t| t.status == "done").map(|t| t.id.clone()).collect();
            if removed.is_empty() {
                return Ok(removed);
            }
            tasks.retain(|t| t.status != "done");
            self.write_tasks(group, &tasks)?;
            self.audit(group, actor, "task-delete-done", json!({ "ids": removed }));
            removed
        };
        // Outside the tasks lock: notify is best-effort and can block on delivery.
        let n = removed.len();
        self.notify_board_edit(
            group,
            &format!("deleted {n} done task{}", if n == 1 { "" } else { "s" }),
        );
        Ok(removed)
    }

    /// Delete a specific set of tasks by id in a single board write, returning
    /// the ids actually removed (a subset of `ids`, in board order). Backs the
    /// board's multi-select "delete selected" action and mirrors
    /// `delete_done_tasks`: the whole batch surfaces to the orchestrator as ONE
    /// board-change notice (#120), emitted here (best-effort), so callers must
    /// not fan out per-task notices. Ids not on the board are skipped, not
    /// errored — the board can change under the human's selection (the
    /// orchestrator or a batch may have removed a row since they clicked) — and
    /// the skipped ids are recorded in the audit entry. An empty selection, or
    /// one matching nothing, writes nothing and notifies nothing.
    pub fn delete_tasks(&self, group: &str, actor: &str, ids: &[String]) -> Result<Vec<String>, String> {
        let removed = {
            let _guard = self.tasks_lock.lock_safe();
            let mut tasks = self.tasks(group);
            let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
            let removed: Vec<String> =
                tasks.iter().filter(|t| wanted.contains(t.id.as_str())).map(|t| t.id.clone()).collect();
            if removed.is_empty() {
                return Ok(removed);
            }
            // Ids the human selected that no longer name a board row. Skipped,
            // not fatal — but audited, so the divergence is traceable.
            let present: HashSet<&str> = removed.iter().map(String::as_str).collect();
            let skipped: Vec<&str> = ids.iter().map(String::as_str).filter(|id| !present.contains(id)).collect();
            tasks.retain(|t| !wanted.contains(t.id.as_str()));
            self.write_tasks(group, &tasks)?;
            self.audit(group, actor, "task-delete-selected", json!({ "ids": removed, "skipped": skipped }));
            removed
        };
        // Outside the tasks lock: notify is best-effort and can block on delivery.
        let n = removed.len();
        self.notify_board_edit(
            group,
            &format!("deleted {n} selected task{}", if n == 1 { "" } else { "s" }),
        );
        Ok(removed)
    }

    /// Reorder by explicit id list (board order = priority). Ids not
    /// mentioned keep their relative order after the mentioned ones.
    pub fn reorder_tasks(&self, group: &str, actor: &str, ids: &[String]) -> Result<(), String> {
        let _guard = self.tasks_lock.lock_safe();
        let mut tasks = self.tasks(group);
        let mut ordered: Vec<Task> = Vec::with_capacity(tasks.len());
        for id in ids {
            if let Some(pos) = tasks.iter().position(|t| &t.id == id) {
                ordered.push(tasks.remove(pos));
            }
        }
        ordered.append(&mut tasks);
        self.write_tasks(group, &ordered)?;
        self.audit(group, actor, "task-reorder", json!({ "order": ids }));
        Ok(())
    }

    /// Guard the merge-gate actions to items actually at the gate. The UI only
    /// shows the buttons on `pr`/`human-testing` items, but the command surface
    /// is callable directly, so enforce it backend-side too — approving a
    /// `queued` item or requesting changes on a `done` one is meaningless.
    fn ensure_at_merge_gate(&self, group: &str, id: &str) -> Result<(), String> {
        let status = self
            .tasks(group)
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("unknown task: {id}"))?
            .status;
        if MERGE_GATE_STATUSES.contains(&status.as_str()) {
            Ok(())
        } else {
            Err(format!(
                "task {id} is {status:?}, not at the merge gate — this action only applies to {}",
                MERGE_GATE_STATUSES.join(" | ")
            ))
        }
    }

    /// Merge-gate approve: mark the item done and issue a **one-time merge grant**
    /// for its PR so the orchestrator can actually merge (the enforced gate blocks
    /// a default-branch merge without a grant — clicking Approve without one leaves
    /// the orchestrator stuck, #83). The status change is the human's direct
    /// sign-off; `comment` is an optional approve-with-comment note delivered with
    /// the grant. When the task has no resolvable PR number, no grant is written and
    /// a plain approval notice is delivered instead (the human merges by hand).
    pub fn approve_task(&self, group: &str, id: &str, comment: Option<&str>) -> Result<Task, String> {
        self.ensure_at_merge_gate(group, id)?;
        let task = self.upsert_task(
            group,
            "human",
            Some(id),
            TaskPatch {
                status: Some("done".into()),
                note: Some("Approved at the merge gate.".into()),
                ..Default::default()
            },
        )?;
        // Grant the one-time merge for this PR (delivers the authorization + any
        // comment to the orchestrator). Falls back to a plain notice if the task
        // carries no PR number to key the grant on.
        let granted = task
            .pr
            .as_deref()
            .and_then(|pr| self.grant_merge(group, pr, comment, "human").ok());
        if granted.is_none() {
            let pr = task.pr.as_deref().unwrap_or("(no PR ref)");
            let extra = comment
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(|c| format!(" Note from the human: {c}"))
                .unwrap_or_default();
            let _ = self.deliver_to_orchestrator(
                group,
                &format!(
                    "[loomux] the human APPROVED {} \"{}\" ({}) at the merge gate and marked it done. \
                     Merge the PR and close out the work item.{extra}",
                    task.id, task.title, pr
                ),
                "human",
            );
        }
        Ok(task)
    }

    /// Merge-gate request-changes: record the findings as a note and deliver
    /// them to the orchestrator to route back to a worker. Status is left for
    /// the orchestrator to manage as it re-dispatches.
    pub fn request_changes(&self, group: &str, id: &str, findings: &str) -> Result<Task, String> {
        let findings = findings.trim();
        if findings.is_empty() {
            return Err("request changes needs a note describing what to fix".into());
        }
        self.ensure_at_merge_gate(group, id)?;
        let task = self.upsert_task(
            group,
            "human",
            Some(id),
            TaskPatch { note: Some(format!("Requested changes: {findings}")), ..Default::default() },
        )?;
        let pr = task.pr.as_deref().unwrap_or("(no PR ref)");
        let _ = self.deliver_to_orchestrator(
            group,
            &format!(
                "[loomux] the human REQUESTED CHANGES on {} \"{}\" ({}) at the merge gate. \
                 Findings: {findings}. Route it back to a worker to address, then re-request review.",
                task.id, task.title, pr
            ),
            "human",
        );
        Ok(task)
    }

    /// Guard the start action to items that are actually queued. The UI only
    /// shows the button on `queued` items, but the command surface is callable
    /// directly, so enforce it backend-side too — starting an in-progress or
    /// done item is meaningless.
    fn ensure_queued(&self, group: &str, id: &str) -> Result<(), String> {
        let status = self
            .tasks(group)
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("unknown task: {id}"))?
            .status;
        if status == "queued" {
            Ok(())
        } else {
            Err(format!("task {id} is {status:?}, not queued — only a queued task can be started"))
        }
    }

    /// Start a queued item: record a human-attributed note and tell the
    /// orchestrator to begin work on it now. Deliberately does NOT flip the
    /// status — the orchestrator moves it to `in-progress` when it actually
    /// assigns a worker, so the board reflects real assignment rather than
    /// intent. The notice is best-effort (the board is the source of truth).
    ///
    /// A paused group is rejected up front (mirroring `steer_orchestrator`):
    /// its delivery is silently suppressed and queued prompts aren't replayed
    /// on resume, so without this guard the nudge would vanish — with a note
    /// left behind implying it landed. Reject before any mutation so no note is
    /// appended, and let the human resume first.
    pub fn start_task(&self, group: &str, id: &str) -> Result<Task, String> {
        self.ensure_queued(group, id)?;
        if self.is_paused(group) {
            return Err("group is paused — resume before starting tasks".into());
        }
        let task = self.upsert_task(
            group,
            "human",
            Some(id),
            TaskPatch {
                note: Some("Started by the human — asked the orchestrator to begin work.".into()),
                ..Default::default()
            },
        )?;
        let _ = self.deliver_to_orchestrator(
            group,
            &format!(
                "[loomux] the human started task {} (\"{}\") — begin work on it now.",
                task.id, task.title
            ),
            "human",
        );
        Ok(task)
    }

    /// Guard the proceed action to items actually in `prototype`. The UI only
    /// shows the button on prototype items, but the command surface is callable
    /// directly, so enforce it backend-side too (constraint 6) — "proceeding" a
    /// queued or done item is meaningless.
    fn ensure_prototype(&self, group: &str, id: &str) -> Result<(), String> {
        let status = self
            .tasks(group)
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("unknown task: {id}"))?
            .status;
        if status == PROTOTYPE_STATUS {
            Ok(())
        } else {
            Err(format!(
                "task {id} is {status:?}, not {PROTOTYPE_STATUS:?} — Proceed only applies to a prototype"
            ))
        }
    }

    /// Proceed on a prototype (#147): the human has validated the demo and wants
    /// it promoted to a full production build. Flips `prototype` → `in-progress`
    /// (the item is back in active development, no longer parked on the human's
    /// verdict), records a human-attributed note, and delivers ONE typed notice
    /// telling the orchestrator to run the promotion. Unlike `start_task`, the
    /// status flip is durable — like `approve_task`, the board carries the
    /// decision even if a paused group drops the notice — so this does NOT reject
    /// on pause (the orchestrator sees the flip + note on resume via list_tasks).
    /// The notice is best-effort (the board is the source of truth).
    pub fn proceed_task(&self, group: &str, id: &str) -> Result<Task, String> {
        self.ensure_prototype(group, id)?;
        let task = self.upsert_task(
            group,
            "human",
            Some(id),
            TaskPatch {
                status: Some("in-progress".into()),
                note: Some(
                    "Proceed — the human validated the prototype; promote it to a full production build."
                        .into(),
                ),
                ..Default::default()
            },
        )?;
        let _ = self.deliver_to_orchestrator(
            group,
            &format!(
                "[loomux] the human clicked PROCEED on task {} (\"{}\") — the prototype is validated. \
                 Promote it to a full production build: production hardening + full reviews, no corners, \
                 the same promotion arc you'd run by hand.",
                task.id, task.title
            ),
            "human",
        );
        Ok(task)
    }

    /// Tell the orchestrator the human touched the board (best-effort; the
    /// board itself is the source of truth via list_tasks).
    fn notify_board_edit(&self, group: &str, summary: &str) {
        let _ = self.deliver_to_orchestrator(
            group,
            &format!("[loomux] the human updated the task board: {summary}. Call list_tasks to sync."),
            "human",
        );
    }

    // ---------- durable roster (session ↔ role mapping, resume) ----------

    /// Upsert an agent into the group's `agents.json`. Best-effort like the
    /// audit log; shares the file lock with the task board.
    fn persist_agent_record(&self, entry: &AgentEntry, status: &str) {
        let _guard = self.tasks_lock.lock_safe();
        let path = self.group_dir(&entry.group).join("agents.json");
        let mut list: Vec<AgentRecord> = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let record = AgentRecord {
            id: entry.id.clone(),
            role: entry.role.as_str().into(),
            block: entry.block.clone(),
            name: entry.name.clone(),
            name_source: entry.name_source,
            session: entry.session_id.clone(),
            cwd: entry.cwd.clone(),
            status: status.to_string(),
            updated_ms: now_ms(),
        };
        // Match by (id, session): agent ids restart at 1 every app run, so
        // a bare-id match would overwrite a previous run's record and lose
        // that session's identity. A session-bearing record also supersedes
        // this run's placeholder for the same id — copilot writes an entry
        // with no session at spawn, then upgrades it once its session id is
        // discovered (only placeholders have session == None).
        match list.iter_mut().find(|r| {
            r.id == record.id && (r.session == record.session || r.session.is_none())
        }) {
            Some(r) => *r = record,
            None => list.push(record),
        }
        let _ = fs::create_dir_all(self.group_dir(&entry.group));
        // Atomic replace so a failed write can't wipe the agent roster (#133).
        // Holds `tasks_lock` (taken above), so writes are serialized.
        let body = serde_json::to_string_pretty(&list).unwrap();
        let _ = atomic_write(&path, body.as_bytes());
    }

    fn group_records(&self, group: &str) -> Vec<AgentRecord> {
        fs::read_to_string(self.group_dir(group).join("agents.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Poll `~/.copilot/session-state` for the session the just-spawned
    /// copilot pane created (the one absent from `baseline`) and bind its id
    /// to the pane. Runs on its own thread — copilot writes the session a few
    /// seconds into boot. Gives up after `COPILOT_SESSION_TIMEOUT`.
    fn spawn_copilot_session_watcher(
        self: Arc<Self>,
        agent_id: String,
        group_id: String,
        cwd: String,
        baseline: HashSet<String>,
    ) {
        let Some(root) = crate::sessions::copilot_session_state_root() else {
            return;
        };
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + COPILOT_SESSION_TIMEOUT;
            loop {
                std::thread::sleep(COPILOT_SESSION_POLL);
                // Stop if the pane died or was already associated (a resume
                // re-spawn, or a manual edit) — nothing left to track.
                match self.agent(&agent_id) {
                    Some(a) if a.status == AgentStatus::Dead => return,
                    Some(a) if a.session_id.is_some() => return,
                    Some(_) => {}
                    None => return,
                }
                if let Some(sid) =
                    crate::sessions::newest_new_copilot_session(&root, &baseline, &cwd)
                {
                    self.associate_copilot_session(&group_id, &agent_id, &sid);
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    self.audit(&group_id, "loomux", "copilot-session-untracked",
                        json!({ "agent": agent_id, "reason": "no new session-state appeared before timeout" }));
                    return;
                }
            }
        });
    }

    /// Bind a discovered copilot session id to a live pane: update the agent
    /// map, the durable roster (`agents.json`), and any task board item this
    /// agent owns — the same session trail Claude gets at spawn. Best-effort.
    /// Public for the session watcher and its tests; a no-op if the pane is
    /// gone or already carries a session id.
    pub fn associate_copilot_session(&self, group_id: &str, agent_id: &str, session_id: &str) {
        let entry = {
            let mut agents = self.agents.lock_safe();
            let Some(a) = agents.get_mut(agent_id) else { return };
            // Don't clobber an id set in the meantime (e.g. a resume).
            if a.session_id.is_some() {
                return;
            }
            a.session_id = Some(session_id.to_string());
            a.clone()
        };
        let status = match entry.status {
            AgentStatus::Dead => "dead",
            _ => "running",
        };
        self.persist_agent_record(&entry, status);
        // Mirror onto the task board: any item this agent owns (by id or
        // display name) that lacks a session gets it, so the orchestrator can
        // resume the task later without hunting the id out of list_agents.
        {
            let _guard = self.tasks_lock.lock_safe();
            let mut tasks = self.tasks(group_id);
            let mut changed = false;
            for t in tasks.iter_mut() {
                let owner = t.assignee.as_deref().unwrap_or("");
                if t.session.is_none() && (owner == entry.id || owner == entry.name) {
                    t.session = Some(session_id.to_string());
                    t.updated_ms = now_ms();
                    changed = true;
                }
            }
            if changed {
                let _ = self.write_tasks(group_id, &tasks);
            }
        }
        self.audit(group_id, "loomux", "copilot-session",
            json!({ "agent": agent_id, "session": session_id }));
    }

    /// Roster entries derived from `agent-spawn` audit lines. Backfill for
    /// groups created before agents.json existed — their session-to-role
    /// mapping lives only in the audit log.
    fn records_from_audit(&self, group: &str) -> Vec<AgentRecord> {
        // Oldest first so newer spawns win the (id, session) upsert; the
        // rotated generation holds the older entries.
        let mut text = String::new();
        for name in ["audit.1.jsonl", "audit.jsonl"] {
            if let Ok(t) = fs::read_to_string(self.group_dir(group).join(name)) {
                text.push_str(&t);
            }
        }
        let mut out: Vec<AgentRecord> = Vec::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            if v["action"] != "agent-spawn" {
                continue;
            }
            let d = &v["detail"];
            let Some(session) = d["session"].as_str() else { continue };
            let role = d["role"].as_str().unwrap_or("worker").to_string();
            let record = AgentRecord {
                id: d["agent"].as_str().unwrap_or("").to_string(),
                name: d["name"]
                    .as_str()
                    .unwrap_or(if role == "orchestrator" { "orchestrator" } else { "agent" })
                    .to_string(),
                // The spawn audit predates the name-tier field; backfilled
                // sessions restore at the default tier (#95r).
                name_source: NameSource::default(),
                // Blocks (#222) are recorded in the spawn audit; an audit line
                // from an older build has none, and the rejoin then falls back
                // to the class's default block.
                block: d["block"].as_str().unwrap_or("").to_string(),
                role,
                session: Some(session.to_string()),
                cwd: d["cwd"].as_str().unwrap_or("").to_string(),
                // The audit alone can't tell liveness; group_live covers it.
                status: "unknown".into(),
                updated_ms: v["ts_ms"].as_u64().unwrap_or(0),
            };
            match out.iter_mut().find(|r| r.id == record.id && r.session == record.session) {
                Some(r) => *r = record,
                None => out.push(record),
            }
        }
        out
    }

    /// Roster + audit backfill, deduped by session (roster wins). Sessions
    /// are the stable key; agent ids recycle across app runs.
    fn merged_records(&self, group: &str) -> Vec<AgentRecord> {
        let mut records = self.group_records(group);
        for r in self.records_from_audit(group) {
            let dup = records.iter().any(|x| match (&x.session, &r.session) {
                (Some(a), Some(b)) => a == b,
                _ => x.id == r.id,
            });
            if !dup {
                records.push(r);
            }
        }
        records
    }

    /// Every recorded session across all groups on disk, with role identity
    /// — drives the session browser's ORCH/W/REV badges and restore flow.
    pub fn session_roles(&self) -> Vec<SessionRole> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return out;
        };
        for e in entries.flatten() {
            let group_id = e.file_name().to_string_lossy().into_owned();
            if !e.path().join("group.json").is_file() {
                continue;
            }
            let live = self.group_is_live(&group_id);
            for r in self.merged_records(&group_id) {
                if let Some(session) = r.session {
                    out.push(SessionRole {
                        session_id: session,
                        group_id: group_id.clone(),
                        role: r.role,
                        agent_name: r.name,
                        group_live: live,
                    });
                }
            }
        }
        out
    }

    /// Load a group's persisted identity (repo + guardrails) from group.json.
    ///
    /// See [`read_blocks`] for how a pre-#222 group.json (flat per-role fields,
    /// no `blocks` array) is migrated to the block roster on read.
    #[doc(hidden)] // pub for integration tests (the #222 migration is asserted on this)
    pub fn load_group_file(&self, group: &str) -> Option<(String, Guardrails)> {
        let v: Value =
            serde_json::from_str(&fs::read_to_string(self.group_dir(group).join("group.json")).ok()?).ok()?;
        let repo = v["repo"].as_str()?.to_string();
        let g = &v["guardrails"];
        let s = |k: &str, fb: &str| g[k].as_str().unwrap_or(fb).to_string();
        Some((
            repo,
            Guardrails {
                max_agents: g["max_agents"].as_u64().unwrap_or(4) as u32,
                agent_cli: s("agent_cli", "claude"),
                // The roster (#222). A group.json written by an older loomux has
                // no `blocks` array — only the eight flat per-role fields — so
                // rebuild the equivalent 4-block roster from those. That is the
                // whole migration: a pre-#222 group rejoins with exactly the CLIs
                // and models it was launched with.
                blocks: read_blocks(g),
                // The advanced-orchestrator toggle (#222). Absent → false: a
                // group launched before the toggle existed ran the built-in
                // roster, so that is what it rejoins as. This is also what makes
                // the toggle durable — a resumed orchestration (session browser)
                // rebuilds its guardrails from here, not from a launcher form.
                advanced_orchestrator: g["advanced_orchestrator"].as_bool().unwrap_or(false),
                auto_ops: g["auto_ops"].as_bool().unwrap_or(true),
                idle_kill_minutes: g["idle_kill_minutes"].as_u64().unwrap_or(0) as u32,
                max_spawns_per_hour: g["max_spawns_per_hour"].as_u64().unwrap_or(0) as u32,
                watchdog_stall_minutes: g["watchdog_stall_minutes"].as_u64().unwrap_or(0) as u32,
                // Autonomous token budget (#83) is a durable human choice, like
                // max_agents: absent in older group.json → 0 (no cap).
                autonomy_budget_tokens: g["autonomy_budget_tokens"].as_u64().unwrap_or(0),
                // Idle-tick window (#83): absent → 0 → clamped() maps to the default.
                idle_tick_minutes: g["idle_tick_minutes"].as_u64().unwrap_or(0) as u32,
                // Idle-tick activity floor (#83): absent → 0 → clamped() → default.
                idle_activity_floor_bytes: g["idle_activity_floor_bytes"].as_u64().unwrap_or(0),
            },
        ))
    }

    // ---------- groups & agents ----------

    /// Record that a resumed group's pinned roster no longer matches what the
    /// repo's workflow file now says (#222, rev-11 F2). Audit only — the pinned
    /// roster is what runs, deliberately.
    ///
    /// The comparison is against the roster the file *would resolve to* (same
    /// `clamped()` a launch applies), not against its raw blocks, so an inherited
    /// model filled in at launch doesn't read as drift. A file that has since been
    /// deleted or broken is drift too: the group is running blocks its repo no
    /// longer declares, which is exactly the thing worth being able to see.
    fn audit_workflow_drift(&self, id: &str, repo: &str, g: &Guardrails) {
        let ids = |bs: &[workflow::Block]| -> Vec<String> { bs.iter().map(|b| b.id.clone()).collect() };
        let (now, note) = match workflow::load_workflow(repo) {
            Ok(Some(wf)) => {
                let resolved = Guardrails {
                    agent_cli: g.agent_cli.clone(),
                    blocks: wf.blocks,
                    ..Guardrails::default()
                }
                .clamped()
                .blocks;
                if resolved == g.blocks {
                    return; // the file still says what the group is running
                }
                // "Appeared" and "changed" are different events to a human reading
                // the trail, and only one of them means "somebody edited the file
                // you approved". A group whose running roster is the built-in four
                // was launched without a workflow in play at all — so the repo has
                // *gained* one since, and this group is simply not running it.
                let note = if workflow::roster_is_custom(&g.blocks) {
                    "the file has changed since this group was launched"
                } else {
                    "the repo has gained a workflow file since this group was launched"
                };
                (ids(&resolved), note)
            }
            Ok(None) if !workflow::roster_is_custom(&g.blocks) => return, // no file, no workflow: nothing to drift from
            Ok(None) => (Vec::new(), "the file the group was launched from is gone"),
            Err(_) => (Vec::new(), "the file no longer validates"),
        };
        self.audit(id, "loomux", "workflow-changed-since-launch", json!({
            "path": workflow::WORKFLOW_PATH,
            "note": note,
            "running": ids(&g.blocks),
            "on_disk": now,
            "action": "keeping the roster this group was launched with — relaunch to pick up the new one",
        }));
    }

    /// Create (or reattach to) the group for `repo`. State and audit history
    /// persist under the repo-derived group id; guardrails are refreshed from
    /// the new launch.
    ///
    /// A **fresh launch** — the human is at the launcher, and has just been shown
    /// what the advanced orchestrator would run. [`create_group_ex`] is the same
    /// thing for a resumed orchestrator session, which is a different question.
    ///
    /// [`create_group_ex`]: Self::create_group_ex
    pub fn create_group(&self, repo: &str, guardrails: Guardrails) -> Result<GroupInfo, String> {
        self.create_group_ex(repo, guardrails, Launch::Fresh)
    }

    /// [`create_group`](Self::create_group), told which kind of start this is.
    ///
    /// The distinction only matters for the advanced orchestrator (#222), and it
    /// matters a lot: **a resumed group runs the roster it was launched with.**
    /// See [`Launch`].
    #[doc(hidden)] // pub for integration tests (the resume pin is asserted on this)
    pub fn create_group_ex(
        &self,
        repo: &str,
        guardrails: Guardrails,
        launch: Launch,
    ) -> Result<GroupInfo, String> {
        let mut guardrails = guardrails.clamped();
        // Base id is repo-derived so a relaunch resumes the same state dir —
        // but a repo can host several *concurrent* orchestrations, and those
        // must never share a group (their orchestrators would receive each
        // other's worker reports). Take the first id without live agents.
        let base = group_id_for_repo(repo);
        let id = (1..)
            .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
            .find(|candidate| !self.group_is_live(candidate))
            .unwrap();
        let dir = self.group_dir(&id);
        fs::create_dir_all(dir.join("configs")).map_err(|e| e.to_string())?;
        let resumed = dir.join("group.json").is_file();

        // The repo's declared roster AND its merge gate (#222), read ONLY when the
        // human turned the advanced orchestrator on **and this is a fresh launch**.
        //
        // With the toggle off — the default — the file is not even opened: this is
        // the whole promise that the default experience is byte-for-byte what it
        // was before #222, and the cheapest way to keep that promise is to not have
        // a code path.
        //
        // On a RESUME the file is not read for the roster either, and that is a
        // consent rule, not an optimization (rev-11 F2). The roster in `group.json`
        // is the one the human was shown in the launcher preview and approved. A
        // `git pull` between launch and resume must not be able to swap a delegate's
        // persona under a session they already consented to — the consent moment is
        // the launch, so the launch is what the roster is pinned to. Drift is
        // *audited* below, not applied; to run a changed workflow, launch a group.
        //
        // #255: set only on a fresh, valid workflow load — the one moment this
        // function actually knows the roster's structural agent requirement.
        // Checked against the resolved `max_agents` once every guardrail
        // override below (including the resume-cap override) has landed.
        let mut capacity: Option<workflow::CapacityRecommendation> = None;
        if guardrails.advanced_orchestrator && launch == Launch::Fresh {
            // Three outcomes, and only the first changes anything:
            //   - a valid `.loomux/workflow.yml` → its blocks ARE the roster;
            //   - no file → the launcher's 4-block roster stands (turning the
            //     toggle on in a repo that declares nothing is a no-op, not an
            //     error — it is how you launch before you write the file);
            //   - a broken file → AUDITED AND SKIPPED. A repo file must never be
            //     able to stop a group from launching, so a validation failure
            //     falls back to the default roster rather than erroring out.
            //     Every problem is recorded, not just the first, so one look at
            //     the audit log fixes the file in one pass. The launcher shows
            //     the human the same findings *before* they hit Create.
            match workflow::load_workflow(repo) {
                Ok(Some(wf)) => {
                    // #255: derived from the roster + the (optional) merge gate
                    // while we still hold both — recorded here so a run's capacity
                    // assumptions are reconstructable from the audit log later, and
                    // checked against the resolved cap below.
                    let capacity_rec =
                        workflow::recommend_capacity(&wf.blocks, wf.gates.get("merge"));
                    self.audit(&id, "loomux", "workflow-loaded", json!({
                        "path": workflow::WORKFLOW_PATH,
                        "name": wf.name,
                        "blocks": wf.blocks.iter().map(|b| json!({ "id": b.id, "kind": b.kind })).collect::<Vec<_>>(),
                        "gates": wf.gates.keys().collect::<Vec<_>>(),
                        "min_agents": capacity_rec.minimum,
                        "recommended_agents": capacity_rec.recommended,
                    }));
                    // The declared merge gate (#222/#197) becomes the `merge_gate`
                    // spec file the gh shim enforces — or, when the file declares
                    // none, is cleared, so removing a gate from the workflow really
                    // removes it.
                    self.sync_merge_gate(&id, wf.gates.get("merge"));
                    // `merge` is the only gate loomux enforces. A gate under any other
                    // name parses (the schema is open) but does nothing — say so, rather
                    // than letting a `gates: { deploy: … }` clause look enforced.
                    let unenforced: Vec<&String> =
                        wf.gates.keys().filter(|k| k.as_str() != "merge").collect();
                    if !unenforced.is_empty() {
                        self.audit(&id, "loomux", "workflow-gate-unenforced", json!({
                            "gates": unenforced,
                            "note": "only gates.merge is enforced by this build — the rest are inert",
                        }));
                    }
                    guardrails.blocks = wf.blocks;
                    // Re-run the roster normalization (model defaults follow each
                    // block's *effective* CLI, which the file may have changed).
                    guardrails = guardrails.clamped();
                    capacity = Some(capacity_rec);
                }
                // No workflow file: no gate. Clears a stale one from a previous
                // launch, so deleting `.loomux/workflow.yml` restores the pre-#222
                // flow exactly.
                Ok(None) => self.sync_merge_gate(&id, None),
                Err(errors) => {
                    self.audit(&id, "loomux", "workflow-invalid", json!({
                        "path": workflow::WORKFLOW_PATH,
                        "errors": errors,
                        "action": "skipped — using the built-in roster",
                    }));
                    // A BROKEN workflow file does NOT clear an existing gate. The
                    // roster can safely fall back to the built-in one — every agent
                    // still spawns, which is #225's "a repo file must never block a
                    // launch". A gate is the opposite kind of thing: dropping it
                    // because the file that declares it stopped parsing would quietly
                    // *widen* what the group's agents may do, and a syntax error is
                    // not consent to merge unreviewed code. So the last known gate
                    // stands, loudly.
                    if self.merge_gate_path(&id).is_file() {
                        self.audit(&id, "loomux", "merge-gate-retained", json!({
                            "reason": "the workflow file is invalid — keeping the last known merge gate rather than failing open",
                        }));
                    }
                }
            }
        } else if guardrails.advanced_orchestrator {
            // A resume. The persisted roster stands — but if the file has moved on
            // since the launch, the human should be able to SEE that the group they
            // are looking at is not what their repo now says. Silence here would
            // make the pin indistinguishable from a stale read.
            //
            // **The merge gate is pinned by the same rule, and left untouched here.**
            // The consent argument applies to it at least as strongly as to the
            // roster: a `git pull` between launch and resume must not be able to
            // *loosen* the gate a session is running under (drop a reviewer, remove
            // the clause entirely) — and re-reading the file is precisely how that
            // would happen. The gate file written at launch stands; the drift audit
            // tells the human the repo has moved on.
            self.audit_workflow_drift(&id, repo, &guardrails);
        } else {
            // The toggle is off, so the workflow is not running — and neither is its
            // gate. Clearing it here is what makes "the default experience is
            // byte-for-byte pre-#222" true for the *merge path* too, and it is what
            // stops a gate declared under an earlier advanced launch of this same
            // group dir from outliving the toggle that authorized it.
            self.sync_merge_gate(&id, None);
            if workflow::workflow_file_exists(repo) {
                // The repo declares a workflow and this group is deliberately not
                // running it. Say so in the trail: "my workflow file did nothing" is
                // otherwise a silent, and very confusing, non-event.
                self.audit(&id, "loomux", "workflow-ignored", json!({
                    "path": workflow::WORKFLOW_PATH,
                    "reason": "the advanced orchestrator is off for this group",
                    "action": "using the built-in roster",
                }));
            }
        }
        // The live-agent cap is adjustable mid-session (`set_max_agents`) and
        // persisted, so it's a durable human choice — like the pause/notify
        // markers re-seeded below. On resume, prefer the persisted cap over the
        // caller's param: the launcher hardcodes its default (4) and can't
        // pre-fill from group.json, so without this a relaunch would silently
        // revert an on-the-fly adjustment. Other guardrails still refresh from
        // the launch (only the cap is live-adjustable). Read before the write
        // below overwrites the file.
        if resumed {
            if let Some((_, persisted)) = self.load_group_file(&id) {
                guardrails.max_agents = persisted.max_agents.clamp(1, MAX_AGENTS_CEILING);
                // The autonomy budget (#83) is likewise live-adjustable and
                // persisted, so a relaunch must keep the human's set value rather
                // than reverting to the launcher's param.
                guardrails.autonomy_budget_tokens = persisted.autonomy_budget_tokens;
                // Same for the live-adjustable idle-tick window — re-normalize the
                // persisted value (0/absent from older group.json → default) since
                // this overwrite lands after the top-of-fn `clamped()`.
                guardrails.idle_tick_minutes = if persisted.idle_tick_minutes == 0 {
                    DEFAULT_IDLE_TICK_MINUTES
                } else {
                    persisted.idle_tick_minutes.clamp(1, MAX_IDLE_TICK_MINUTES)
                };
                guardrails.idle_activity_floor_bytes = if persisted.idle_activity_floor_bytes == 0 {
                    DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES
                } else {
                    persisted.idle_activity_floor_bytes.clamp(1, MAX_IDLE_ACTIVITY_FLOOR_BYTES)
                };
            }
        }
        // #255: advisory only — never override a cap the human set. A launcher
        // warning (surfaced from `orch_workflow_preview`, computed the same way)
        // is meant to catch this *before* Create; this audit record is the durable
        // trail for a launch that went ahead under a cap the workflow can't run
        // its designed roster under — e.g. resumed with a persisted cap the file
        // has since outgrown.
        if let Some(rec) = capacity {
            if guardrails.max_agents < rec.minimum {
                self.audit(&id, "loomux", "max-agents-below-minimum", json!({
                    "max_agents": guardrails.max_agents,
                    "minimum": rec.minimum,
                    "recommended": rec.recommended,
                    "note": format!(
                        "max_agents ({}) is below this workflow's minimum ({}) — its merge \
                         gate plus a worker can never all be live at once without evicting a \
                         live agent to make room.",
                        guardrails.max_agents, rec.minimum,
                    ),
                }));
            }
        }
        let info = GroupInfo { id: id.clone(), repo: repo.to_string(), guardrails };
        // Atomic replace: group.json is identity-critical (a truncated file
        // breaks the rejoin path), so a failed/interrupted write must leave the
        // prior file intact rather than half-written (#133). Matches the
        // crash-safe pattern `persist_max_agents` already uses for this file.
        // Includes the #83 autonomous guardrails.
        let body = serde_json::to_string_pretty(&json!({
            "group_id": info.id,
            "repo": info.repo,
            "created_ms": now_ms(),
            "guardrails": {
                "max_agents": info.guardrails.max_agents,
                "agent_cli": info.guardrails.agent_cli,
                // #222: the roster replaces the eight flat per-role fields. The
                // reader still understands the old shape (`read_blocks`), so a
                // group.json from 0.8.0 keeps loading; nothing writes it again.
                "blocks": blocks_json(&info.guardrails.blocks),
                // #222: whether this group runs the repo's workflow file. Absent
                // from an older group.json → false on read, which is exactly what
                // that group was: a built-in roster.
                "advanced_orchestrator": info.guardrails.advanced_orchestrator,
                "auto_ops": info.guardrails.auto_ops,
                "idle_kill_minutes": info.guardrails.idle_kill_minutes,
                "max_spawns_per_hour": info.guardrails.max_spawns_per_hour,
                "watchdog_stall_minutes": info.guardrails.watchdog_stall_minutes,
                "autonomy_budget_tokens": info.guardrails.autonomy_budget_tokens,
                "idle_tick_minutes": info.guardrails.idle_tick_minutes,
                "idle_activity_floor_bytes": info.guardrails.idle_activity_floor_bytes,
            },
        }))
        .unwrap();
        atomic_write(&dir.join("group.json"), body.as_bytes()).map_err(|e| e.to_string())?;
        self.write_instruction_files(&info)?;
        // A pause is a durable human safety action: re-seed it from the
        // marker file so a resumed group stays paused across restarts.
        if dir.join("paused").is_file() {
            self.paused.lock_safe().insert(id.clone());
        }
        // Desktop-notification opt-in is likewise a durable per-group choice.
        if dir.join("notify").is_file() {
            self.notify_groups.lock_safe().insert(id.clone());
        }
        // Autonomous mode (#83) and the auto-merge gate are durable per-group
        // choices too; re-seed them so a resumed group keeps ticking (and its
        // budget anchor, stored in the marker's content) across restarts. Audit
        // each resume so a persisted consent marker silently resuming autonomy/
        // auto-merge is at least *visible* in the trail (belt-and-suspenders for
        // the L2 consent-boundary concern — a marker that shouldn't be here shows
        // up rather than resuming invisibly).
        // A budget suspension takes precedence over the enable marker: if a failed
        // suspension left the `autonomous` marker on disk, the co-written
        // `autonomy_suspended` marker (rev-49) must still force the group back OFF at
        // restart — never silently resume ticking past a spent budget. The group
        // then reads as suspended (audited below + `orch_autonomy.suspended`).
        if dir.join("autonomy_suspended").is_file() {
            self.audit(&id, "loomux", "autonomous-suspended-resumed",
                json!({ "from": "marker" }));
        } else if dir.join("autonomous").is_file() {
            self.autonomous_groups.lock_safe().insert(id.clone());
            self.audit(&id, "loomux", "autonomous-resumed",
                json!({ "from": "marker", "budget_anchor_tokens": self.autonomy_anchor(&id) }));
        }
        // Auto-merge re-seed, with the #83 dependency reconciled: auto-merge is
        // valid ONLY alongside autonomous mode. A stale `auto_merge` marker without
        // a live `autonomous` marker (an older group predating the dependency, or a
        // hand-edited state dir) is force-cleared on read and audited, so the
        // enforced gate can never see the auto_merge-on/autonomous-off combo.
        if dir.join("auto_merge").is_file() {
            if self.autonomous_groups.lock_safe().contains(&id) {
                self.auto_merge_groups.lock_safe().insert(id.clone());
                self.audit(&id, "loomux", "auto-merge-resumed", json!({ "from": "marker" }));
            } else {
                let _ = remove_marker(&dir.join("auto_merge"));
                self.audit(&id, "loomux", "auto-merge-off",
                    json!({ "reason": "reconcile-autonomous-off" }));
            }
        }
        // Auto-release re-seed with the same dependency reconcile (independent of
        // auto_merge): valid only alongside autonomous; a stale marker is cleared.
        if dir.join("auto_release").is_file() {
            if self.autonomous_groups.lock_safe().contains(&id) {
                self.auto_release_groups.lock_safe().insert(id.clone());
                self.audit(&id, "loomux", "auto-release-resumed", json!({ "from": "marker" }));
            } else {
                let _ = remove_marker(&dir.join("auto_release"));
                self.audit(&id, "loomux", "auto-release-off",
                    json!({ "reason": "reconcile-autonomous-off" }));
            }
        }
        // Supervised dangerous mode re-seed (#83): valid only while NOT autonomous
        // (mutually exclusive). If both markers survived a hand-edit, autonomous
        // wins and the stale dangerous marker is cleared + audited.
        if dir.join("dangerous_mode").is_file() {
            if self.autonomous_groups.lock_safe().contains(&id) {
                let _ = remove_marker(&dir.join("dangerous_mode"));
                self.audit(&id, "loomux", "dangerous-mode-off",
                    json!({ "reason": "reconcile-autonomous-on" }));
            } else {
                self.dangerous_groups.lock_safe().insert(id.clone());
                self.audit(&id, "loomux", "dangerous-mode-resumed", json!({ "from": "marker" }));
            }
        }
        self.groups.lock_safe().insert(id.clone(), info.clone());
        self.audit(&id, "loomux", if resumed { "group-resume" } else { "group-create" },
            json!({ "repo": repo, "max_agents": info.guardrails.max_agents,
                    "blocks": blocks_json(&info.guardrails.blocks) }));
        Ok(info)
    }

    pub fn group(&self, id: &str) -> Option<GroupInfo> {
        self.groups.lock_safe().get(id).cloned()
    }

    /// A group is live while any of its agents is not dead.
    fn group_is_live(&self, id: &str) -> bool {
        self.agents
            .lock_safe()
            .values()
            .any(|a| a.group == id && a.status != AgentStatus::Dead)
    }

    // ---------- cost containment: pause, idle-kill, spawn-rate, usage ----------

    /// Whether a group is currently paused (prompts/kickoffs suppressed).
    pub fn is_paused(&self, group: &str) -> bool {
        self.paused.lock_safe().contains(group)
    }

    /// Pause a group: loomux stops delivering prompts and kickoffs to its
    /// agents, so they finish their current turn and idle out (containing
    /// unattended spend) without being killed. Durable via a marker file.
    pub fn pause_group(&self, group: &str) -> Result<(), String> {
        let newly = self.paused.lock_safe().insert(group.to_string());
        if newly {
            let dir = self.group_dir(group);
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let _ = fs::write(dir.join("paused"), b"");
            self.audit(group, "human", "group-pause", json!({}));
        }
        Ok(())
    }

    /// Resume a paused group: prompt/kickoff delivery flows again. Queued
    /// prompts are not replayed — agents resync from the board/state on the
    /// next prompt, which is the point of idling out.
    pub fn resume_group(&self, group: &str) -> Result<(), String> {
        let was = self.paused.lock_safe().remove(group);
        if was {
            let _ = fs::remove_file(self.group_dir(group).join("paused"));
            self.audit(group, "human", "group-resume", json!({}));
        }
        Ok(())
    }

    /// Flip a worker/reviewer between idle (awaiting/finished a task) and
    /// active. `idle` true stamps `idle_since_ms = now`; false clears it.
    /// No-op for the orchestrator, which is never idle-reaped.
    fn set_agent_idle(&self, agent_id: &str, idle: bool) {
        let mut agents = self.agents.lock_safe();
        if let Some(a) = agents.get_mut(agent_id) {
            if a.role == Role::Orchestrator {
                return;
            }
            a.idle_since_ms = idle.then(now_ms);
            if !idle {
                // (Re)assigned work: restart the watchdog's silence clock and
                // clear its anti-nag latch so the fresh stall gets a nudge.
                a.last_progress_ms = now_ms();
                a.watchdog_notified = false;
                // New work supersedes a prior done/blocked report — drop its
                // attention latch so a stale badge doesn't linger.
                self.attn_reports.lock_safe().remove(agent_id);
            }
        }
    }

    /// Ids of workers/reviewers whose idle time has crossed their group's
    /// `idle_kill_minutes`. Pure selection (no killing) so the reaper policy
    /// is testable at a chosen `now`.
    pub fn idle_reap_candidates(&self, now: u64) -> Vec<String> {
        let thresholds: HashMap<String, u32> = self
            .groups
            .lock_safe()
            .iter()
            .map(|(id, g)| (id.clone(), g.guardrails.idle_kill_minutes))
            .collect();
        self.agents
            .lock_safe()
            .values()
            .filter(|a| a.role != Role::Orchestrator && a.status == AgentStatus::Running)
            .filter(|a| {
                let t = thresholds.get(&a.group).copied().unwrap_or(0);
                idle_should_kill(a.idle_since_ms, now, t)
            })
            .map(|a| a.id.clone())
            .collect()
    }

    /// Kill every idle worker/reviewer past its group's timeout, notifying
    /// each group's orchestrator so it can respawn on demand. Returns the
    /// killed agent ids. Called on a timer by `start_idle_reaper`.
    pub fn reap_idle_agents(&self, now: u64) -> Vec<String> {
        let mut killed = Vec::new();
        for id in self.idle_reap_candidates(now) {
            let Some(a) = self.agent(&id) else { continue };
            let mins = self
                .group(&a.group)
                .map(|g| g.guardrails.idle_kill_minutes)
                .unwrap_or(0);
            // Re-check against the agent's *current* idle state: selection and
            // kill happen under separate locks, so a worker prompted in that
            // window (idle clock cleared) must not be killed.
            if !idle_should_kill(a.idle_since_ms, now, mins) {
                continue;
            }
            self.audit(&a.group, "loomux", "idle-kill",
                json!({ "agent": id, "name": a.name, "idle_minutes": mins }));
            let _ = self.deliver_to_orchestrator(
                &a.group,
                &format!(
                    "[loomux] idle-kill guardrail: agent {} ({}) sat without a task for {mins}+ min and was terminated to contain cost. Respawn a worker when you have work for it.",
                    a.name, a.id
                ),
                "loomux",
            );
            let _ = self.kill_agent(&id);
            killed.push(id);
        }
        killed
    }

    // ---------- watchdog: stalled-agent detection ----------

    /// Record that an agent just did something loomux can see (reported,
    /// messaged the orchestrator): reset its watchdog silence clock and clear
    /// the anti-nag latch so a *later* stall still earns a fresh nudge. No-op
    /// for the orchestrator (never watchdogged). Output-driven activity is
    /// handled separately in `watchdog_tick` via the pty counter.
    pub fn note_agent_activity(&self, agent_id: &str) {
        let mut agents = self.agents.lock_safe();
        if let Some(a) = agents.get_mut(agent_id) {
            if a.role == Role::Orchestrator {
                return;
            }
            a.last_progress_ms = now_ms();
            a.watchdog_notified = false;
        }
    }

    /// Snapshot every agent's monotonic pty output counter. Needs the app's
    /// `PtyManager`, so it yields an empty map without an app handle (unit
    /// tests drive `watchdog_tick` with synthetic counters instead).
    fn agent_output_totals(&self) -> HashMap<String, u64> {
        let Some(app) = self.app.lock_safe().clone() else {
            return HashMap::new();
        };
        let ptys = app.state::<crate::pty::PtyManager>();
        self.agents
            .lock_safe()
            .values()
            .filter_map(|a| Some((a.id.clone(), ptys.output_total(a.pty_id?)?)))
            .collect()
    }

    /// One watchdog pass. For each *working* agent (running worker/reviewer
    /// with a task assigned — idle clock clear), fold in the latest pty output
    /// counter from `outputs`: any growth is activity that resets the silence
    /// clock and the anti-nag latch. An agent silent (no output, no report)
    /// past its group's `watchdog_stall_minutes` earns exactly one audited
    /// `[loomux]` nudge to the orchestrator suggesting get_output + re-send.
    /// Paused groups are skipped entirely — delivery is suppressed there
    /// anyway, so we must not spend the one-notice budget while paused.
    /// Returns the notified agent ids. Split from the pty read
    /// (`agent_output_totals`) so the stall / anti-nag / pause logic is
    /// testable with synthetic counters and no threads.
    pub fn watchdog_tick(&self, now: u64, outputs: &HashMap<String, u64>) -> Vec<String> {
        let thresholds: HashMap<String, u32> = self
            .groups
            .lock_safe()
            .iter()
            .map(|(id, g)| (id.clone(), g.guardrails.watchdog_stall_minutes))
            .collect();
        let paused = self.paused.lock_safe().clone();

        // First pass under the agents lock: refresh counters and pick who to
        // nudge. Delivery (which types into a pane and can block) happens after
        // the lock is released.
        let mut to_notify: Vec<(String, String, String, u32)> = Vec::new();
        {
            let mut agents = self.agents.lock_safe();
            for a in agents.values_mut() {
                // Only agents actively working: running, not the orchestrator,
                // and currently assigned (idle_since_ms clear). This excludes
                // idle, done/blocked, dead, and reaped agents by construction.
                if a.role == Role::Orchestrator
                    || a.status != AgentStatus::Running
                    || a.idle_since_ms.is_some()
                {
                    continue;
                }
                // Output growth = activity: reset the clock and the latch, and
                // this tick can't also flag the agent as stalled.
                if let Some(&cur) = outputs.get(&a.id) {
                    if cur > a.last_output_total {
                        a.last_output_total = cur;
                        a.last_progress_ms = now;
                        a.watchdog_notified = false;
                        continue;
                    }
                }
                // A paused group's agents idle out on purpose; never nudge and
                // never burn their one-notice budget while paused.
                if paused.contains(&a.group) {
                    continue;
                }
                let threshold = thresholds.get(&a.group).copied().unwrap_or(0);
                if watchdog_should_notify(a.last_progress_ms, now, threshold, a.watchdog_notified) {
                    a.watchdog_notified = true;
                    let minutes = (now.saturating_sub(a.last_progress_ms) / 60_000) as u32;
                    to_notify.push((a.id.clone(), a.group.clone(), a.name.clone(), minutes));
                }
            }
        }

        let mut notified = Vec::new();
        for (id, group, name, minutes) in to_notify {
            self.audit(&group, "loomux", "watchdog-stall",
                json!({ "agent": id, "name": name, "silent_minutes": minutes }));
            let _ = self.deliver_to_orchestrator(
                &group,
                &format!(
                    "[loomux] watchdog: agent {name} ({id}) has produced no terminal output and sent no report for {minutes}+ min — it may be stalled or waiting on input. Inspect it with get_output(\"{id}\"); if its kickoff was lost or it is stuck, re-send the task with send_prompt. You will get this notice at most once per stall."
                ),
                "loomux",
            );
            notified.push(id);
        }
        notified
    }

    /// One full watchdog cycle: read pty counters, then tick. Called on a
    /// timer by `start_watchdog`.
    pub fn run_watchdog(&self, now: u64) -> Vec<String> {
        let outputs = self.agent_output_totals();
        self.watchdog_tick(now, &outputs)
    }

    // ---------- autonomous mode (#83): idle-tick + budget enforcement ----------

    /// Read every live orchestrator pane's output counter and last human-keystroke
    /// time — the raw inputs `idle_tick_tick` needs to judge output-silence.
    /// Split from the decision (as `agent_output_totals` is for the watchdog) so
    /// the tick logic is testable with synthetic maps and no pty. Empty without an
    /// app handle (unit tests drive `idle_tick_tick` directly).
    fn orchestrator_activity(&self) -> (HashMap<String, u64>, HashMap<String, u64>) {
        let mut outs = HashMap::new();
        let mut ins = HashMap::new();
        let Some(app) = self.app.lock_safe().clone() else {
            return (outs, ins);
        };
        let ptys = app.state::<crate::pty::PtyManager>();
        for a in self.agents.lock_safe().values() {
            if a.role != Role::Orchestrator {
                continue;
            }
            let Some(pid) = a.pty_id else { continue };
            if let Some(t) = ptys.output_total(pid) {
                outs.insert(a.id.clone(), t);
            }
            if let Some(u) = ptys.last_user_input_ms(pid) {
                ins.insert(a.id.clone(), u);
            }
        }
        (outs, ins)
    }

    /// One idle-tick pass. For each **autonomous, non-paused** group's running
    /// orchestrator, fold in the latest pty output counter and last human-input
    /// time: output growth (the orchestrator acting) resets the quiet clock and
    /// the one-notice latch; recent human input also defers the clock (never tick
    /// while the human steers — the belt-and-suspenders gate on top of
    /// output-silence). An orchestrator output-quiet past `IDLE_TICK_MINUTES`,
    /// not already latched, and under the per-hour cap earns exactly one audited
    /// `[loomux] idle tick` notice telling it to run its monitoring/intake
    /// cadence. Paused groups are skipped wholesale (delivery is suppressed there;
    /// don't burn the latch). Returns the notified orchestrator ids. Split from
    /// the pty read (`orchestrator_activity`) so the gate / latch / cap / pause
    /// logic is testable with synthetic counters — the `watchdog_tick` shape.
    pub fn idle_tick_tick(
        &self,
        now: u64,
        outputs: &HashMap<String, u64>,
        inputs: &HashMap<String, u64>,
    ) -> Vec<String> {
        let autonomous = self.autonomous_groups.lock_safe().clone();
        if autonomous.is_empty() {
            return Vec::new();
        }
        let paused = self.paused.lock_safe().clone();
        let tick_times = self.idle_tick_times.lock_safe().clone();
        // Per-group idle-tick window + activity floor (guardrails, live-adjustable).
        // Snapshot like `watchdog_tick` does its thresholds.
        let cfg: HashMap<String, (u32, u64)> = self
            .groups
            .lock_safe()
            .iter()
            .map(|(id, g)| {
                (id.clone(), (g.guardrails.idle_tick_minutes, g.guardrails.idle_activity_floor_bytes))
            })
            .collect();

        let mut to_notify: Vec<(String, String)> = Vec::new();
        {
            let mut agents = self.agents.lock_safe();
            for a in agents.values_mut() {
                if a.role != Role::Orchestrator
                    || a.status != AgentStatus::Running
                    || !autonomous.contains(&a.group)
                {
                    continue;
                }
                // Meaningful output growth = the orchestrator produced a real burst
                // (it acted): reset the quiet clock and clear the latch, and this
                // tick can't also fire. Sub-floor growth is idle repaint noise — it
                // rebaselines the counter but does NOT reset the clock, so an
                // occasional statusline/spinner frame can't starve the tick (the
                // bug where any stray byte demanded another full quiet window).
                let (threshold, floor) = cfg
                    .get(&a.group)
                    .copied()
                    .unwrap_or((DEFAULT_IDLE_TICK_MINUTES, DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES));
                if let Some(&cur) = outputs.get(&a.id) {
                    let meaningful = idle_output_is_activity(a.last_output_total, cur, floor);
                    a.last_output_total = cur; // rebaseline every observation
                    if meaningful {
                        a.last_progress_ms = now;
                        a.idle_tick_notified = false;
                        continue;
                    }
                }
                // Belt-and-suspenders: recent human input in the pane is activity
                // — fold it into the quiet clock so a tick never lands while the
                // human is steering (mirrors attention routing's `waiting`
                // heuristic). Not latch-clearing: human typing isn't the
                // orchestrator acting on our notice, it just defers the window.
                if let Some(&last_in) = inputs.get(&a.id) {
                    if last_in > a.last_progress_ms {
                        a.last_progress_ms = last_in;
                    }
                }
                // A paused group's orchestrator is deliberately quiet; never tick
                // and never burn its one-notice latch while paused.
                if paused.contains(&a.group) {
                    continue;
                }
                let times = tick_times.get(&a.group).map(Vec::as_slice).unwrap_or(&[]);
                if idle_tick_should_fire(
                    a.last_progress_ms,
                    now,
                    threshold,
                    a.idle_tick_notified,
                    times,
                    MAX_IDLE_TICKS_PER_HOUR,
                ) {
                    a.idle_tick_notified = true;
                    to_notify.push((a.id.clone(), a.group.clone()));
                }
            }
        }

        let mut notified = Vec::new();
        for (id, group) in to_notify {
            // Record the delivery for the per-hour backstop, pruning to the window.
            // This ring is in-memory only (like `spawn_times`): a restart resets
            // the window, which is the safe direction — the cap is a runaway
            // backstop, and the quiet-window + one-notice latch already bound ticks
            // to ~one per window regardless, so a fresh window after a (rare)
            // restart can't produce a runaway, only at most a few extra ticks.
            {
                let mut tt = self.idle_tick_times.lock_safe();
                let v = tt.entry(group.clone()).or_default();
                v.push(now);
                v.retain(|&t| now.saturating_sub(t) < SPAWN_RATE_WINDOW_MS);
            }
            self.audit(&group, "loomux", "idle-tick", json!({ "orchestrator": id }));
            let _ = self.deliver_to_orchestrator(&group, &idle_tick_notice(), "loomux");
            notified.push(id);
        }
        notified
    }

    /// Enforce every autonomous group's token budget (#83). For each autonomous,
    /// non-paused group with a budget set, meter spend as the delta from the
    /// enable-time anchor; once it crosses the budget, **suspend** autonomous mode
    /// (flip the marker off — explicit consent required to resume), audit it, and
    /// deliver ONE `[loomux]` notice. Because suspension removes the group from
    /// the autonomous set, a later pass skips it, so the notice can't repeat.
    /// Returns the suspended group ids. Runs before the idle tick each cycle.
    pub fn enforce_autonomy_budgets(&self, _now: u64) -> Vec<String> {
        let autonomous = self.autonomous_groups.lock_safe().clone();
        if autonomous.is_empty() {
            return Vec::new();
        }
        let paused = self.paused.lock_safe().clone();
        let mut suspended = Vec::new();
        for group in autonomous {
            // Paused groups already don't tick; leave their meter frozen.
            if paused.contains(&group) {
                continue;
            }
            let budget = self
                .group(&group)
                .map(|g| g.guardrails.autonomy_budget_tokens)
                .unwrap_or(0);
            if budget == 0 {
                continue; // no cap
            }
            let anchor = self.autonomy_anchor(&group);
            let spent = self.group_token_total(&group).saturating_sub(anchor);
            if autonomy_budget_exhausted(spent, budget) {
                self.audit(&group, "loomux", "autonomy-budget-exhausted",
                    json!({ "spent_tokens": spent, "budget_tokens": budget }));
                // Money-stop: drop the group from the autonomous set unconditionally
                // so ticking halts even if the marker can't be removed (rev-49).
                self.suspend_autonomous(&group);
                // Distinguish a budget suspension from a plain user-off with a
                // durable `autonomy_suspended` marker, so the UI can tell the human
                // "suspended — raise the budget or re-enable" instead of
                // reconstructing it from the audit log. Written *after* the disable
                // (which turns autonomous off); cleared on a genuine re-enable. A
                // hint, not a consent gate, so a failed write fails soft.
                let _ = fs::write(
                    self.group_dir(&group).join("autonomy_suspended"),
                    json!({ "spent_tokens": spent, "budget_tokens": budget }).to_string(),
                );
                let _ = self.deliver_to_orchestrator(
                    &group,
                    &autonomy_budget_notice(spent, budget),
                    "loomux",
                );
                suspended.push(group);
            }
        }
        suspended
    }

    /// One full idle-tick cycle: enforce budgets (which may suspend groups), then
    /// read pty counters and tick the still-autonomous orchestrators. Called on a
    /// timer by `start_idle_tick`; `now` injected so tests drive it deterministically.
    pub fn run_idle_tick(&self, now: u64) -> Vec<String> {
        self.enforce_autonomy_budgets(now);
        let (outputs, inputs) = self.orchestrator_activity();
        self.idle_tick_tick(now, &outputs, &inputs)
    }

    // ---------- attention routing: surface which pane needs the human ----------

    /// Latch (or clear) a worker's report as an attention signal. `done` and
    /// `blocked` badge the pane and can fire a toast until the human acks or the
    /// agent is reassigned; `progress` (the agent is working again) clears it.
    /// No-op for the orchestrator, which never reports.
    pub fn note_report_attention(&self, agent_id: &str, status: &str) {
        let mut m = self.attn_reports.lock_safe();
        match status {
            "done" => {
                m.insert(agent_id.to_string(), "done");
            }
            "blocked" => {
                m.insert(agent_id.to_string(), "blocked");
            }
            _ => {
                m.remove(agent_id);
            }
        }
    }

    /// The human focused/handled a pane: drop any latched report so its badge
    /// clears, and suppress the live `waiting` badge so focusing a pane whose
    /// menu is still on screen makes the ack *stick* — otherwise the next 3s scan
    /// re-emits `waiting` and re-lights the pane the human is already on (#40
    /// review). The suppression self-clears once the pane's output changes (the
    /// menu was answered / the CLI repainted), so a genuinely new prompt on the
    /// same pane flags again. The `gate` reason is board state, cleared by moving
    /// the task, so it needs no ack.
    pub fn ack_attention(&self, agent_id: &str) {
        self.attn_reports.lock_safe().remove(agent_id);
        self.attn_waiting_ack.lock_safe().insert(agent_id.to_string());
    }

    /// The human turned to a *plain* pane (#40): make its `waiting` ack stick the
    /// same way `ack_attention` does for agents, keyed by the pane's pty id. The
    /// suppression lifts when the pane's output next changes (see
    /// `plain_pane_attention`).
    pub fn ack_attention_pty(&self, pty_id: u32) {
        self.attn_waiting_ack.lock_safe().insert(format!("pty:{pty_id}"));
    }

    /// Whether desktop notifications are enabled for a group.
    pub fn notify_enabled(&self, group: &str) -> bool {
        self.notify_groups.lock_safe().contains(group)
    }

    /// Enable/disable desktop notifications for a group, durably (a `notify`
    /// marker file, mirroring the pause marker) so the choice survives restarts.
    pub fn set_notify(&self, group: &str, on: bool) -> Result<(), String> {
        let dir = self.group_dir(group);
        let mut set = self.notify_groups.lock_safe();
        if on {
            if set.insert(group.to_string()) {
                fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let _ = fs::write(dir.join("notify"), b"");
                self.audit(group, "human", "notify-on", json!({}));
            }
        } else if set.remove(group) {
            let _ = fs::remove_file(dir.join("notify"));
            self.audit(group, "human", "notify-off", json!({}));
        }
        Ok(())
    }

    // ---------- autonomous mode (#83): idle-tick + auto-merge + budget ----------

    /// Whether autonomous idle-ticking is enabled for a group (drives the toggle
    /// button state and gates the idle-tick loop).
    pub fn is_autonomous(&self, group: &str) -> bool {
        self.autonomous_groups.lock_safe().contains(group)
    }

    /// The usage-token count captured when autonomous mode was last enabled — the
    /// anchor the budget meters spend *from*, stored as the `autonomous` marker's
    /// content so it survives restarts. 0 when off or unstamped (legacy/empty
    /// marker → meters against 0, i.e. all history, which is the safe/conservative
    /// direction: it can only suspend *sooner*).
    fn autonomy_anchor(&self, group: &str) -> u64 {
        fs::read_to_string(self.group_dir(group).join("autonomous"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
    }

    /// Whether autonomous mode is OFF *because the budget enforcer suspended it*
    /// (as opposed to a plain user toggle-off or never-on). Backed by a durable
    /// `autonomy_suspended` marker written by `enforce_autonomy_budgets` and
    /// cleared on a genuine re-enable, so it survives restarts. Only meaningful
    /// while off — `autonomy_state` gates it on `!is_autonomous`.
    fn autonomy_suspended(&self, group: &str) -> bool {
        self.group_dir(group).join("autonomy_suspended").is_file()
    }

    /// Enable/disable autonomous idle-ticking for a group, durably (an
    /// `autonomous` marker file, mirroring the pause/notify markers). Enabling
    /// stamps the marker with the group's current usage-token total as the budget
    /// anchor, so the budget meters only spend incurred *after* this point — the
    /// "autonomous-era spend" the human is consenting to (re-enabling after a
    /// budget suspension re-anchors, which is what "toggle to resume" means).
    /// Audited on every real state change (actor `human`). The budget-suspension
    /// path uses `suspend_autonomous` instead (different failure policy).
    pub fn set_autonomous(&self, group: &str, on: bool) -> Result<(), String> {
        self.set_autonomous_as(group, on, "human")
    }

    /// Disable autonomous mode as a **budget suspension** (actor `loomux`). Unlike a
    /// user disable (`set_autonomous_as`, disk-first + fail-loud to protect the
    /// consent boundary), the money-stop here is inverted: continued spend past the
    /// cap is the one direction this feature must never allow, so the in-memory flag
    /// is dropped **unconditionally** — ticking halts even if the durable marker
    /// can't be removed. A marker-removal failure is audited, not fatal; a surviving
    /// `autonomous` marker is then overridden at restart by the co-written
    /// `autonomy_suspended` marker (see the re-seed in `create_group`), so the group
    /// comes back OFF + suspended-visible, never silently ticking past its budget.
    fn suspend_autonomous(&self, group: &str) {
        // Stop the spend first and unconditionally.
        self.autonomous_groups.lock_safe().remove(group);
        self.clear_idle_tick_latch(group);
        // Money-stop the merge gate too (rev-79 F4): auto-merge without autonomous
        // would leave the gate open, so drop it from the in-memory gate set
        // UNCONDITIONALLY — the same #149 pattern (in-memory authoritative even if
        // disk removal fails).
        self.force_disable_auto_merge(group, "loomux", "autonomous-suspended");
        self.force_disable_auto_release(group, "loomux", "autonomous-suspended");
        // Best-effort durable disable; failure is surfaced in the audit trail.
        match remove_marker(&self.group_dir(group).join("autonomous")) {
            Ok(()) => self.audit(group, "loomux", "autonomous-off", json!({})),
            Err(e) => self.audit(group, "loomux", "autonomous-off-failed", json!({ "error": e })),
        }
    }

    /// Drop auto-merge for a group UNCONDITIONALLY (rev-79 F4 / #149 money-stop):
    /// the in-memory gate-decision set is authoritative and cleared even if the
    /// durable marker removal fails, so autonomous-off can never leave the merge
    /// gate open. Best-effort marker removal + audit/notify only when it was on.
    fn force_disable_auto_merge(&self, group: &str, actor: &str, reason: &str) {
        if self.auto_merge_groups.lock_safe().remove(group) {
            let _ = remove_marker(&self.group_dir(group).join("auto_merge"));
            self.audit(group, actor, "auto-merge-off", json!({ "reason": reason }));
            let _ = self.deliver_to_orchestrator(group, &auto_merge_notice(false), "loomux");
        }
    }

    /// Drop auto-release for a group UNCONDITIONALLY (same money-stop as
    /// `force_disable_auto_merge`): auto-release without autonomous would leave the
    /// release gate open, so the in-memory gate set is authoritative and cleared
    /// even if the marker removal fails.
    fn force_disable_auto_release(&self, group: &str, actor: &str, reason: &str) {
        if self.auto_release_groups.lock_safe().remove(group) {
            let _ = remove_marker(&self.group_dir(group).join("auto_release"));
            self.audit(group, actor, "auto-release-off", json!({ "reason": reason }));
            let _ = self.deliver_to_orchestrator(group, &auto_release_notice(false), "loomux");
        }
    }

    /// Drop supervised dangerous mode UNCONDITIONALLY (mutual exclusion: enabling
    /// autonomous force-clears it — the two can never both be on). In-memory
    /// authoritative even if the marker's disk removal fails, with an audit entry
    /// and a human-visible notice. `by_autonomous` tailors the notice wording.
    fn force_disable_dangerous_mode(&self, group: &str, actor: &str, reason: &str, by_autonomous: bool) {
        if self.dangerous_groups.lock_safe().remove(group) {
            let _ = remove_marker(&self.group_dir(group).join("dangerous_mode"));
            self.audit(group, actor, "dangerous-mode-off", json!({ "reason": reason }));
            let _ = self.deliver_to_orchestrator(group, &dangerous_mode_notice(false, by_autonomous), "loomux");
        }
    }

    /// `set_autonomous` with an explicit actor so a loomux-initiated suspension
    /// (budget exhausted) audits honestly rather than as a human toggle.
    fn set_autonomous_as(&self, group: &str, on: bool, actor: &str) -> Result<(), String> {
        let dir = self.group_dir(group);
        if on {
            // Reserve the enable atomically (L1): a single `insert` decides who
            // proceeds, so a concurrent/duplicate enable can't double-anchor or
            // race the marker write. Only the first caller (newly inserted) writes
            // the anchor + marker; anyone else sees it already on and no-ops.
            let newly = self.autonomous_groups.lock_safe().insert(group.to_string());
            if !newly {
                return Ok(()); // already on — don't re-anchor
            }
            // Anchor = spend at enable time, so the budget delta starts at 0.
            // Computed without holding the set lock (group_usage takes its own).
            let anchor = self.group_token_total(group);
            if let Err(e) = fs::create_dir_all(&dir)
                .and_then(|_| fs::write(dir.join("autonomous"), anchor.to_string()))
            {
                // Roll back the reservation so memory never claims ON without a
                // durable marker (a lost enable is the safe direction — it fails
                // OFF and re-asks for consent — but we still surface the failure).
                self.autonomous_groups.lock_safe().remove(group);
                return Err(format!("failed to enable autonomous mode: {e}"));
            }
            // A genuine (re-)enable resolves any prior budget suspension: clear the
            // suspended marker so the UI stops flagging it. Best-effort — it's a
            // UI hint, and `autonomy_state` only reports suspended while OFF anyway.
            let _ = remove_marker(&dir.join("autonomy_suspended"));
            self.audit(group, actor, "autonomous-on",
                json!({ "budget_anchor_tokens": anchor }));
            // Mutual exclusion (#83): supervised dangerous mode is the *not*-
            // autonomous manual mode, so enabling autonomous force-clears it
            // (audited + human-visible notice).
            self.force_disable_dangerous_mode(group, actor, "autonomous-enabled", true);
        } else {
            if !self.autonomous_groups.lock_safe().contains(group) {
                return Ok(()); // already off
            }
            // Remove the durable marker FIRST and fail the call if it doesn't go
            // (L2): a surviving marker would silently re-enable autonomous mode on
            // the next restart's re-seed without renewed consent. Only flip the
            // in-memory flag once disk agrees, so a failed disable leaves state
            // consistently ON (matching the marker the human still sees).
            if let Err(e) = remove_marker(&dir.join("autonomous")) {
                self.audit(group, actor, "autonomous-off-failed", json!({ "error": e }));
                return Err(format!(
                    "couldn't disable autonomous mode: the consent marker could not be \
                     removed, so it stays ON — retry or check disk/permissions"
                ));
            }
            self.autonomous_groups.lock_safe().remove(group);
            // Clear the idle-tick latch so a later re-enable starts clean.
            self.clear_idle_tick_latch(group);
            self.audit(group, actor, "autonomous-off", json!({}));
            // Dependency (#83): auto-merge AND auto-release exist only in autonomous
            // mode, so turning autonomous OFF force-disables both unconditionally
            // (rev-79 F4) — the pair can never be gate-on/autonomous-off.
            self.force_disable_auto_merge(group, actor, "autonomous-disabled");
            self.force_disable_auto_release(group, actor, "autonomous-disabled");
        }
        Ok(())
    }

    /// Whether the orchestrator may merge adequately-tested PRs itself for a group
    /// (auto-merge gate off = the default human merge gate). Drives the toggle
    /// state and is mirrored into the orchestrator's kickoff config.
    pub fn is_auto_merge(&self, group: &str) -> bool {
        self.auto_merge_groups.lock_safe().contains(group)
    }

    /// Enable/disable the auto-merge gate for a group, durably (an `auto_merge`
    /// marker file). Default OFF = today's behavior (human merges). The behavior
    /// lives in the orchestrator template, which reads the flag from its kickoff
    /// config; a live toggle both re-seeds that config for a restart and delivers
    /// one audited notice so the running orchestrator learns the new gate.
    pub fn set_auto_merge(&self, group: &str, on: bool) -> Result<(), String> {
        let dir = self.group_dir(group);
        if on {
            // Dependency (#83): auto-merge authority exists ONLY in autonomous mode.
            // Reject enabling it while autonomous is off so the pair can never be
            // auto_merge-on/autonomous-off — the combo the enforced gate keys on.
            if !self.is_autonomous(group) {
                return Err(
                    "auto-merge requires autonomous mode — turn on Autonomous mode first".into(),
                );
            }
            // Atomic reserve (mirrors set_autonomous_as): only the newly-inserting
            // caller writes the marker; a duplicate enable no-ops.
            let newly = self.auto_merge_groups.lock_safe().insert(group.to_string());
            if !newly {
                return Ok(()); // no-op: don't re-notify
            }
            if let Err(e) = fs::create_dir_all(&dir)
                .and_then(|_| fs::write(dir.join("auto_merge"), b""))
            {
                self.auto_merge_groups.lock_safe().remove(group);
                return Err(format!("failed to enable auto-merge: {e}"));
            }
            self.audit(group, "human", "auto-merge-on", json!({}));
        } else {
            if !self.auto_merge_groups.lock_safe().contains(group) {
                return Ok(()); // no-op: don't re-notify
            }
            // Disk first, then memory (L2): a surviving `auto_merge` marker would
            // silently re-enable the orchestrator's merge authority on restart
            // without renewed consent, so a failed removal fails the toggle and
            // leaves the gate consistently ON.
            if let Err(e) = remove_marker(&dir.join("auto_merge")) {
                self.audit(group, "human", "auto-merge-off-failed", json!({ "error": e }));
                return Err(format!(
                    "couldn't disable auto-merge: the consent marker could not be \
                     removed, so it stays ON — retry or check disk/permissions"
                ));
            }
            self.auto_merge_groups.lock_safe().remove(group);
            self.audit(group, "human", "auto-merge-off", json!({}));
        }
        // Tell the running orchestrator the gate moved (best-effort; a dead/paused
        // orchestrator just misses it and re-reads its kickoff config on resume).
        let _ = self.deliver_to_orchestrator(group, &auto_merge_notice(on), "loomux");
        Ok(())
    }

    /// Whether the orchestrator may publish releases/tags itself for a group
    /// (auto-release gate off = releases need a per-tag human grant). Independent
    /// of auto-merge. Drives the toggle state + mirrored into the kickoff config.
    pub fn is_auto_release(&self, group: &str) -> bool {
        self.auto_release_groups.lock_safe().contains(group)
    }

    /// Enable/disable the auto-release gate for a group, durably (an `auto_release`
    /// marker file). Default OFF = publishing needs a per-tag human grant. Mirrors
    /// `set_auto_merge` exactly (independent marker): gated behind autonomous mode
    /// (rejects enable unless autonomous is on), disk-first fail-loud disable, one
    /// audited notice to the orchestrator.
    pub fn set_auto_release(&self, group: &str, on: bool) -> Result<(), String> {
        let dir = self.group_dir(group);
        if on {
            // Same dependency as auto-merge: auto-release authority exists ONLY in
            // autonomous mode, so the pair can never be auto_release-on/autonomous-off.
            if !self.is_autonomous(group) {
                return Err(
                    "auto-release requires autonomous mode — turn on Autonomous mode first".into(),
                );
            }
            let newly = self.auto_release_groups.lock_safe().insert(group.to_string());
            if !newly {
                return Ok(()); // no-op: don't re-notify
            }
            if let Err(e) = fs::create_dir_all(&dir)
                .and_then(|_| fs::write(dir.join("auto_release"), b""))
            {
                self.auto_release_groups.lock_safe().remove(group);
                return Err(format!("failed to enable auto-release: {e}"));
            }
            self.audit(group, "human", "auto-release-on", json!({}));
        } else {
            if !self.auto_release_groups.lock_safe().contains(group) {
                return Ok(()); // no-op: don't re-notify
            }
            // Disk first, then memory: a surviving `auto_release` marker would
            // silently re-enable publishing authority on restart, so a failed
            // removal fails the toggle and leaves the gate consistently ON.
            if let Err(e) = remove_marker(&dir.join("auto_release")) {
                self.audit(group, "human", "auto-release-off-failed", json!({ "error": e }));
                return Err(format!(
                    "couldn't disable auto-release: the consent marker could not be \
                     removed, so it stays ON — retry or check disk/permissions"
                ));
            }
            self.auto_release_groups.lock_safe().remove(group);
            self.audit(group, "human", "auto-release-off", json!({}));
        }
        let _ = self.deliver_to_orchestrator(group, &auto_release_notice(on), "loomux");
        Ok(())
    }

    /// Whether supervised dangerous mode is on for a group (the human is present and
    /// authorized manual merges/releases outside autonomous mode). Mutually
    /// exclusive with autonomous.
    pub fn is_dangerous_mode(&self, group: &str) -> bool {
        self.dangerous_groups.lock_safe().contains(group)
    }

    /// Enable/disable supervised dangerous mode, durably (a `dangerous_mode`
    /// marker). **Mutually exclusive with autonomous**: enabling is REJECTED while
    /// autonomous is on (with a clear error); enabling autonomous force-clears this
    /// (see `set_autonomous_as`). Disk-first fail-loud disable, one audited notice.
    /// Human-only (Tauri command; no MCP surface — an agent can no more enable this
    /// than it can mint a grant; the marker's FS-forgeability is the same documented
    /// bypass class as grant files, closed by a machine account).
    pub fn set_dangerous_mode(&self, group: &str, on: bool) -> Result<(), String> {
        let dir = self.group_dir(group);
        if on {
            // Mutual exclusion: dangerous mode is the *supervised, not-autonomous*
            // mode. Reject enabling it while autonomous is on.
            if self.is_autonomous(group) {
                return Err(
                    "dangerous mode can't be enabled while autonomous mode is on — they are \
                     mutually exclusive; turn off Autonomous mode first".into(),
                );
            }
            let newly = self.dangerous_groups.lock_safe().insert(group.to_string());
            if !newly {
                return Ok(()); // no-op
            }
            if let Err(e) = fs::create_dir_all(&dir)
                .and_then(|_| fs::write(dir.join("dangerous_mode"), b""))
            {
                self.dangerous_groups.lock_safe().remove(group);
                return Err(format!("failed to enable dangerous mode: {e}"));
            }
            self.audit(group, "human", "dangerous-mode-on", json!({}));
        } else {
            if !self.dangerous_groups.lock_safe().contains(group) {
                return Ok(()); // no-op
            }
            // Disk first, fail loud: a surviving marker would silently re-enable
            // merge/release authority on restart.
            if let Err(e) = remove_marker(&dir.join("dangerous_mode")) {
                self.audit(group, "human", "dangerous-mode-off-failed", json!({ "error": e }));
                return Err(format!(
                    "couldn't disable dangerous mode: the marker could not be removed, so it \
                     stays ON — retry or check disk/permissions"
                ));
            }
            self.dangerous_groups.lock_safe().remove(group);
            self.audit(group, "human", "dangerous-mode-off", json!({ "reason": "human" }));
        }
        let _ = self.deliver_to_orchestrator(group, &dangerous_mode_notice(on, false), "loomux");
        Ok(())
    }

    // ---------- low-disk backstop (#134) ----------

    /// One low-disk backstop pass given the current free bytes on the workspace
    /// drive. On the tick that first crosses below `LOW_DISK_BYTES`, deliver ONE
    /// audited notice to each live, non-paused group's orchestrator and latch;
    /// the latch clears once free space recovers past `LOW_DISK_CLEAR_BYTES`.
    /// Paused groups are skipped (like the watchdog) — their agents idle out on
    /// purpose and prompt delivery is suppressed there anyway. Returns the
    /// notified group ids. Free-bytes is injected so the latch/hysteresis logic
    /// is testable without a real disk.
    pub fn disk_tick(&self, free: u64) -> Vec<String> {
        let fire = {
            let mut latched = self.low_disk_notified.lock_safe();
            let (new_latched, fire) =
                low_disk_transition(free, LOW_DISK_BYTES, LOW_DISK_CLEAR_BYTES, *latched);
            *latched = new_latched;
            fire
        };
        if !fire {
            return Vec::new();
        }
        // Snapshot groups/paused, then deliver outside any lock (delivery types
        // into a pane and can block).
        let paused = self.paused.lock_safe().clone();
        let groups: Vec<String> = self.groups.lock_safe().keys().cloned().collect();
        let notice = low_disk_notice(free);
        let mut notified = Vec::new();
        for group in groups {
            if paused.contains(&group) {
                continue;
            }
            self.audit(&group, "loomux", "low-disk", json!({ "free_bytes": free }));
            if self.deliver_to_orchestrator(&group, &notice, "loomux").is_ok() {
                notified.push(group);
            }
        }
        notified
    }

    /// Sample free space on the workspace drive (the app-data root, where the
    /// board/state live — the surface a disk-full write corrupts) and run one
    /// `disk_tick`. Best-effort: if the disk can't be read, do nothing.
    pub fn run_disk_monitor(&self) {
        if let Some(free) = free_disk_bytes(&self.root) {
            self.disk_tick(free);
        }
    }


    /// Set a live group's autonomous token budget on the fly (0 = no cap). Written
    /// to the in-memory guardrail (which the idle-tick budget check reads fresh)
    /// and persisted to group.json so a restart keeps it, then audited. Does NOT
    /// move the enable-time anchor — the delta the budget meters is unaffected, so
    /// raising the budget after a suspension lets the human resume without losing
    /// the already-counted spend. Returns the applied value.
    pub fn set_autonomy_budget(&self, group: &str, tokens: u64) -> Result<u64, String> {
        let old = self
            .group(group)
            .ok_or("unknown group")?
            .guardrails
            .autonomy_budget_tokens;
        if tokens == old {
            return Ok(tokens);
        }
        // Persist first: a failed disk write must leave the in-memory value (what
        // the budget check reads) unchanged so the two never disagree.
        self.persist_autonomy_budget(group, tokens)?;
        self.groups
            .lock_safe()
            .get_mut(group)
            .ok_or("unknown group")?
            .guardrails
            .autonomy_budget_tokens = tokens;
        self.audit(group, "human", "autonomy-budget-set",
            json!({ "from": old, "to": tokens }));
        Ok(tokens)
    }

    /// Set a live group's idle-tick quiet window in minutes on the fly (#83). 0 is
    /// coerced to the default and any value floored at 1 (the `autonomous` marker is
    /// the on/off switch — this must never silently disable ticking); clamped to
    /// `MAX_IDLE_TICK_MINUTES`. Written to the in-memory guardrail (the idle-tick
    /// loop reads it fresh each pass) and persisted, then audited. Returns the
    /// applied (clamped) value — lets the human drop it to 1–2 min to verify.
    pub fn set_idle_tick_minutes(&self, group: &str, minutes: u32) -> Result<u32, String> {
        let applied = if minutes == 0 {
            DEFAULT_IDLE_TICK_MINUTES
        } else {
            minutes.clamp(1, MAX_IDLE_TICK_MINUTES)
        };
        let old = self
            .group(group)
            .ok_or("unknown group")?
            .guardrails
            .idle_tick_minutes;
        if applied == old {
            return Ok(applied);
        }
        // Persist first so a failed write leaves the in-memory value (what the loop
        // reads) unchanged.
        self.persist_idle_tick_minutes(group, applied)?;
        self.groups
            .lock_safe()
            .get_mut(group)
            .ok_or("unknown group")?
            .guardrails
            .idle_tick_minutes = applied;
        self.audit(group, "human", "idle-tick-minutes-set",
            json!({ "from": old, "to": applied }));
        Ok(applied)
    }

    /// Rewrite only `guardrails.idle_tick_minutes` in group.json (additive patch).
    fn persist_idle_tick_minutes(&self, group: &str, minutes: u32) -> Result<(), String> {
        self.persist_guardrail_u64(group, "idle_tick_minutes", minutes as u64)
    }

    /// Set a live group's idle-tick activity floor in bytes on the fly (#83, the
    /// rev-59 runtime remedy). 0 → default; floored at 1 and clamped to the max.
    /// Persisted + audited; the idle-tick loop reads it fresh each pass. Returns the
    /// applied (clamped) value — raise it if a chatty CLI's idle repaints starve the
    /// tick, lower it if real small outputs read as idle.
    pub fn set_idle_activity_floor(&self, group: &str, bytes: u64) -> Result<u64, String> {
        let applied = if bytes == 0 {
            DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES
        } else {
            bytes.clamp(1, MAX_IDLE_ACTIVITY_FLOOR_BYTES)
        };
        let old = self
            .group(group)
            .ok_or("unknown group")?
            .guardrails
            .idle_activity_floor_bytes;
        if applied == old {
            return Ok(applied);
        }
        self.persist_guardrail_u64(group, "idle_activity_floor_bytes", applied)?;
        self.groups
            .lock_safe()
            .get_mut(group)
            .ok_or("unknown group")?
            .guardrails
            .idle_activity_floor_bytes = applied;
        self.audit(group, "human", "idle-activity-floor-set",
            json!({ "from": old, "to": applied }));
        Ok(applied)
    }

    /// Rewrite only `guardrails.autonomy_budget_tokens` in group.json, preserving
    /// every other stored field (additive patch, same crash-safe write as
    /// `persist_max_agents`).
    fn persist_autonomy_budget(&self, group: &str, tokens: u64) -> Result<(), String> {
        self.persist_guardrail_u64(group, "autonomy_budget_tokens", tokens)
    }

    /// Additive crash-safe patch of a single numeric `guardrails.<key>` in
    /// group.json (preserves every other field; temp-file + atomic rename, the
    /// `persist_max_agents` pattern). The shared body behind the live-settable
    /// numeric guardrails (budget, activity floor).
    fn persist_guardrail_u64(&self, group: &str, key: &str, value: u64) -> Result<(), String> {
        let dir = self.group_dir(group);
        let path = dir.join("group.json");
        let mut v: Value = serde_json::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let obj = v.as_object_mut().ok_or("group.json root is not a JSON object")?;
        match obj.get_mut("guardrails").and_then(Value::as_object_mut) {
            Some(guard) => {
                guard.insert(key.into(), json!(value));
            }
            None => {
                obj.insert("guardrails".into(), json!({ key: value }));
            }
        }
        let body = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        let tmp = dir.join("group.json.tmp");
        fs::write(&tmp, &body).map_err(|e| e.to_string())?;
        if fs::rename(&tmp, &path).is_err() {
            fs::write(&path, &body).map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&tmp);
        }
        Ok(())
    }

    /// The group's lifetime usage-token total (live + historical snapshots), the
    /// figure the autonomy budget meters against. Reuses `group_usage` so the
    /// live-agent refresh and the exact-token summing live in one place.
    fn group_token_total(&self, group: &str) -> u64 {
        self.group_usage(group)
            .get("lifetime_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    /// The autonomous-mode state the frontend panel reads to render its toggle
    /// rows, the budget meter, and the idle-tick countdown (#83, W2's slice). One
    /// call for the whole panel: on/off, the auto-merge gate, the token budget +
    /// enable-time anchor + spend-since-enable (`null` spend when off), the
    /// idle-tick window, and — while on — how long the orchestrator has been
    /// output-quiet and how many seconds until the next tick is eligible (so the
    /// panel can show "next tick in ~Xm" instead of the tick being invisible).
    pub fn autonomy_state(&self, group: &str) -> Value {
        let on = self.is_autonomous(group);
        let rails = self.group(group).map(|g| g.guardrails);
        let budget = rails.as_ref().map(|g| g.autonomy_budget_tokens).unwrap_or(0);
        let idle_tick_minutes = rails
            .as_ref()
            .map(|g| g.idle_tick_minutes)
            .filter(|&m| m > 0)
            .unwrap_or(DEFAULT_IDLE_TICK_MINUTES);
        let anchor = if on { self.autonomy_anchor(group) } else { 0 };
        let spend = on.then(|| self.group_token_total(group).saturating_sub(anchor));
        // `suspended` is meaningful only while OFF: true iff the budget enforcer
        // (not the user) flipped it off, so the UI can distinguish "budget spent —
        // raise it or re-enable" from a plain user toggle-off.
        let suspended = !on && self.autonomy_suspended(group);
        let floor = rails
            .as_ref()
            .map(|g| g.idle_activity_floor_bytes)
            .filter(|&b| b > 0)
            .unwrap_or(DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES);

        // Idle-tick observability (while on). `tick_status` is the honest reason the
        // UI renders; `eligible_in_secs` is a *real* countdown only when one exists
        // — never a lying 0 that hits zero while a non-time gate holds the tick
        // (rev-59). Statuses:
        //   "off"                 — autonomous off / no live orchestrator (null secs)
        //   "starting"            — orchestrator still booting (not Running); the tick
        //                           only considers Running panes, so no timer yet (null)
        //   "paused"              — group paused: delivery suppressed, so NO tick fires
        //                           however long the clock runs; secs = null (not a lie)
        //   "counting_down"       — quiet clock < window; secs = time left in window
        //   "eligible"            — window met, latch clear, under cap; secs = 0
        //                           (fires within one loop pass, ≤ IDLE_TICK_INTERVAL)
        //   "waiting_for_activity"— already ticked this window (latch set); no timer
        //                           gates it — it waits for the orchestrator to emit
        //                           output — so secs = null, NOT 0
        //   "rate_capped"         — the per-hour cap is full; secs = time until the
        //                           oldest tick ages out of the window (a real timer)
        let (quiet_secs, eligible_in_secs, tick_status) = if on {
            self.idle_tick_observability(group, idle_tick_minutes)
        } else {
            (None, None, "off")
        };
        json!({
            "autonomous": on,
            "auto_merge": self.is_auto_merge(group),
            "auto_release": self.is_auto_release(group),
            "dangerous_mode": self.is_dangerous_mode(group),
            "budget_tokens": budget,
            "budget_anchor_tokens": anchor,
            "spend_since_enable_tokens": spend,
            "suspended": suspended,
            "idle_tick_minutes": idle_tick_minutes,
            "idle_activity_floor_bytes": floor,
            "quiet_secs": quiet_secs,
            "eligible_in_secs": eligible_in_secs,
            "tick_status": tick_status,
        })
    }

    /// Compute the honest idle-tick countdown for `autonomy_state` (#83, rev-59):
    /// `(quiet_secs, eligible_in_secs, tick_status)`, folding the quiet clock, the
    /// one-notice latch, and the per-hour cap so a rendered `eligible_in_secs`
    /// never hits 0 while a non-time gate still holds the tick. See the caller's
    /// status table. `now_ms` is read once here so the arithmetic is self-contained.
    fn idle_tick_observability(
        &self,
        group: &str,
        idle_tick_minutes: u32,
    ) -> (Option<u64>, Option<u64>, &'static str) {
        let now = now_ms();
        // The orchestrator's status + quiet clock + latch (maintained by the loop).
        let orch = self
            .agents
            .lock_safe()
            .values()
            .find(|a| a.group == group && a.role == Role::Orchestrator
                && a.status != AgentStatus::Dead)
            .map(|a| (a.status, a.last_progress_ms, a.idle_tick_notified));
        let Some((status, since, latched)) = orch else {
            return (None, None, "off"); // no live orchestrator → no meter
        };
        // Transient boot: `idle_tick_tick` only ticks a Running orchestrator, so a
        // Starting one has no live countdown yet — report it honestly, not a timer.
        if status != AgentStatus::Running {
            return (None, None, "starting");
        }
        let quiet = now.saturating_sub(since) / 1000;
        let window = idle_tick_minutes as u64 * 60;
        let quiet_remaining = window.saturating_sub(quiet);

        // Paused: `idle_tick_tick` skips paused groups wholesale (delivery is
        // suppressed there), so NO tick fires however long the quiet clock runs.
        // Mirror the latch branch — the quiet clock is still live but there is no
        // countdown — so the panel never shows a ticking timer while paused.
        if self.is_paused(group) {
            return (Some(quiet), None, "paused");
        }
        // Latch: already ticked this window — no timer counts down to the next
        // (it waits for the orchestrator to produce output), so report it as such
        // rather than a false 0.
        if latched {
            return (Some(quiet), None, "waiting_for_activity");
        }
        // Per-hour cap: if the trailing-hour tick count is at the cap, the next
        // tick is gated until the oldest ages out — a real timer, so fold it in.
        let cap_remaining = {
            let times = self.idle_tick_times.lock_safe();
            let recent: Vec<u64> = times
                .get(group)
                .map(|v| v.iter().copied().filter(|&t| now.saturating_sub(t) < SPAWN_RATE_WINDOW_MS).collect())
                .unwrap_or_default();
            if recent.len() as u32 >= MAX_IDLE_TICKS_PER_HOUR {
                let oldest = recent.iter().copied().min().unwrap_or(now);
                Some((oldest + SPAWN_RATE_WINDOW_MS).saturating_sub(now) / 1000)
            } else {
                None
            }
        };
        if let Some(cap_wait) = cap_remaining {
            // Eligible only once BOTH the quiet window and a cap slot are satisfied.
            return (Some(quiet), Some(quiet_remaining.max(cap_wait)), "rate_capped");
        }
        if quiet_remaining > 0 {
            (Some(quiet), Some(quiet_remaining), "counting_down")
        } else {
            (Some(quiet), Some(0), "eligible")
        }
    }

    /// Clear the orchestrator's idle-tick anti-nag latch for a group (e.g. on
    /// disable, so a later re-enable starts fresh).
    fn clear_idle_tick_latch(&self, group: &str) {
        for a in self.agents.lock_safe().values_mut() {
            if a.group == group && a.role == Role::Orchestrator {
                a.idle_tick_notified = false;
            }
        }
    }

    /// Adjust a live group's max live-agent cap on the fly. Bounds are the
    /// launcher's `1..=MAX_AGENTS_CEILING`. The new value is written to the
    /// in-memory guardrail (which `spawn_agent` reads fresh on every spawn, so
    /// it takes effect immediately — nothing caches the creation-time number)
    /// and persisted to group.json so a restart keeps it, then the change is
    /// audited (per-click) and the orchestrator notice is *debounced* — a burst
    /// of stepper clicks coalesces into one re-plan prompt (#79). Lowering the cap below
    /// the current live count kills nobody: new spawns are simply refused until
    /// attrition brings the count back under the cap. Returns the new value.
    /// A no-op change (`n` already the current cap) short-circuits without a
    /// second write, audit, or notice. `actor` records who made the change.
    pub fn set_max_agents(&self, group: &str, n: u32, actor: &str) -> Result<u32, String> {
        if !(1..=MAX_AGENTS_CEILING).contains(&n) {
            return Err(format!("max agents must be between 1 and {MAX_AGENTS_CEILING}"));
        }
        let old = self.group(group).ok_or("unknown group")?.guardrails.max_agents;
        if n == old {
            return Ok(n);
        }
        // Persist first: a failed disk write must leave the in-memory cap (the
        // value enforcement reads) unchanged, so the two never disagree.
        self.persist_max_agents(group, n)?;
        self.groups
            .lock_safe()
            .get_mut(group)
            .ok_or("unknown group")?
            .guardrails
            .max_agents = n;
        self.audit(group, actor, "max-agents-set", json!({ "from": old, "to": n }));
        // The orchestrator's kickoff prompt already rendered the old
        // {{MAX_AGENTS}} into static text; it needs the new ceiling to re-plan.
        // But rapid-clicking the stepper (4→3→2) would otherwise fire a notice
        // per click, each a real prompt that burns orchestrator tokens/time
        // (#79). So debounce: record the change here (carrying the burst's
        // original `from`) and let `flush_due_max_notices` deliver ONE notice —
        // 4→2, not 4→3 then 3→2 — once the clicks stop. Enforcement/persist
        // above and the audit are per-click and immediate; only the notice waits.
        record_max_notice(
            &mut self.pending_max_notice.lock_safe(),
            group,
            old,
            n,
            now_ms(),
            MAX_NOTICE_DEBOUNCE,
        );
        Ok(n)
    }

    /// Rewrite only `guardrails.max_agents` in group.json, preserving every
    /// other stored field (created_ms, the other guardrails, and anything a
    /// later feature adds). Patching the parsed JSON in place — rather than
    /// reserializing a full GroupInfo — keeps this additive and rebase-clean.
    fn persist_max_agents(&self, group: &str, n: u32) -> Result<(), String> {
        let dir = self.group_dir(group);
        let path = dir.join("group.json");
        let mut v: Value = serde_json::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        // Guard the indexing so a corrupt-but-valid-JSON file (e.g. a `null`
        // root) fails soft instead of panicking on assignment.
        let obj = v.as_object_mut().ok_or("group.json root is not a JSON object")?;
        match obj.get_mut("guardrails").and_then(Value::as_object_mut) {
            Some(guard) => {
                guard.insert("max_agents".into(), json!(n));
            }
            None => {
                obj.insert("guardrails".into(), json!({ "max_agents": n }));
            }
        }
        // Crash-safe write: group.json is identity-critical — a half-written
        // file breaks the rejoin path ("group.json is missing") — so never
        // expose a truncated version (#133).
        let body = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        atomic_write(&path, body.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Deliver any debounced cap-change notice whose quiet window has elapsed
    /// (#79). Called on a timer by `start_max_notice_flusher`; `now` is injected
    /// so tests drive the coalescing deterministically without sleeping out the
    /// debounce. A burst that netted to a no-op is dropped inside
    /// `take_due_max_notices` and never reaches the orchestrator.
    #[doc(hidden)] // pub for integration tests
    pub fn flush_due_max_notices(&self, now: u64) {
        let due = take_due_max_notices(&mut self.pending_max_notice.lock_safe(), now);
        for (group, from, to) in due {
            // Best-effort, like the exit notice: a dead/paused orchestrator
            // just misses it. Delivery is intentionally outside the lock.
            let _ = self.deliver_to_orchestrator(&group, &max_agents_notice(from, to), "loomux");
        }
    }

    /// Read every agent pane's output counter, last-lines tail, and last human
    /// keystroke time — the raw inputs `attention_tick` needs. Empty without an
    /// app handle (unit tests drive `attention_tick` with synthetic maps).
    fn attention_inputs(&self) -> (HashMap<String, u64>, HashMap<String, String>, HashMap<String, u64>) {
        let mut outs = HashMap::new();
        let mut tails = HashMap::new();
        let mut ins = HashMap::new();
        let Some(app) = self.app.lock_safe().clone() else {
            return (outs, tails, ins);
        };
        let ptys = app.state::<crate::pty::PtyManager>();
        for a in self.agents.lock_safe().values() {
            let Some(pid) = a.pty_id else { continue };
            if let Some(t) = ptys.output_total(pid) {
                outs.insert(a.id.clone(), t);
            }
            if let Some(raw) = ptys.output_tail(pid) {
                // A prompt is at the very end; strip only the last few KB so
                // the scan stays cheap against a saturated 256 KB ring.
                let start = raw.len().saturating_sub(4096);
                tails.insert(a.id.clone(), strip_ansi(&raw[start..]));
            }
            if let Some(u) = ptys.last_user_input_ms(pid) {
                ins.insert(a.id.clone(), u);
            }
        }
        (outs, tails, ins)
    }

    /// Pty snapshots for every live pane that is NOT a registered agent, keyed
    /// by pty id, plus the agent-pty set. Feeds `plain_pane_attention` so the
    /// scan reaches plain shells the human opened by hand (#40). Empty without an
    /// app handle (unit tests drive the pure core `pane_attention_inputs_from`).
    #[allow(clippy::type_complexity)]
    fn pane_attention_inputs(
        &self,
    ) -> (HashMap<u32, u64>, HashMap<u32, String>, HashMap<u32, u64>, HashSet<u32>) {
        let mut agent_ptys = HashSet::new();
        let Some(app) = self.app.lock_safe().clone() else {
            return (HashMap::new(), HashMap::new(), HashMap::new(), agent_ptys);
        };
        for a in self.agents.lock_safe().values() {
            if let Some(pid) = a.pty_id {
                agent_ptys.insert(pid);
            }
        }
        let ptys = app.state::<crate::pty::PtyManager>();
        let mut live = Vec::new();
        for pid in ptys.live_ids() {
            // Skip agent ptys *before* touching the ring: `attention_tick`
            // already covers them, and `attention_inputs` already cloned their
            // (up-to-256 KB) output ring this tick — cloning it a second time
            // here would be pure waste (#40 review).
            if agent_ptys.contains(&pid) {
                continue;
            }
            let Some(total) = ptys.output_total(pid) else { continue };
            let raw = ptys.output_tail(pid).unwrap_or_default();
            let input = ptys.last_user_input_ms(pid).unwrap_or(0);
            live.push((pid, total, raw, input));
        }
        let (outs, tails, ins) = self.pane_attention_inputs_from(&live, &agent_ptys);
        (outs, tails, ins, agent_ptys)
    }

    /// Pure core of `pane_attention_inputs`: build the pty-keyed snapshot maps
    /// `plain_pane_attention` consumes from a list of live pane snapshots
    /// `(pty_id, output_total, raw_tail, last_input_ms)`, ANSI-stripping only the
    /// trailing few KB of each tail (a prompt is at the end). Agent ptys are
    /// skipped. Pure w.r.t. the pty, so run_attention's gather wiring is testable
    /// with a fake live-ids source (#40 review).
    #[allow(clippy::type_complexity)]
    pub fn pane_attention_inputs_from(
        &self,
        live: &[(u32, u64, Vec<u8>, u64)],
        agent_ptys: &HashSet<u32>,
    ) -> (HashMap<u32, u64>, HashMap<u32, String>, HashMap<u32, u64>) {
        let mut outs = HashMap::new();
        let mut tails = HashMap::new();
        let mut ins = HashMap::new();
        for (pid, total, raw, input) in live {
            if agent_ptys.contains(pid) {
                continue;
            }
            outs.insert(*pid, *total);
            let start = raw.len().saturating_sub(4096);
            tails.insert(*pid, strip_ansi(&raw[start..]));
            ins.insert(*pid, *input);
        }
        (outs, tails, ins)
    }

    /// One attention pass: compute the current set of panes that need the human
    /// from live agent state plus the supplied pty snapshots. Reasons, in
    /// priority order, are `blocked` (reported), `waiting` (parked on a prompt:
    /// output quiet past `ATTENTION_QUIET_MS`, a prompt-shaped tail, and no
    /// recent human keystroke), `report` (reported done), and `gate` (this
    /// agent's board task sits at a `pr`/`human-testing`/`blocked` merge gate).
    /// Pure w.r.t. the OS/pty — the pty reads live in `attention_inputs` — so
    /// the whole policy is testable with synthetic maps and no real CLI.
    pub fn attention_tick(
        &self,
        now: u64,
        outputs: &HashMap<String, u64>,
        tails: &HashMap<String, String>,
        last_inputs: &HashMap<String, u64>,
    ) -> Vec<AttentionItem> {
        // Board-derived gate map: agent id → gate status, across every live
        // group. Read once per group (a small fs read) rather than per agent.
        let groups: HashSet<String> =
            self.agents.lock_safe().values().map(|a| a.group.clone()).collect();
        let mut gate_of: HashMap<String, String> = HashMap::new();
        for g in &groups {
            for t in self.tasks(g) {
                // `prototype` is a human gate too (#147): the assigned pane is
                // where the pending demo-verdict work lives, so flag it like the
                // merge gates and `blocked`.
                let is_gate = MERGE_GATE_STATUSES.contains(&t.status.as_str())
                    || t.status == "blocked"
                    || t.status == PROTOTYPE_STATUS;
                if is_gate {
                    if let Some(assignee) = t.assignee.filter(|s| !s.trim().is_empty()) {
                        gate_of.insert(assignee, t.status);
                    }
                }
            }
        }

        let reports = self.attn_reports.lock_safe().clone();
        let mut quiet = self.attn_quiet.lock_safe();
        let mut waiting_ack = self.attn_waiting_ack.lock_safe();
        let agents = self.agents.lock_safe();
        let mut out = Vec::new();
        for a in agents.values() {
            if a.status != AgentStatus::Running {
                quiet.remove(&a.id);
                waiting_ack.remove(&a.id);
                continue;
            }
            // Track how long the pane's output has been stable.
            let cur = outputs.get(&a.id).copied().unwrap_or(0);
            let entry = quiet.entry(a.id.clone()).or_insert((cur, now));
            let output_changed = cur != entry.0;
            if output_changed {
                *entry = (cur, now);
                // The pane repainted — the acked menu was answered or replaced,
                // so re-arm: a fresh prompt on this pane flags again.
                waiting_ack.remove(&a.id);
            }
            let quiet_for = now.saturating_sub(entry.1);
            let recently_typed = last_inputs
                .get(&a.id)
                .map(|&t| t != 0 && now.saturating_sub(t) < ATTENTION_RECENT_INPUT_MS)
                .unwrap_or(false);
            let waiting = !recently_typed
                && !waiting_ack.contains(&a.id)
                && quiet_for >= ATTENTION_QUIET_MS
                && tails.get(&a.id).map(|t| prompt_wait_detected(t)).unwrap_or(false);

            let report = reports.get(a.id.as_str()).copied();
            let (reason, detail): (&'static str, String) = if report == Some("blocked") {
                ("blocked", format!("{} reported blocked — it needs you", a.name))
            } else if waiting {
                ("waiting", format!("{} is waiting on a prompt", a.name))
            } else if report == Some("done") {
                ("report", format!("{} reported done — review & merge", a.name))
            } else if let Some(st) = gate_of.get(a.id.as_str()) {
                ("gate", format!("task is {st} — awaiting your call"))
            } else {
                continue;
            };
            out.push(AttentionItem {
                agent_id: a.id.clone(),
                group: a.group.clone(),
                name: a.name.clone(),
                role: Some(a.role),
                pty_id: a.pty_id,
                reason,
                detail,
            });
        }
        out.sort_by(|x, y| x.agent_id.cmp(&y.agent_id));
        out
    }

    /// Attention scan for *plain* panes (#40): any pane with a live pty that is
    /// **not** a registered agent — the shells the human opens by hand to run a
    /// CLI. It only ever raises `waiting` (parked on an interactive prompt): the
    /// agent-only reasons (`blocked`/`report`/`gate`) require a roster identity a
    /// plain pane doesn't have. Same quiet + no-keystroke + prompt-tail gate and
    /// the same sticky-ack semantics as the agent path, keyed by a synthetic
    /// `pty:<id>` id in the shared `attn_quiet`/`attn_waiting_ack` maps (agent
    /// ids never collide — they're group-scoped uuids). Pure w.r.t. the pty (the
    /// pty reads live in `pane_attention_inputs`), so it's fixture-testable.
    /// `agent_ptys` are the ptys already handled by `attention_tick`, skipped here.
    pub fn plain_pane_attention(
        &self,
        now: u64,
        outputs: &HashMap<u32, u64>,
        tails: &HashMap<u32, String>,
        last_inputs: &HashMap<u32, u64>,
        agent_ptys: &HashSet<u32>,
    ) -> Vec<AttentionItem> {
        let mut quiet = self.attn_quiet.lock_safe();
        let mut waiting_ack = self.attn_waiting_ack.lock_safe();
        let mut out = Vec::new();
        for (&pty, &cur) in outputs {
            if agent_ptys.contains(&pty) {
                continue;
            }
            let key = format!("pty:{pty}");
            let entry = quiet.entry(key.clone()).or_insert((cur, now));
            if cur != entry.0 {
                *entry = (cur, now);
                waiting_ack.remove(&key); // repainted → re-arm (menu answered)
            }
            let quiet_for = now.saturating_sub(entry.1);
            let recently_typed = last_inputs
                .get(&pty)
                .map(|&t| t != 0 && now.saturating_sub(t) < ATTENTION_RECENT_INPUT_MS)
                .unwrap_or(false);
            let waiting = !recently_typed
                && !waiting_ack.contains(&key)
                && quiet_for >= ATTENTION_QUIET_MS
                && tails.get(&pty).map(|t| prompt_wait_detected(t)).unwrap_or(false);
            if waiting {
                out.push(AttentionItem {
                    agent_id: String::new(),
                    group: String::new(),
                    name: String::new(),
                    role: None,
                    pty_id: Some(pty),
                    reason: "waiting",
                    detail: "This pane is waiting on your input".to_string(),
                });
            }
        }
        // Prune bookkeeping for ptys that have gone away (pane closed), so the
        // shared maps don't grow unbounded with `pty:` keys.
        quiet.retain(|k, _| !k.starts_with("pty:") || k[4..].parse::<u32>().map(|p| outputs.contains_key(&p)).unwrap_or(false));
        waiting_ack.retain(|k| !k.starts_with("pty:") || k[4..].parse::<u32>().map(|p| outputs.contains_key(&p)).unwrap_or(false));
        out.sort_by_key(|i| i.pty_id);
        out
    }

    /// Decide which current attention items warrant a fresh desktop toast:
    /// their group opted in, the reason is an event (not the persistent `gate`
    /// board state, which the board highlight already surfaces), and this
    /// (agent, reason) hasn't been toasted yet. Records only what actually
    /// fires — so enabling notifications surfaces already-pending attention —
    /// and prunes cleared/changed entries so a fresh onset toasts again.
    /// Returns the agent ids to toast; pure w.r.t. the OS, so the policy is
    /// testable without firing a real notification.
    pub fn attention_toast_targets(&self, items: &[AttentionItem]) -> Vec<String> {
        let notify = self.notify_groups.lock_safe().clone();
        let mut toasted = self.attn_emitted.lock_safe();
        let mut fire = Vec::new();
        for i in items {
            let already = toasted.get(&i.agent_id).map(|p| p == i.reason).unwrap_or(false);
            if !already && i.reason != "gate" && notify.contains(&i.group) {
                fire.push(i.agent_id.clone());
                toasted.insert(i.agent_id.clone(), i.reason.to_string());
            }
        }
        // Drop ledger entries whose attention cleared or whose reason changed,
        // so the same pane can toast again on a genuinely new onset.
        let current: HashMap<&str, &str> =
            items.iter().map(|i| (i.agent_id.as_str(), i.reason)).collect();
        toasted.retain(|id, reason| current.get(id.as_str()) == Some(&reason.as_str()));
        fire
    }

    /// One full attention cycle: read pty snapshots, compute the attention set,
    /// fire toasts for newly-attention panes in opted-in groups, and push the
    /// whole set to the frontend. Called on a timer by `start_attention`.
    /// The full attention set: the roster scan (`attention_tick`, all reasons)
    /// merged with the plain-pane scan (`plain_pane_attention`, `waiting` only).
    /// This is run_attention's core, factored out so the merge wiring — plain
    /// panes surface, an agent's pty is never double-covered — is testable
    /// without a real PtyManager (#40 review).
    #[allow(clippy::too_many_arguments)]
    pub fn attention_scan(
        &self,
        now: u64,
        agent_outputs: &HashMap<String, u64>,
        agent_tails: &HashMap<String, String>,
        agent_inputs: &HashMap<String, u64>,
        pane_outputs: &HashMap<u32, u64>,
        pane_tails: &HashMap<u32, String>,
        pane_inputs: &HashMap<u32, u64>,
        agent_ptys: &HashSet<u32>,
    ) -> Vec<AttentionItem> {
        let mut items = self.attention_tick(now, agent_outputs, agent_tails, agent_inputs);
        items.extend(self.plain_pane_attention(now, pane_outputs, pane_tails, pane_inputs, agent_ptys));
        items
    }

    pub fn run_attention(&self, now: u64) {
        let (outputs, tails, last_inputs) = self.attention_inputs();
        // Also scan plain (non-agent) panes for an interactive prompt (#40).
        let (p_out, p_tails, p_ins, agent_ptys) = self.pane_attention_inputs();
        let items = self.attention_scan(
            now, &outputs, &tails, &last_inputs, &p_out, &p_tails, &p_ins, &agent_ptys,
        );
        for id in self.attention_toast_targets(&items) {
            if let Some(i) = items.iter().find(|i| i.agent_id == id) {
                self.audit(&i.group, "loomux", "attention-toast",
                    json!({ "agent": i.agent_id, "reason": i.reason }));
                notify_desktop(&format!("loomux · {}", i.name), &i.detail);
            }
        }
        if let Some(app) = self.app.lock_safe().clone() {
            let _ = app.emit("orch-attention", &items);
        }
    }

    /// Record a spawn against the group's rolling-hour window and report
    /// whether the spawn-rate guardrail is now exceeded. Checks and records
    /// under one lock so concurrent spawns can't both slip past the cap.
    fn check_and_record_spawn(&self, group: &str, limit: u32) -> Result<(), String> {
        let now = now_ms();
        let mut all = self.spawn_times.lock_safe();
        let times = all.entry(group.to_string()).or_default();
        times.retain(|&t| now.saturating_sub(t) < SPAWN_RATE_WINDOW_MS);
        if spawn_rate_exceeded(times, now, limit, SPAWN_RATE_WINDOW_MS) {
            return Err(format!(
                "guardrail: spawn-rate limit reached ({limit} spawns/hour). Wait, or reuse an idle agent instead of spawning a new one."
            ));
        }
        times.push(now);
        Ok(())
    }

    /// Compute an agent's current usage from the best available source, in
    /// preference order: the CLI's own session transcript (token records —
    /// exact, and readable even after the pane is gone) → a last-resort parse
    /// of the dollar figure the CLI prints in its statusline. Returns a
    /// snapshot keyed for durable accumulation (issue #42).
    fn compute_usage_snapshot(&self, entry: &AgentEntry, cli: &str) -> UsageSnapshot {
        let key = entry
            .session_id
            .clone()
            .unwrap_or_else(|| format!("agent:{}", entry.id));
        let role = entry.role.as_str();
        let mut snap = UsageSnapshot {
            key,
            agent_id: entry.id.clone(),
            name: entry.name.clone(),
            role: role.to_string(),
            source: "none".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cost_usd: None,
            estimated: false,
            model: None,
            updated_ms: now_ms(),
        };

        // Primary source: per-session token usage from the transcript. Claude
        // Code writes it; Copilot has no readable token record today (see the
        // `usage` module's design note), so it falls through to the statusline.
        if cli == "claude" {
            if let Some(sid) = entry.session_id.as_deref() {
                // Use the test override when set, else the default ~/.claude root.
                let root = self
                    .claude_projects_dir
                    .lock_safe()
                    .clone()
                    .or_else(crate::usage::default_claude_projects_root);
                if let Some(u) = root
                    .as_deref()
                    .and_then(|r| crate::usage::claude_session_usage_in(r, sid))
                {
                    if u.tokens.total() > 0 {
                        snap.source = "transcript".to_string();
                        snap.input_tokens = u.tokens.input_tokens;
                        snap.output_tokens = u.tokens.output_tokens;
                        snap.cache_creation_tokens = u.tokens.cache_creation_tokens;
                        snap.cache_read_tokens = u.tokens.cache_read_tokens;
                        snap.cost_usd = u.cost_usd;
                        snap.estimated = true; // token-derived dollar estimate
                        snap.model = u.model;
                        return snap;
                    }
                }
            }
        }

        // Last resort: the dollar figure the CLI renders in its own statusline.
        // Unreliable (empty on subscription/Max accounts; gone once the pane is
        // killed), so it only runs when no transcript usage was found.
        if let Some(app) = self.app.lock_safe().clone() {
            if let Some(pty) = entry.pty_id {
                let ptys = app.state::<crate::pty::PtyManager>();
                if let Some(raw) = ptys.output_tail(pty) {
                    if let Some(c) = parse_session_cost(&strip_ansi(&raw)) {
                        snap.source = "statusline".to_string();
                        snap.cost_usd = Some(c); // reported by the CLI, not estimated
                    }
                }
            }
        }
        snap
    }

    fn load_usage_snapshots(&self, group: &str) -> Vec<UsageSnapshot> {
        let path = self.group_dir(group).join("usage.json");
        let Ok(text) = fs::read_to_string(&path) else {
            return Vec::new(); // absent is normal (no usage yet)
        };
        match serde_json::from_str(&text) {
            Ok(list) => list,
            Err(e) => {
                // The file exists but is corrupt (interrupted write, manual
                // edit). Silently treating it as empty would wipe all
                // killed-agent history, so preserve it for inspection and
                // start fresh rather than overwrite it on the next upsert.
                let bad = path.with_extension("json.bad");
                let _ = fs::rename(&path, &bad);
                self.audit(group, "loomux", "usage-corrupt",
                    json!({ "error": e.to_string(), "preserved": bad.to_string_lossy() }));
                Vec::new()
            }
        }
    }

    /// Upsert one agent's snapshot into the group's durable `usage.json`,
    /// matched by `key`. Shares the task-board file lock. Public for the
    /// kill-snapshot accumulation test.
    #[doc(hidden)]
    pub fn upsert_usage_snapshot(&self, group: &str, snap: UsageSnapshot) {
        let _guard = self.tasks_lock.lock_safe();
        let mut list = self.load_usage_snapshots(group);
        match list.iter_mut().find(|s| s.key == snap.key) {
            Some(existing) => {
                // A transcript only ever grows, so a read that comes back empty
                // (e.g. transient failure, or the pane died before Copilot wrote
                // a token record) must not clobber usage we already captured —
                // otherwise a kill could zero a session's spend. Refresh the
                // identity fields but keep the richer usage.
                let new_empty = snap.source == "none"
                    && snap.cost_usd.is_none()
                    && snap.input_tokens
                        + snap.output_tokens
                        + snap.cache_creation_tokens
                        + snap.cache_read_tokens
                        == 0;
                let old_has_data = existing.source != "none"
                    || existing.cost_usd.is_some()
                    || existing.input_tokens
                        + existing.output_tokens
                        + existing.cache_creation_tokens
                        + existing.cache_read_tokens
                        > 0;
                if new_empty && old_has_data {
                    existing.agent_id = snap.agent_id;
                    existing.name = snap.name;
                    existing.role = snap.role;
                    existing.updated_ms = snap.updated_ms;
                } else {
                    *existing = snap;
                }
            }
            None => list.push(snap),
        }
        let dir = self.group_dir(group);
        let _ = fs::create_dir_all(&dir);
        // Crash-safe write: a crash mid-write leaves the old (valid) file
        // intact, never a half-written usage.json (#133). Holds `tasks_lock`.
        let body = serde_json::to_string_pretty(&list).unwrap();
        let _ = atomic_write(&dir.join("usage.json"), body.as_bytes());
    }

    /// Aggregate the group's usage into one summary with a **live vs lifetime**
    /// split. Live agents' snapshots are refreshed from their transcripts on
    /// each call; killed/recycled agents keep the snapshot captured when they
    /// exited, so the lifetime total never forgets historical spend. Tokens are
    /// exact; dollar figures are estimates (labelled per agent).
    pub fn group_usage(&self, group: &str) -> Value {
        let live_agents: Vec<AgentEntry> = self
            .agents
            .lock_safe()
            .values()
            .filter(|a| a.group == group && a.status != AgentStatus::Dead)
            .cloned()
            .collect();
        // Each agent's CLI is per-role (issue #4), so resolve it per agent.
        // The group-level `cli` in the summary is the group default (workers/
        // reviewers/planners may each run a different one).
        let rails = self.group(group).map(|g| g.guardrails);
        let cli = rails
            .as_ref()
            .map(|g| g.agent_cli.clone())
            .unwrap_or_else(|| "claude".to_string());

        // Refresh each live agent's durable snapshot from its current usage.
        let mut live_keys: HashSet<String> = HashSet::new();
        for a in &live_agents {
            let cli = rails.as_ref().map(|g| g.cli_for(a.role)).unwrap_or("claude");
            let snap = self.compute_usage_snapshot(a, cli);
            live_keys.insert(snap.key.clone());
            self.upsert_usage_snapshot(group, snap);
        }

        // The store now holds live + historical (killed) snapshots.
        let snaps = {
            let _guard = self.tasks_lock.lock_safe();
            self.load_usage_snapshots(group)
        };

        let (mut live_cost, mut lifetime_cost) = (0.0f64, 0.0f64);
        let (mut live_cost_known, mut lifetime_cost_known) = (false, false);
        let (mut live_tokens, mut lifetime_tokens) = (0u64, 0u64);
        // Track whether each total mixes token-estimated and CLI-reported
        // dollars, so we never blend them under one honest label.
        let (mut live_est, mut live_rep) = (false, false);
        let (mut lifetime_est, mut lifetime_rep) = (false, false);
        let mut rows: Vec<Value> = Vec::new();

        for s in &snaps {
            let tokens = s.input_tokens
                + s.output_tokens
                + s.cache_creation_tokens
                + s.cache_read_tokens;
            let live = live_keys.contains(&s.key);
            lifetime_tokens += tokens;
            if let Some(c) = s.cost_usd {
                lifetime_cost += c;
                lifetime_cost_known = true;
                if s.estimated {
                    lifetime_est = true;
                } else {
                    lifetime_rep = true;
                }
            }
            if live {
                live_tokens += tokens;
                if let Some(c) = s.cost_usd {
                    live_cost += c;
                    live_cost_known = true;
                    if s.estimated {
                        live_est = true;
                    } else {
                        live_rep = true;
                    }
                }
            }
            rows.push(json!({
                "id": s.agent_id,
                "name": s.name,
                "role": s.role,
                "live": live,
                "source": s.source,
                "model": s.model,
                "cost_usd": s.cost_usd,
                "estimated": s.estimated,
                "tokens": {
                    "input": s.input_tokens,
                    "output": s.output_tokens,
                    "cache_creation": s.cache_creation_tokens,
                    "cache_read": s.cache_read_tokens,
                    "total": tokens,
                },
            }));
        }
        rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));

        // How to label each dollar total: all token-estimated, all
        // CLI-reported, or a mix. `null` when there is no cost figure.
        let basis = |est: bool, rep: bool| -> Option<&'static str> {
            match (est, rep) {
                (true, true) => Some("mixed"),
                (true, false) => Some("estimated"),
                (false, true) => Some("reported"),
                (false, false) => None,
            }
        };

        json!({
            "group": group,
            "cli": cli,
            "live_cost_usd": live_cost_known.then_some(live_cost),
            "lifetime_cost_usd": lifetime_cost_known.then_some(lifetime_cost),
            "live_cost_basis": basis(live_est, live_rep),
            "lifetime_cost_basis": basis(lifetime_est, lifetime_rep),
            "live_tokens": live_tokens,
            "lifetime_tokens": lifetime_tokens,
            "agents": rows,
            "note": "Tokens come from each agent's session transcript and are exact; dollar figures are estimated from a dated model price table. Subscription/Max accounts have no marginal dollar cost (the CLI statusline shows $0.00), so tokens are the reliable metric. Killed/recycled agents stay in the lifetime total; statusline-parsed dollars are a last-resort fallback.",
        })
    }

    // ---------- lifecycle: group summary & end-orchestration ----------

    /// A one-glance summary of a group's live agents for the lifecycle panel:
    /// how many are up, the role breakdown, and uptime (per agent and for the
    /// group as a whole, measured from the earliest-started live agent — the
    /// orchestrator in practice). Also reports the paused flag so the panel can
    /// compose pause and end-orchestration sanely.
    pub fn group_summary(&self, group: &str) -> Value {
        let now = now_ms();
        let live: Vec<AgentEntry> = self
            .agents
            .lock_safe()
            .values()
            .filter(|a| a.group == group && a.status != AgentStatus::Dead)
            .cloned()
            .collect();
        let (mut orch, mut worker, mut reviewer, mut planner) = (0u32, 0u32, 0u32, 0u32);
        let mut earliest: Option<u64> = None;
        let mut list: Vec<Value> = live
            .iter()
            .map(|a| {
                match a.role {
                    Role::Orchestrator => orch += 1,
                    Role::Worker => worker += 1,
                    Role::Reviewer => reviewer += 1,
                    Role::Planner => planner += 1,
                }
                earliest = Some(earliest.map_or(a.started_ms, |e| e.min(a.started_ms)));
                json!({
                    "id": a.id, "name": a.name, "role": a.role,
                    // The block this agent IS (#222). Equal to the role for the
                    // built-in roster, so the group panel shows nothing new for a
                    // default group — and shows `rev-security` rather than a
                    // second anonymous "REV" chip for a workflow group, which is
                    // the whole point of declaring the reviewers separately.
                    "block": a.block,
                    "task": a.task, "idle_since_ms": a.idle_since_ms,
                    "uptime_ms": now.saturating_sub(a.started_ms),
                })
            })
            .collect();
        list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        json!({
            "group": group,
            "live_agents": live.len(),
            // The current adjustable cap and how many delegates count against
            // it, so the UI can show the stepper's value and warn when a lower
            // cap would (harmlessly) block spawns until attrition. Must match
            // `live_delegate_count` (the value enforcement actually reads):
            // planners count too (#47), so a cap-below-live warning stays honest.
            "max_agents": self.group(group).map(|g| g.guardrails.max_agents),
            "live_delegates": worker + reviewer + planner,
            "paused": self.is_paused(group),
            "uptime_ms": earliest.map(|e| now.saturating_sub(e)),
            "roles": { "orchestrator": orch, "worker": worker, "reviewer": reviewer, "planner": planner },
            "agents": list,
        })
    }

    /// End a whole orchestration: kill every one of the group's agents (the
    /// orchestrator included — unlike `kill_agent`, which protects it), and,
    /// when asked, remove the agents' worktrees. Human-initiated and
    /// destructive: it is a Tauri command only (never an MCP tool an agent
    /// could call on itself), audited as actor `human`, and the frontend
    /// confirms before invoking. Composes with a paused group: killing works
    /// regardless of pause, and the pause marker is cleared so the teardown is
    /// total — a later relaunch on the same repo won't inherit a stale pause.
    pub fn end_group(&self, group: &str, cleanup_worktrees: bool) -> Result<Value, String> {
        // Snapshot every member (all statuses): already-dead workers may still
        // have a worktree on disk that cleanup should reclaim.
        let members: Vec<AgentEntry> = self
            .agents
            .lock_safe()
            .values()
            .filter(|a| a.group == group)
            .cloned()
            .collect();
        if members.is_empty() {
            return Err("no such group (no agents ever registered here)".into());
        }
        let app = self.app.lock_safe().clone();

        // Kill the live ones. Kill the pty (best-effort) then mark the entry
        // dead directly — mark_dead is idempotent against the async pty-exit,
        // and going straight through it avoids the orchestrator-notification
        // path in on_pty_exit (there is no orchestrator left to tell).
        let mut killed = Vec::new();
        for a in &members {
            if a.status == AgentStatus::Dead {
                continue;
            }
            if let (Some(app), Some(pty)) = (app.as_ref(), a.pty_id) {
                app.state::<crate::pty::PtyManager>().kill(pty);
            }
            self.mark_dead(&a.id, None);
            killed.push(a.id.clone());
        }

        // Optionally reclaim the worktrees. Resolve the repo (from memory or
        // group.json) so `git worktree remove` runs from the main checkout.
        let mut worktrees_removed = Vec::new();
        let mut worktree_errors = Vec::new();
        if cleanup_worktrees {
            let repo = self
                .group(group)
                .map(|g| g.repo)
                .or_else(|| self.load_group_file(group).map(|(r, _)| r));
            if let Some(repo) = repo {
                let cwds: Vec<String> = members.iter().map(|a| a.cwd.clone()).collect();
                for path in worktree_cleanup_targets(&repo, &cwds) {
                    match crate::git::git_worktree_remove(&repo, &path) {
                        Ok(()) => worktrees_removed.push(path),
                        Err(e) => worktree_errors.push(json!({ "path": path, "error": e })),
                    }
                }
            }
        }

        // Sweep the steering-strip image attachments (#72): they're only useful
        // while the group's agents are live, so teardown reclaims the scratch
        // dir alongside the worktrees. Best-effort — a leftover screenshot must
        // never block a group from ending. This includes any that were queued
        // but never sent (removed chips / abandoned drafts), so the cheap
        // policy is simply "cleaned up on group end", no per-image bookkeeping.
        let _ = fs::remove_dir_all(self.attachments_dir(group));

        // Total teardown: drop any pause (in-memory + marker) so a future
        // relaunch on this repo starts clean rather than silently paused.
        if self.paused.lock_safe().remove(group) {
            let _ = fs::remove_file(self.group_dir(group).join("paused"));
        }

        self.audit(group, "human", "group-end", json!({
            "killed": killed,
            "cleanup_worktrees": cleanup_worktrees,
            "worktrees_removed": worktrees_removed,
            "worktree_errors": worktree_errors,
        }));

        // Tell the frontend to close the group's (now-dead) panes so the human
        // isn't left ✕-clicking a screen of dead terminals — the very chore
        // this action exists to remove.
        if let Some(app) = app.as_ref() {
            let _ = app.emit("orch-group-ended", json!({ "group_id": group }));
        }

        Ok(json!({
            "group": group,
            "killed": killed,
            "worktrees_removed": worktrees_removed,
            "worktree_errors": worktree_errors,
        }))
    }

    /// The orchestrator's **This repo declares a workflow** section, or `""` for
    /// the default roster (#222).
    ///
    /// It tells the orchestrator the three things the file actually changes about
    /// its job — spawn by block id rather than by kind, run *every* declared
    /// reviewer on each PR rather than one, and treat a declared gate as a hard
    /// precondition — and the one thing it does not: the edges are advisory, and
    /// the scheduling judgment stays the orchestrator's. See doc/design/workflows.md
    /// ("Why edges are advisory") for why that asymmetry is the whole design.
    ///
    /// `{{MAX_AGENTS}}` is rendered here rather than left to the caller: the
    /// caller substitutes this text *into* the orchestrator template, and by then
    /// its own `MAX_AGENTS` pass has already gone by.
    fn workflow_section(&self, g: &GroupInfo) -> String {
        if !workflow::roster_is_custom(&g.guardrails.blocks) {
            return String::new();
        }
        let cli = &g.guardrails.agent_cli;
        let rows: Vec<String> = g
            .guardrails
            .blocks
            .iter()
            .filter(|b| b.kind != Role::Orchestrator)
            .map(|b| {
                format!(
                    "- **`{id}`** — {name} · {kind} · {cli} · model `{model}`{persona}",
                    id = b.id,
                    name = b.name,
                    kind = b.kind.as_str(),
                    cli = workflow::cli_of(b, cli),
                    model = workflow::model_of(b, cli),
                    persona = if b.has_persona() { " · has a persona" } else { "" },
                )
            })
            .collect();
        let reviewers: Vec<String> = g
            .guardrails
            .blocks
            .iter()
            .filter(|b| b.kind == Role::Reviewer)
            .map(|b| format!("`{}`", b.id))
            .collect();
        // Leading blank line, and the fragment's own trailing one trimmed: the
        // placeholder sits at the END of the preceding sentence in the template
        // (never on a line of its own), which is exactly what lets the empty case
        // above leave the file untouched to the byte.
        format!(
            "\n\n{}",
            render_template(
            WORKFLOW_TPL,
            &[
                ("WORKFLOW_PATH", workflow::WORKFLOW_PATH),
                ("MAX_AGENTS", &g.guardrails.max_agents.to_string()),
                // A roster can legally declare no reviewer at all (a build-only
                // workflow). Say that, rather than emitting an empty list and
                // leaving the sentence dangling.
                (
                    "REVIEWERS",
                    &if reviewers.is_empty() {
                        "— this workflow declares no reviewer block, so there is nobody to fan out to; \
                         tell the human if a PR looks like it needs review"
                            .to_string()
                    } else {
                        reviewers.join(", ")
                    },
                ),
                ("BLOCKS", &rows.join("\n")),
            ],
            )
            .trim_end()
        )
    }

    /// A delegate's **Your block** section, or `""` when the workflow file did not
    /// touch this block (#222) — which is every block of the default roster, and
    /// is why a no-workflow group's `worker.md` is the pre-#222 file to the byte.
    ///
    /// Emitted per block, not per group: a plain built-in `worker` block sitting
    /// in a roster whose *reviewers* are custom has had nothing about its own
    /// identity changed, and telling it otherwise is noise in a file agents are
    /// expected to actually read. The one exception is a reviewer with siblings —
    /// being one of several focused reviewers *is* a change to how it should
    /// review, so it gets the lane note even with no persona of its own.
    ///
    /// Not reached for a `replace`-mode persona: that block's file is the
    /// non-overridable mechanics core, which makes every point below in its own
    /// voice, and "everything else in this document still holds" would be
    /// pointing at a document that isn't there.
    fn block_note(&self, g: &GroupInfo, b: &workflow::Block) -> String {
        let reviewers: Vec<&str> = g
            .guardrails
            .blocks
            .iter()
            .filter(|x| x.kind == Role::Reviewer)
            .map(|x| x.id.as_str())
            .collect();
        let multi_reviewer = b.kind == Role::Reviewer && reviewers.len() > 1;
        // A reviewer the group's merge gate NAMES is told so, whatever else is true of
        // it (#222/#197). This has to be part of the early-return test, not just an
        // extra paragraph: a gate can name a plain built-in `reviewer` block with no
        // persona and no siblings, and that block would otherwise be the one agent in
        // the group that never learns its verdict is the thing holding the merge.
        let gate = self.merge_gate(&g.id).filter(|_| b.kind == Role::Reviewer);
        let gated = gate.as_ref().is_some_and(|gt| gt.reviewers.iter().any(|r| *r == b.id));
        if b.is_builtin() && !b.has_persona() && !multi_reviewer && !gated {
            return String::new();
        }
        // `persona_allowed` for the same reason the preview asks it: an orchestrator
        // block's persona is denied at spawn, so "adopt it" would point at
        // instructions that never arrive.
        let persona_note = if b.has_persona() && workflow::persona_allowed(b) {
            " Your **persona** comes from that file too: it reached you through your CLI's own \
             custom-agent flag, or — on a CLI that has no inline one — as an addendum in your \
             kickoff prompt. Adopt it."
        } else {
            ""
        };
        let lane_note = if multi_reviewer {
            let others: Vec<String> = reviewers
                .iter()
                .filter(|id| **id != b.id)
                .map(|id| format!("`{id}`"))
                .collect();
            format!(
                "\n\nYou are **one of {n} reviewer blocks** on each PR — the others are {others}. \
                 Review **only your lane**. The split is deliberate: another block is covering what \
                 you skip, and duplicating its work costs the human money and buries your own \
                 findings. A serious defect plainly outside your lane is worth one line, not a \
                 second review. Say in your report which lane you reviewed and give a clear \
                 verdict — a merge gate may be waiting on it.",
                n = reviewers.len(),
                others = others.join(", "),
            )
        } else {
            String::new()
        };
        // The verdict contract, given ONLY to a reviewer a gate actually names — for
        // everyone else it would be prose about a tool that gates nothing. It is the
        // one instruction in this file that a merge physically waits on, so it says
        // what the shim will do rather than asking nicely.
        let gate_note = match gate.filter(|_| gated) {
            Some(gt) => {
                let rule = match gt.require {
                    workflow::GateRequire::AllPass => format!(
                        "every one of {} must record a `pass`",
                        gt.reviewers.iter().map(|r| format!("`{r}`")).collect::<Vec<_>>().join(", ")
                    ),
                    workflow::GateRequire::Threshold(n) => format!(
                        "{n} of {} must record a `pass`",
                        gt.reviewers.iter().map(|r| format!("`{r}`")).collect::<Vec<_>>().join(", ")
                    ),
                };
                format!(
                    "\n\n**Your verdict is the merge gate.** This repo's workflow declares a merge \
                     gate that names you, so `gh pr merge` is **refused** until {rule} — loomux's \
                     `gh` interceptor enforces it, and nobody can talk it into merging: not the \
                     orchestrator, not a human grant. Record yours with \
                     `review_verdict(pr, verdict, summary)` once you have finished reviewing and \
                     posted your review on the PR:\n\
                     \n\
                     - `pass` — reviewed, nothing blocking.\n\
                     - `fail` — blocking findings. Re-review after the fix and record `pass` to \
                     clear it (re-recording replaces your earlier verdict).\n\
                     - `escalate` — you are **not deciding this one**: ambiguous requirement, \
                     outside what you can judge, a risk you won't sign off on. A human must look.\n\
                     \n\
                     `fail` and `escalate` both refuse the merge, and **one blocking verdict beats \
                     any number of passes** — so never record `pass` to be agreeable, to unblock a \
                     queue, or because another reviewer already passed. If you have not finished, \
                     record nothing: an outstanding verdict holds the gate shut, which is exactly \
                     what it is for.\n\
                     \n\
                     **Your verdict is bound to the commit you reviewed.** If anything is pushed to \
                     the PR afterwards — even a lint fix — your pass goes **stale**, the gate \
                     reopens, and the merge is refused until you review the new head and record \
                     again. Expect to be called back after a fix; do not assume an earlier pass \
                     still covers the PR. `list_verdicts(pr)` shows you where the gate stands."
                )
            }
            None => String::new(),
        };
        format!(
            "\n\n{}",
            render_template(
                BLOCK_TPL,
                &[
                    ("WORKFLOW_PATH", workflow::WORKFLOW_PATH),
                    ("BLOCK_ID", &b.id),
                    ("BLOCK_KIND", b.kind.as_str()),
                    ("PERSONA_NOTE", persona_note),
                    ("LANE_NOTE", &lane_note),
                    ("GATE_NOTE", &gate_note),
                    // LAST, and this is the same discipline the caller applies to
                    // `{{BLOCK_NOTE}}` itself: `render_template` walks its list in
                    // order, so the only var whose value is repo-authored goes in
                    // when there are no passes left to rescan it. A block named
                    // `{{LANE_NOTE}}` is inert text, not a second lane note spliced
                    // into the middle of a sentence (rev-11 F3). `sanitize_display`
                    // strips braces as well, so this is belt AND braces — the order
                    // is what protects the template, the sanitizer what protects any
                    // future template that puts a name somewhere else.
                    ("BLOCK_NAME", &b.name),
                ],
            )
            .trim_end()
        )
    }

    /// Render every block's role-instruction doc into the group dir so kickoff
    /// prompts can reference them by path instead of pasting pages of text.
    ///
    /// One file per **block** now, not per role (#222) — `worker.md` for the
    /// built-in roster (unchanged), `<block-id>.md` for a custom block. All four
    /// built-in files are always written even when a workflow file has replaced
    /// the roster, because they are also what a `mode: replace` persona is
    /// measured against and what a rejoined legacy session may still reference.
    fn write_instruction_files(&self, g: &GroupInfo) -> Result<(), String> {
        let max = g.guardrails.max_agents.to_string();
        // The orchestrator's workflow section (#222) — EMPTY for the default
        // roster, which is what keeps every no-workflow group's instruction files
        // byte-for-byte what they were. `BLOCK_NOTE` is per-block, so the base
        // vars carry the empty default and `write_block_instructions` overrides
        // it; without the default here, a class-fallback file written by the loop
        // below would keep a literal `{{BLOCK_NOTE}}` in its text.
        let workflow_section = self.workflow_section(g);
        let vars: Vec<(&str, &str)> = vec![
            ("REPO", g.repo.as_str()),
            ("GROUP_ID", g.id.as_str()),
            ("MAX_AGENTS", max.as_str()),
            ("WORKER_MODEL", g.guardrails.model_for(Role::Worker)),
            ("REVIEWER_MODEL", g.guardrails.model_for(Role::Reviewer)),
            ("PLANNER_MODEL", g.guardrails.model_for(Role::Planner)),
            ("WORKFLOW", workflow_section.as_str()),
            ("BLOCK_NOTE", ""),
        ];
        let dir = self.group_dir(&g.id);
        for role in [Role::Orchestrator, Role::Worker, Role::Reviewer, Role::Planner] {
            // Skip the classes the roster covers — a block whose id is a class
            // name owns that class's file (ids are reserved per class, see
            // `clamped`), and the block loop below writes it persona-aware. For
            // the default roster that is all four, so this loop writes nothing and
            // the group dir gets four writes, not eight. The classes it *does*
            // write are the ones the roster left out: their files still have to
            // exist, because a legacy session rejoining without a block id falls
            // back to its class's file (`kickoff_prompt`).
            if g.guardrails.blocks.iter().any(|b| b.id == role.as_str()) {
                continue;
            }
            fs::write(dir.join(role.instructions_file()), render_template(role.template(), &vars))
                .map_err(|e| e.to_string())?;
        }
        for b in &g.guardrails.blocks {
            let persona = self.resolve_persona_or_audit(g, b);
            self.write_block_instructions(g, b, persona.as_ref(), &vars)?;
        }
        Ok(())
    }

    /// [`resolve_persona`](Self::resolve_persona), with the failure policy
    /// applied: a persona that won't load is **audited and dropped**, never
    /// fatal. A repo file must not be able to stop an agent from starting, so
    /// every caller wants this, not the raw `Result`.
    fn resolve_persona_or_audit(
        &self,
        g: &GroupInfo,
        b: &workflow::Block,
    ) -> Option<ResolvedPersona> {
        match self.resolve_persona(g, b) {
            Ok(p) => p,
            Err(e) => {
                self.audit(&g.id, "loomux", "workflow-persona-skipped", json!({
                    "block": b.id, "profile": b.profile, "error": e,
                }));
                None
            }
        }
    }

    /// Write one block's role-instruction file, honoring its persona mode.
    ///
    /// - **append** (and no persona): the built-in class template, as before.
    ///   The persona itself does not go here — it reaches the agent through the
    ///   CLI's native custom-agent flag (or the kickoff), so this file stays the
    ///   loomux contract and nothing else.
    /// - **replace**: [`mechanics_core`] *instead of* the class template. The
    ///   persona has replaced the role body — but the mechanics (MCP tools, the
    ///   board, `report()` discipline, branch→PR) are **not overridable**, so
    ///   loomux writes them itself and the kickoff points the agent at them. A
    ///   replace persona can change who the agent is; it can never leave it
    ///   unable to report or unable to open a PR.
    ///
    /// `persona` is the block's already-resolved persona — passed in rather than
    /// re-resolved, so the file this writes and the flags the CLI gets can never
    /// disagree about a persona that was edited mid-spawn.
    fn write_block_instructions(
        &self,
        g: &GroupInfo,
        b: &workflow::Block,
        persona: Option<&ResolvedPersona>,
        vars: &[(&str, &str)],
    ) -> Result<(), String> {
        let replace = persona.is_some_and(|p| p.mode == profiles::ProfileMode::Replace);
        let body = if replace {
            format!(
                "# {} — loomux mechanics (non-overridable)\n\n\
                 This repo's persona for the `{}` block runs in `mode: replace`: it replaces \
                 loomux's built-in {} instructions. The mechanics below are NOT part of that \
                 trade — loomux guarantees them whatever the persona says.\n\n{}\n",
                b.name,
                b.id,
                b.kind.as_str(),
                mechanics_core(b.kind),
            )
        } else {
            // This block's own `## Your block` section (#222) — empty for a block
            // the workflow file didn't touch, which is every block of the default
            // roster. It is appended LAST: `render_template` walks the list in
            // order, so nothing after it can rescan the note's text, and the note
            // is the one place a repo-authored string (a block's `name`) reaches a
            // template. A `{{MAX_AGENTS}}` in a block name stays inert text.
            let note = self.block_note(g, b);
            let mut vars: Vec<(&str, &str)> =
                vars.iter().filter(|(k, _)| *k != "BLOCK_NOTE").copied().collect();
            vars.push(("BLOCK_NOTE", note.as_str()));
            render_template(b.kind.template(), &vars)
        };
        fs::write(self.group_dir(&g.id).join(b.instructions_file()), body)
            .map_err(|e| e.to_string())
    }

    // ---------- enforced merge gate (#83): gh shim + per-pane env ----------

    /// The shared directory holding the `gh` + `git` interceptor shims, prepended
    /// to every *agent* pane's PATH so a default-branch merge (`gh pr merge`) or a
    /// release/tag publish (`gh release …`, `git push` of a `v*` tag) is
    /// structurally gated (a live incident showed template guidance alone fails).
    /// One shim set for all groups — it reads `LOOMUX_GROUP_DIR` at runtime to find
    /// the group's markers/grants — under the loomux data dir, beside the per-group
    /// orchestration state.
    fn shim_dir(&self) -> PathBuf {
        self.root.parent().unwrap_or(&self.root).join("ghshim")
    }

    /// Write one shim script (POSIX + a Windows `.cmd` delegator) for `program`,
    /// resolving the real binary and baking its absolute path in so the shim never
    /// re-resolves to itself. No-op returning `false` when the real program isn't
    /// installed (nothing to intercept). `sh` builds the POSIX body from the
    /// forward-slashed real path; `cmd` builds the `.cmd` delegator.
    fn write_shim(
        &self,
        dir: &Path,
        program: &str,
        sh: impl Fn(&str) -> String,
        cmd: impl Fn(&str) -> String,
    ) -> bool {
        let Some(real) = crate::winpath::resolve_program(
            program,
            &crate::winpath::launch_path(),
            &crate::winpath::launch_pathext(),
        ) else {
            return false;
        };
        // Forward-slash so the path is safe inside the POSIX shim (Git Bash accepts
        // `C:/…`); still valid for the `.cmd` wrapper.
        let real_fwd = real.to_string_lossy().replace('\\', "/");
        let script = dir.join(program);
        let _ = fs::write(&script, sh(&real_fwd).as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&script, fs::Permissions::from_mode(0o755));
        }
        #[cfg(target_os = "windows")]
        {
            // cmd/pwsh panes resolve `<program>` to `<program>.cmd` (`.PS1` isn't
            // on the default PATHEXT). It delegates to the POSIX shim so the gate
            // logic lives in one place; without `sh` it runs the real program (a
            // documented bypass — Git Bash, which ships `sh`, is present wherever
            // Claude Code runs its Bash tool, the primary interception point).
            let _ = fs::write(dir.join(format!("{program}.cmd")), cmd(&real_fwd).as_bytes());
        }
        true
    }

    /// Write (idempotently) the `gh` + `git` shim scripts and return the shim dir,
    /// or `None` when neither real binary is installed. Cheap (a few small file
    /// writes); called per spawn so a freshly-installed gh/git is picked up by the
    /// next pane.
    fn ensure_shims(&self) -> Option<PathBuf> {
        let dir = self.shim_dir();
        if fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let gh = self.write_shim(&dir, "gh", gh_shim_sh, gh_shim_cmd);
        let git = self.write_shim(&dir, "git", git_shim_sh, git_shim_cmd);
        (gh || git).then_some(dir)
    }

    /// The extra environment injected into an *agent* pane (never a human's plain
    /// shell): the gh + git shims prepended to PATH so the merge/release gates are
    /// enforced, and `LOOMUX_GROUP_DIR` so the shim finds this group's
    /// markers/grants. Empty when neither gh nor git is installed (nothing to gate).
    fn agent_pane_env(&self, group: &str) -> Vec<(String, String)> {
        let Some(shim) = self.ensure_shims() else {
            return Vec::new();
        };
        let sep = if cfg!(windows) { ';' } else { ':' };
        let base = crate::winpath::fresh_path()
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        let path = format!("{}{sep}{base}", shim.display());
        vec![
            ("PATH".to_string(), path),
            ("LOOMUX_GROUP_DIR".to_string(), self.group_dir(group).display().to_string()),
        ]
    }

    /// Grant directory for a kind (`merge_grants` | `release_grants`), under the
    /// group state dir. The shim reads these; only human Tauri commands write them.
    fn grant_dir(&self, group: &str, kind: &str) -> PathBuf {
        self.group_dir(group).join(kind)
    }

    /// Write a one-time human **merge** grant for PR `pr` (#83): authorizes exactly
    /// one default-branch merge of that PR within `GRANT_TTL_SECS`, after which the
    /// shim consumes it (one merge) or it expires. `pr` may be a number / `#n` /
    /// PR URL — normalized to a number; the grant is keyed `merge_grants/pr-<N>` so
    /// a grant for #5 can't authorize merging #7. Optional `comment` is delivered
    /// to the orchestrator alongside the authorization (approve-with-comment).
    /// Written atomically so the shim can never read a half-written grant. Returns
    /// the normalized PR number.
    ///
    /// **Human-only boundary:** this is reachable ONLY through Tauri commands (the
    /// board Approve button / a human grant action). No MCP tool calls it — agents
    /// run the shim and consume grants, they never mint them.
    pub fn grant_merge(
        &self,
        group: &str,
        pr: &str,
        comment: Option<&str>,
        actor: &str,
    ) -> Result<u64, String> {
        if self.group(group).is_none() {
            return Err("unknown group".into());
        }
        let num = pr_number(pr).ok_or_else(|| format!("no PR number found in {pr:?}"))?;
        let nonce = GRANT_SEQ.fetch_add(1, Ordering::Relaxed);
        let expires = now_ms() / 1000 + GRANT_TTL_SECS;
        let path = self.grant_dir(group, "merge_grants").join(format!("pr-{num}"));
        atomic_write(&path, format!("{expires}\n{nonce}\n").as_bytes()).map_err(|e| e.to_string())?;
        self.audit(group, actor, "merge-grant-written",
            json!({ "pr": num, "expires_secs": expires, "nonce": nonce }));
        let mins = GRANT_TTL_SECS / 60;
        let note = comment.map(str::trim).filter(|c| !c.is_empty());
        let msg = match note {
            Some(c) => format!(
                "[loomux] the human GRANTED a one-time merge of PR #{num} (valid ~{mins} min). \
                 Note from the human: {c}\nYou may now merge THAT PR once (only #{num}); report when done."),
            None => format!(
                "[loomux] the human GRANTED a one-time merge of PR #{num} (valid ~{mins} min). \
                 You may now merge THAT PR once (only #{num}); report when done."),
        };
        let _ = self.deliver_to_orchestrator(group, &msg, "human");
        Ok(num)
    }

    /// Write a one-time human **release/tag** grant for `tag` (#83): authorizes one
    /// `gh release create|edit|delete <tag>` or one `git push` of that tag within
    /// `GRANT_TTL_SECS`. Releases publish to the world, so — unlike merges — they
    /// are NEVER blanket-allowed by autonomous+auto_merge; each needs an explicit
    /// grant. Optional `comment` delivered to the orchestrator. Human-only, same
    /// boundary as `grant_merge`.
    pub fn grant_release(
        &self,
        group: &str,
        tag: &str,
        comment: Option<&str>,
        actor: &str,
    ) -> Result<(), String> {
        if self.group(group).is_none() {
            return Err("unknown group".into());
        }
        let tag = tag.trim();
        if tag.is_empty() {
            return Err("release grant needs a tag".into());
        }
        let seg = grant_segment(tag);
        let nonce = GRANT_SEQ.fetch_add(1, Ordering::Relaxed);
        let expires = now_ms() / 1000 + GRANT_TTL_SECS;
        let path = self.grant_dir(group, "release_grants").join(&seg);
        atomic_write(&path, format!("{expires}\n{nonce}\n").as_bytes()).map_err(|e| e.to_string())?;
        self.audit(group, actor, "release-grant-written",
            json!({ "tag": tag, "expires_secs": expires, "nonce": nonce }));
        let mins = GRANT_TTL_SECS / 60;
        let note = comment.map(str::trim).filter(|c| !c.is_empty());
        let msg = match note {
            Some(c) => format!(
                "[loomux] the human GRANTED a one-time release/tag publish of {tag} (valid ~{mins} min). \
                 Note from the human: {c}\nYou may now publish THAT release/tag once; report when done."),
            None => format!(
                "[loomux] the human GRANTED a one-time release/tag publish of {tag} (valid ~{mins} min). \
                 You may now publish THAT release/tag once; report when done."),
        };
        let _ = self.deliver_to_orchestrator(group, &msg, "human");
        Ok(())
    }

    // ---------- the workflow merge gate + review verdicts (#222 / #197) ----------

    /// The group-dir spec file the `gh` shim reads to enforce `gates.merge`.
    /// Absent = no declared gate = the pre-#222 flow, exactly.
    fn merge_gate_path(&self, group: &str) -> PathBuf {
        self.group_dir(group).join(workflow::MERGE_GATE_FILE)
    }

    /// The group's declared merge gate, or `None` if the repo declared none **or
    /// the gate file is unusable**. Callers must not read `None` as "no gate" when
    /// the file exists (`merge_gate_declared`) — the shim will read that file and
    /// refuse on exactly what makes this return `None`. The shim does its own read
    /// (in shell); this is for the Rust side — reporting gate status back to a
    /// reviewer that just recorded a verdict, and to the orchestrator that has to
    /// decide what to do next.
    pub fn merge_gate(&self, group: &str) -> Option<workflow::Gate> {
        workflow::parse_gate_file(&fs::read_to_string(self.merge_gate_path(group)).ok()?)
    }

    /// Whether the group has a gate file at all — the shim's own precondition.
    pub fn merge_gate_declared(&self, group: &str) -> bool {
        self.merge_gate_path(group).is_file()
    }

    /// Bring the group's `merge_gate` file in line with the repo's workflow file,
    /// called on every group create/resume.
    ///
    /// `Some(gate)` writes it; `None` **removes** it — a repo that deletes its
    /// `gates.merge` clause (or its whole workflow file) must not keep a gate the
    /// file no longer declares. The one case that is *not* routed here is a
    /// workflow file that fails to parse: `create_group` leaves an existing gate
    /// file alone there, because "I can't read your workflow" is not evidence that
    /// you stopped wanting the gate — see the call site.
    fn sync_merge_gate(&self, group: &str, gate: Option<&workflow::Gate>) {
        let path = self.merge_gate_path(group);
        match gate {
            Some(g) => {
                if atomic_write(&path, workflow::gate_file_text(g).as_bytes()).is_ok() {
                    self.audit(group, "loomux", "merge-gate-declared", json!({
                        "require": match g.require {
                            workflow::GateRequire::AllPass => "all-pass".to_string(),
                            workflow::GateRequire::Threshold(n) => format!("threshold {n}"),
                        },
                        "reviewers": g.reviewers,
                        "also": g.also,
                        // Say it out loud in the trail: an `also:` condition this
                        // build can't check refuses every merge (fail closed).
                        "unsupported_conditions": g.also.iter()
                            .filter(|c| !workflow::condition_supported(c)).collect::<Vec<_>>(),
                    }));
                }
            }
            None => {
                if path.is_file() && remove_marker(&path).is_ok() {
                    self.audit(group, "loomux", "merge-gate-cleared",
                        json!({ "reason": "the repo's workflow declares no gates.merge" }));
                }
            }
        }
    }

    /// Where this group's recorded verdicts for one PR live: one file per reviewer
    /// block (`verdicts/pr-<N>/<block-id>`). Both segments are loomux-generated —
    /// `pr` is a parsed number and a block id is sanitized to `[A-Za-z0-9_-]` — so
    /// neither can walk out of the group dir.
    fn verdict_dir(&self, group: &str, pr: u64) -> PathBuf {
        self.group_dir(group).join(workflow::VERDICTS_DIR).join(format!("pr-{pr}"))
    }

    /// The PR's current head commit, via the **real** gh (the backend's PATH is
    /// unshimmed, so resolve the binary the same way `write_shim` does rather than
    /// trusting `PATH`). `None` when gh is absent, unauthenticated, or the repo/PR
    /// isn't there — which the verdict path records as an *empty* head, i.e. stale,
    /// never as "unbound, therefore fine".
    ///
    /// `pr_head_override` is the test seam (mirroring `claude_projects_dir`): the
    /// integration tests must be able to record a verdict against a known revision
    /// without a GitHub repo, and every one of them drives the real MCP dispatch.
    fn pr_head(&self, repo: &str, pr: u64) -> Option<String> {
        if let Some(sha) = self.pr_head_override.lock_safe().clone() {
            return Some(sha);
        }
        if !Path::new(repo).is_dir() {
            return None;
        }
        let gh = crate::winpath::resolve_program(
            "gh",
            &crate::winpath::launch_path(),
            &crate::winpath::launch_pathext(),
        )?;
        let mut cmd = std::process::Command::new(gh);
        cmd.current_dir(repo)
            .args(["pr", "view", &pr.to_string(), "--json", "headRefOid", "--jq", ".headRefOid"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW — never flash a console
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        let sha = workflow::sanitize_sha(&String::from_utf8_lossy(&out.stdout));
        (!sha.is_empty()).then_some(sha)
    }

    /// Test seam: pretend `gh pr view --json headRefOid` returns this commit for
    /// every PR. Lets the integration tests bind verdicts to a revision (and then
    /// move it, to simulate a re-push) without a live GitHub repo.
    #[doc(hidden)]
    pub fn set_pr_head_override(&self, sha: Option<String>) {
        *self.pr_head_override.lock_safe() = sha;
    }

    /// Record a reviewer's verdict on a PR (the `review_verdict` MCP tool) — the
    /// durable, attributed state the merge gate reads.
    ///
    /// **Only a reviewer-kind block may record one**, re-checked here and not only
    /// in the MCP dispatch: the verdict is the thing that opens a gate, so the
    /// authorization belongs next to the write. A worker that could file its own
    /// PASS would make the gate decorative.
    ///
    /// The verdict is bound to the PR's **head commit at record time**, so it
    /// cannot survive a re-push: the gate compares that against the PR's current
    /// head and treats a mismatch as outstanding. Without the binding, a `pass` on
    /// #7 still reads green after the worker pushes two more commits — the gate
    /// would be satisfied to the letter of #197 and violated in its spirit.
    ///
    /// Re-recording replaces that reviewer's verdict (a reviewer that re-reviews
    /// after a fix upgrades its own `fail` to a `pass`, and a reviewer whose pass
    /// went stale re-reviews the new head); every write is audited, so the history
    /// is in the trail even though only the latest verdict gates.
    pub fn record_verdict(
        &self,
        group: &str,
        agent_id: &str,
        pr: &str,
        verdict: &str,
        summary: &str,
    ) -> Result<workflow::ReviewVerdict, String> {
        let a = self.agent(agent_id).ok_or_else(|| format!("unknown agent: {agent_id}"))?;
        if a.group != group {
            return Err(format!("unknown agent: {agent_id}")); // never leak other groups' ids
        }
        if a.role != Role::Reviewer {
            return Err(format!(
                "permission denied: review_verdict records a REVIEW outcome, so only a \
                 reviewer-kind block may call it — you are block {:?} (kind {}). Use \
                 report(status, summary) instead.",
                a.block,
                a.role.as_str()
            ));
        }
        let num = pr_number(pr)
            .ok_or_else(|| format!("no PR number found in {pr:?} — pass the number, #n, or the PR URL"))?;
        let verdict = workflow::Verdict::parse(verdict).ok_or_else(|| {
            format!("unknown verdict {verdict:?} — must be one of {}", workflow::verdict_names())
        })?;
        let summary = workflow::sanitize_summary(summary);
        if summary.is_empty() {
            return Err("summary required — one or two lines a human can act on: what you \
                        reviewed, and what decided the verdict".into());
        }
        let block = workflow::sanitize_id(&a.block)
            .ok_or("this agent's block id is unusable — it cannot be attributed a verdict")?;
        // The revision this verdict reviewed. Best-effort: an unresolvable head is
        // stored empty, which the gate reads as stale — so a verdict loomux could
        // not bind to a commit can never open a gate on its own.
        let repo = self.group(group).map(|g| g.repo).unwrap_or_default();
        let head = self.pr_head(&repo, num).unwrap_or_default();
        let rec = workflow::ReviewVerdict {
            pr: num,
            block,
            agent_id: a.id.clone(),
            verdict,
            head,
            summary,
            ts_ms: now_ms(),
        };
        // Atomic: the shim may read this file at any instant, and a half-written
        // verdict must never read as a `pass` (the first line is the verdict word).
        atomic_write(
            &self.verdict_dir(group, num).join(&rec.block),
            workflow::verdict_file_text(&rec).as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        self.audit(group, &rec.agent_id, "review-verdict", json!({
            "pr": num,
            "block": rec.block,
            "verdict": rec.verdict.as_str(),
            "head": rec.head,
            "summary": rec.summary.chars().take(500).collect::<String>(),
        }));
        Ok(rec)
    }

    /// Every verdict recorded for a PR, by reviewer block (block order).
    pub fn verdicts(&self, group: &str, pr: u64) -> Vec<workflow::ReviewVerdict> {
        let mut out: Vec<workflow::ReviewVerdict> = fs::read_dir(self.verdict_dir(group, pr))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let block = e.file_name().to_string_lossy().into_owned();
                let text = fs::read_to_string(e.path()).ok()?;
                workflow::parse_verdict_file(pr, &block, &text)
            })
            .collect();
        out.sort_by(|a, b| a.block.cmp(&b.block));
        out
    }

    /// The verdicts a gate decision reads: reviewer block → its latest record.
    fn verdict_map(&self, group: &str, pr: u64) -> BTreeMap<String, workflow::ReviewVerdict> {
        self.verdicts(group, pr).into_iter().map(|v| (v.block.clone(), v)).collect()
    }

    /// PRs this group has any recorded verdict for (ascending).
    pub fn verdict_prs(&self, group: &str) -> Vec<u64> {
        let mut prs: Vec<u64> = fs::read_dir(self.group_dir(group).join(workflow::VERDICTS_DIR))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().strip_prefix("pr-")?.parse().ok())
            .collect();
        prs.sort_unstable();
        prs
    }

    /// One line telling an agent where a PR stands against the declared gate —
    /// handed back to the reviewer that just voted and delivered to the
    /// orchestrator, so nobody has to guess whether a merge is now possible.
    /// `None` when the group declares no gate (then only the human gate applies).
    ///
    /// This must agree with the shim, which is the thing that actually refuses the
    /// merge: it reads the same files, resolves the same head, and fails closed on
    /// the same shapes. A status line that said SATISFIED while the shim refused
    /// would be worse than no status line at all.
    pub fn gate_status_line(&self, group: &str, pr: u64) -> Option<String> {
        if !self.merge_gate_declared(group) {
            return None;
        }
        // The file exists but doesn't parse: the shim refuses every merge on it
        // (`malformed-gate`), so say that rather than "no gate".
        let Some(gate) = self.merge_gate(group) else {
            return Some(format!(
                "merge gate for PR #{pr}: the group's merge_gate file is MALFORMED — every merge \
                 is refused until it is fixed (repair .loomux/workflow.yml and relaunch the group)."
            ));
        };
        let repo = self.group(group).map(|g| g.repo).unwrap_or_default();
        let head = self.pr_head(&repo, pr);
        let outcome = workflow::evaluate_merge_gate(&gate, &self.verdict_map(group, pr), head.as_deref());
        let also = if gate.also.is_empty() {
            String::new()
        } else {
            format!(" Condition(s) checked at merge time: {}.", gate.also.join(", "))
        };
        // "still waiting on X (no verdict) / Y (passed an older revision)".
        let waiting = |outstanding: &[String], stale: &[String]| -> String {
            let mut parts: Vec<String> = Vec::new();
            if !outstanding.is_empty() {
                parts.push(format!("{} (no verdict yet)", outstanding.join(", ")));
            }
            if !stale.is_empty() {
                parts.push(format!(
                    "{} (passed an EARLIER revision — the branch has moved, so they must re-review)",
                    stale.join(", ")
                ));
            }
            parts.join("; ")
        };
        Some(match outcome {
            workflow::GateOutcome::Satisfied => format!(
                "merge gate for PR #{pr}: SATISFIED by the reviewer verdicts ({}) for the current \
                 revision.{also} The human merge gate still applies on the default branch.",
                gate.reviewers.join(", ")
            ),
            workflow::GateOutcome::Blocked { blocking } => format!(
                "merge gate for PR #{pr}: BLOCKED — {} recorded a fail/escalate verdict. A \
                 blocking verdict beats any number of passes; the PR must be fixed and \
                 re-reviewed.",
                blocking.join(", ")
            ),
            workflow::GateOutcome::Short { passes, need, outstanding, stale } => format!(
                "merge gate for PR #{pr}: NOT YET SATISFIED — {passes} of {need} required PASS \
                 verdicts cover the PR's current head; still waiting on {}. `gh pr merge` is \
                 refused until then.{also}",
                waiting(&outstanding, &stale)
            ),
            workflow::GateOutcome::UnknownRevision => format!(
                "merge gate for PR #{pr}: loomux cannot resolve the PR's current head commit, so \
                 it cannot tell whether the recorded verdicts reviewed the code that would merge. \
                 The merge is refused until it can."
            ),
        })
    }

    pub fn resolve_token(&self, token: &str) -> Option<Caller> {
        let id = self.by_token.lock_safe().get(token).cloned()?;
        let agents = self.agents.lock_safe();
        let a = agents.get(&id)?;
        if a.status == AgentStatus::Dead {
            return None;
        }
        Some(Caller { agent_id: a.id.clone(), group: a.group.clone(), role: a.role })
    }

    pub fn agent(&self, id: &str) -> Option<AgentEntry> {
        self.agents.lock_safe().get(id).cloned()
    }

    fn live_delegate_count(&self, group: &str) -> u32 {
        self.agents
            .lock_safe()
            .values()
            .filter(|a| a.group == group && a.role != Role::Orchestrator && a.status != AgentStatus::Dead)
            .count() as u32
    }

    /// Human-readable roster of the group's live delegates (workers, reviewers,
    /// planners — the orchestrator is exempt from the cap) for the cap-rejection
    /// guardrail message (#203). Locks `agents`; the race-safe cap check in
    /// `spawn_agent` already holds that lock, so it calls
    /// [`format_delegate_roster`] directly against its guard instead of this.
    fn live_delegate_roster(&self, group: &str) -> String {
        let rows = self
            .agents
            .lock_safe()
            .values()
            .filter(|a| a.group == group && a.role != Role::Orchestrator && a.status != AgentStatus::Dead)
            .map(|a| (a.id.clone(), a.role.as_str(), a.idle_since_ms.is_some()))
            .collect();
        format_delegate_roster(rows)
    }

    /// Write the per-agent MCP config the agent CLI connects with. Claude
    /// and Copilot share the same core schema; Copilot additionally expects
    /// a `tools` allowlist inside the server entry.
    fn write_mcp_config(
        &self,
        group: &str,
        agent_id: &str,
        token: &str,
        cli: &str,
    ) -> Result<PathBuf, String> {
        let port = self.port();
        if port == 0 {
            return Err("loomux MCP server is not running".into());
        }
        let mut server = json!({
            "type": "http",
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "headers": { "X-Loomux-Agent": token },
        });
        if cli == "copilot" {
            server["tools"] = json!(["*"]);
        }
        let cfg = json!({ "mcpServers": { "loomux": server } });
        let dir = self.group_dir(group).join("configs");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{agent_id}.json"));
        fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Resolve a block's persona from its `prompt:` (inline) or `profile:` (a
    /// repo file) — `Ok(None)` when the block declares neither, which is every
    /// block of the default roster.
    ///
    /// A broken `profile:` is an **error the caller audits and swallows**, not a
    /// failed spawn: a repo file must never be able to stop an agent from
    /// starting. Re-read on every spawn, so editing a persona applies to the
    /// next agent without restarting the group.
    #[doc(hidden)] // pub for integration tests
    pub fn resolve_persona(
        &self,
        group: &GroupInfo,
        block: &workflow::Block,
    ) -> Result<Option<ResolvedPersona>, String> {
        // The orchestrator block is loomux-owned: a repo may pin its cli/model,
        // never author its persona or pre-approve its tools. `parse_workflow`
        // rejects that outright and says why — but a hand-edited `group.json`
        // never meets the parser, so the persona is dropped here too, and audited.
        //
        // Neutralizing it *here* (rather than only in `persona_inject`) is what
        // makes it total: this is the single point both the CLI flags and the
        // block's instruction file are resolved through, so a `mode: replace`
        // orchestrator persona cannot rewrite `orchestrator.md` either. See
        // `parse_workflow` for why the trust root is not a customization surface.
        if !workflow::persona_allowed(block) && (block.has_persona() || !block.allow.is_empty()) {
            self.audit(&group.id, "loomux", "workflow-orchestrator-persona-denied", json!({
                "block": block.id,
                "prompt": block.prompt.is_some(),
                "profile": block.profile,
                "allow": block.allow,
                "why": "the orchestrator is loomux's trust root — a repo file may not author its \
                        prompt or pre-approve its tools",
            }));
            return Ok(None);
        }
        if let Some(rel) = block.profile.as_deref() {
            let p = profiles::load_block_profile(&group.repo, rel, block.kind)?;
            let handle = p.copilot_agent.clone().unwrap_or_else(|| p.name.clone());
            // Copilot's `--agent` resolves names against `.github/agents/`, so
            // only a persona that actually lives there can use the native flag —
            // AND the name must resolve back to the file loomux actually read.
            //
            // The handle comes from the file's frontmatter `name:`, not from its
            // path. So `.github/agents/security-review.md` whose frontmatter says
            // `name: worker` would make loomux emit `--agent worker`, and Copilot
            // would go and load whichever file declares `name: worker` — the
            // *worker* persona. loomux would have kind-checked one file and
            // launched another, with the audit line insisting all was well.
            //
            // So: only take the native path when the handle unambiguously names
            // this file. Otherwise fall back to kickoff injection, which delivers
            // the persona loomux actually read.
            let native = profiles::is_copilot_native(rel)
                && profiles::handle_resolves_to(&group.repo, &handle, rel);
            if profiles::is_copilot_native(rel) && !native {
                self.audit(&group.id, "loomux", "copilot-agent-handle-ambiguous", json!({
                    "block": block.id, "profile": rel, "handle": handle,
                    "action": "using kickoff injection — `--agent` would resolve to a different file",
                }));
            }
            return Ok(Some(ResolvedPersona {
                text: workflow::sanitize_persona(&p.instructions),
                name: handle,
                description: workflow::sanitize_persona(&p.description),
                mode: p.mode,
                allow: p.allow.iter().chain(block.allow.iter()).cloned().collect(),
                copilot_native: native,
            }));
        }
        if let Some(prompt) = block.prompt.as_deref() {
            return Ok(Some(ResolvedPersona {
                text: workflow::sanitize_persona(prompt),
                name: block.id.clone(),
                // `sanitize_display` keeps a name readable — it strips control
                // characters and braces, and nothing else — so it can still
                // contain an apostrophe, and the description rides into the
                // single-quoted `--agents` token. Persona-sanitize it too, or a
                // block named `Bob's review` would close that quote.
                description: workflow::sanitize_persona(&block.name),
                // An inline `prompt:` is an addendum to the built-in role
                // contract. Only a persona FILE can declare `mode: replace` —
                // replacing loomux's role body is a deliberate, reviewable act,
                // not something a one-liner in a workflow file falls into.
                mode: profiles::ProfileMode::Append,
                allow: block.allow.clone(),
                copilot_native: false,
            }));
        }
        // No persona, but a block may still carry `allow:` patterns.
        if !block.allow.is_empty() {
            return Ok(Some(ResolvedPersona {
                text: String::new(),
                name: block.id.clone(),
                description: workflow::sanitize_persona(&block.name),
                mode: profiles::ProfileMode::Append,
                allow: block.allow.clone(),
                copilot_native: false,
            }));
        }
        Ok(None)
    }

    /// Compile a resolved persona into the launch flags of `cli` — the table in
    /// [`PersonaInject`]. `None` in, `PersonaInject::default()` out: no persona,
    /// no flags, pre-#222 command line.
    /// Audit a repo-authored `allow:` that was refused because the block's class
    /// is read-only. Silently dropping it would leave an author wondering why
    /// their pattern does nothing; honoring it would break capability closure.
    fn audit_allow_denied(&self, group: &str, block: &workflow::Block, allow: &[String]) {
        self.audit(group, "loomux", "workflow-allow-denied", json!({
            "block": block.id,
            "kind": block.kind.as_str(),
            "allow": allow,
            "why": "a read-only capability class may not pre-approve tool patterns — \
                    an allow pattern could hand it a shell that writes files",
        }));
    }

    #[doc(hidden)] // pub for integration tests
    pub fn persona_inject(
        &self,
        group: &str,
        block: &workflow::Block,
        cli: &str,
        persona: Option<&ResolvedPersona>,
    ) -> PersonaInject {
        let Some(p) = persona else {
            return PersonaInject::default();
        };
        // CAPABILITY CLOSURE, enforced at the last possible moment (#222).
        //
        // `workflow::parse_workflow` already refuses `allow:` on a read-only
        // block, but that is not the only way a pattern gets here: a
        // `.github/agents/*.md` persona can carry its own `allow:` frontmatter,
        // and a hand-edited group.json never sees the parser at all. A read-only
        // class is read-only by denying a *fixed list* of tools — so an allow
        // pattern that names something not on that list (`Bash(python *)`,
        // `Bash(tee *)`, …) would hand a planner a pre-approved shell that writes
        // files, with no human in its pane to say no.
        //
        // Nobody can enumerate every write-capable program. So a read-only block
        // simply gets no allow patterns, from any source, ever.
        let allow: Vec<String> = if block.kind.is_read_only() {
            if !p.allow.is_empty() {
                self.audit_allow_denied(group, block, &p.allow);
            }
            Vec::new()
        } else {
            p.allow.clone()
        };
        let mut out = PersonaInject { extra_allow: allow, ..Default::default() };
        if p.text.trim().is_empty() {
            return out; // allow-only block
        }
        if cli == "copilot" {
            if p.copilot_native {
                out.copilot_agent = Some(p.name.clone());
            } else {
                // No inline persona flag on Copilot and no user-authored file to
                // name — so the persona travels as text in the kickoff prompt.
                out.kickoff = Some(p.text.clone());
            }
            return out;
        }
        // Claude (and the fallback adapter): define the block inline and
        // activate it. `description` is required by the CLI's schema.
        let payload = json!({
            &block.id: {
                "description": if p.description.trim().is_empty() { block.id.as_str() } else { p.description.trim() },
                "prompt": p.text,
            }
        });
        out.claude_agents_json =
            Some(workflow::ascii_escape_json(&serde_json::to_string(&payload).unwrap_or_default()));
        out.claude_agent = Some(block.id.clone());
        out
    }

    /// Build an agent's launch command for the group's CLI. Baseline
    /// permissions minimize the approvals needed just to *initialize*: the
    /// group state dir is added as a workspace (so reading the instructions
    /// file never prompts) and the loomux MCP tools are pre-approved (so
    /// `report` etc. never prompt). `auto_ops` additionally pre-approves
    /// git/gh commands so the branch→commit→PR flow runs unattended;
    /// everything else still asks the human. A `read_only` planner is
    /// *always* treated as unattended (Auto perms + git/gh allowlist)
    /// regardless of `auto_ops`: it never mutates and has no human in its
    /// pane, so gating it would only deadlock it (see below).
    ///
    /// `read_only` hardens the planner contract at the CLI level (#47): where
    /// the CLI supports tool denial, the file-editing tools and the git
    /// mutation subcommands (`commit`/`push`) are denied outright, so a planner
    /// cannot write code or create branches/commits/pushes even under Auto
    /// perms — while `gh` stays available so it can still post its plan as an
    /// issue comment. Deny rules take precedence over the allow list on both
    /// CLIs. NOTE: this is a real, structural denial for the write/commit/push
    /// surface; it is deliberately NOT a full sandbox (e.g. `gh pr create` is
    /// left reachable so the plan comment works), so the *complete* read-only
    /// contract still rests partly on the planner's instructions.
    ///
    /// `persona` compiles a workflow block's persona down to the CLI's **native**
    /// custom-agent flag (#222) — see [`PersonaInject`]. A block with no persona
    /// passes `PersonaInject::default()`, which adds nothing: that is what makes
    /// a group with no `.loomux/workflow.yml` byte-for-byte identical to
    /// pre-#222 loomux (pinned by `default_roster_command_lines_match_legacy`).
    ///
    /// Ordering matters and is not cosmetic: `extra_allow` must be emitted while
    /// `--allowedTools` is still the open list, i.e. *before* `--disallowedTools`
    /// — otherwise the allow patterns would be parsed as *denials*.
    #[allow(clippy::too_many_arguments)]
    #[doc(hidden)] // pub for integration tests
    pub fn build_agent_command(
        &self,
        cli: &str,
        model: &str,
        auto_ops: bool,
        cfg: &Path,
        group_dir: &Path,
        workdir: &Path,
        session: Option<&str>,
        resume: bool,
        read_only: bool,
        persona: &PersonaInject,
    ) -> String {
        // A planner never mutates and has no human in its pane, so there is
        // nothing for `auto_ops` to gate: it must explore, post its plan
        // comment, and report with zero prompts, or it would stall waiting on
        // an approval no one is there to give. So a planner (`read_only`)
        // always runs unattended on BOTH CLIs; workers/reviewers follow the
        // group's `auto_ops`. (This is also why claude's `plan` permission
        // mode / copilot's `--plan` can't be used here — both hold the plan
        // for interactive human sign-off.)
        let unattended = auto_ops || read_only;
        match cli {
            "copilot" => {
                // Copilot has `--resume <id>` but no way to pre-assign an
                // id, so sessions aren't tracked for it (yet).
                let resume_flag = match (session, resume) {
                    (Some(s), true) => format!("--resume {s} "),
                    _ => String::new(),
                };
                // NOTE: the @ (copilot's file-path marker) must sit INSIDE
                // the quotes — the pane shell is PowerShell, where a bare
                // `@"` opens a here-string and the whole line dies with a
                // ParserError before copilot ever runs.
                // --no-auto-update: a mid-boot self-update restarts the
                // CLI and flushes anything typed into the first instance.
                // --add-dir <workdir>: pre-trusts the agent's workspace so
                // panes don't stall on a folder-trust prompt.
                let mut cmd = format!(
                    "copilot {resume_flag}--additional-mcp-config \"@{}\" --model {model} \
                     --add-dir \"{}\" --add-dir \"{}\" --allow-tool loomux --no-auto-update",
                    cfg.display(),
                    group_dir.display(),
                    workdir.display()
                );
                if unattended {
                    // Group workers/planners get true autopilot mode: all tools +
                    // all paths pre-approved AND --autopilot (the autonomy
                    // system-prompt framing). The resulting "Enable autopilot
                    // mode" startup dialog is answered deterministically by the
                    // kickoff path (see COPILOT_GROUP_AUTOPILOT_FLAGS /
                    // confirm_copilot_autopilot_dialog) before the brief is
                    // pasted. A planner (read_only) always takes this path even
                    // in a non-auto_ops group — interactive mode would stall it on
                    // a human that isn't there; the deny rules below keep it
                    // read-only, and deny takes precedence over --allow-all-tools
                    // in Copilot.
                    cmd.push(' ');
                    cmd.push_str(COPILOT_GROUP_AUTOPILOT_FLAGS);
                } else {
                    cmd.push_str(" --allow-tool \"shell(git:*)\" --allow-tool \"shell(gh:*)\"");
                }
                if read_only {
                    // Deny file writes and git mutations even under
                    // --allow-all-tools (deny takes precedence in Copilot).
                    // `write`/`edit` are Copilot's file-modification tools;
                    // `gh` is left allowed so the plan comment can be posted.
                    cmd.push_str(
                        " --deny-tool \"write\" --deny-tool \"edit\" \
                         --deny-tool \"shell(git commit)\" --deny-tool \"shell(git push)\"",
                    );
                }
                // Copilot's native custom agent (#222). `--agent <name>` resolves
                // the name against `.github/agents/` — it CANNOT take an inline
                // definition, so this is set only when the block's `profile:`
                // points at a file the *user* authored there. loomux never writes
                // a generated persona into `.github/agents/` to make this flag
                // work: that would dirty the user's git tree with files they did
                // not write. A block with an inline `prompt:` instead reaches
                // Copilot through the kickoff prompt (`PersonaInject::kickoff`).
                if let Some(agent) = &persona.copilot_agent {
                    cmd.push_str(&format!(" --agent {agent}"));
                }
                for pat in &persona.extra_allow {
                    cmd.push_str(&format!(" --allow-tool \"{pat}\""));
                }
                cmd
            }
            // "claude" and the explicit fallback for anything unrecognized.
            _ => {
                // Assigning the session id up front is what makes per-task
                // sessions resumable later: loomux never has to fish the id
                // out of the CLI.
                let session_flag = match (session, resume) {
                    (Some(s), true) => format!("--resume {s} "),
                    (Some(s), false) => format!("--session-id {s} "),
                    (None, _) => String::new(),
                };
                // "Auto" preset = Claude Code's native auto permission mode
                // (what the human uses interactively); otherwise acceptEdits.
                // A planner (`read_only`) is always `unattended` (see above),
                // so it runs under Auto perms even in a non-auto_ops group.
                let perm = claude_permission_mode(unattended);
                let mut cmd = format!(
                    "claude {session_flag}--mcp-config \"{}\" --strict-mcp-config --model {model} \
                     --permission-mode {perm} --add-dir \"{}\" --allowedTools mcp__loomux",
                    cfg.display(),
                    group_dir.display()
                );
                if unattended {
                    // Pre-approve git + gh so the unattended flow runs without
                    // prompts (workers: branch→commit→PR; planners: read-only
                    // explore + `gh issue comment` for the plan). `Bash(git *)`
                    // matches every git subcommand; a planner's denials below
                    // carve commit/push back out.
                    cmd.push(' ');
                    cmd.push_str(CLAUDE_UNATTENDED_ALLOW);
                }
                // Persona `allow:` patterns extend the SAME `--allowedTools`
                // list, so they must land before `--disallowedTools` opens the
                // deny list below. They can only widen within the capability
                // class: on Claude, `--disallowedTools` beats the allow list, so
                // a planner persona cannot allow itself back into `git commit`.
                for pat in &persona.extra_allow {
                    cmd.push_str(&format!(" \"{pat}\""));
                }
                if read_only {
                    // Deny the file-editing tools and the git mutation
                    // subcommands outright (--disallowedTools overrides the
                    // permission mode AND the allow list in Claude Code), so a
                    // planner can't write code or commit/push. `gh` (incl.
                    // `gh issue comment`) stays reachable for the plan comment.
                    //
                    // Spelling matters. `:*` is a valid wildcard only as a
                    // TRAILING suffix (`Bash(gh:*)` is fine); a colon in the
                    // MIDDLE of the command (`Bash(git commit:*)`) is not —
                    // Claude Code discards that rule as malformed AND prints a
                    // startup warning, the "auto deny rule" flash a human
                    // caught on planner boot. So the enforcing denial rests on
                    // the space form `Bash(git commit *)`: it is the canonical
                    // spelling and actually blocks commit/push, with no
                    // warning. (An earlier draft passed both spellings; the
                    // colon-mid one added nothing but the warning.)
                    cmd.push_str(
                        " --disallowedTools Edit Write MultiEdit NotebookEdit \
                         \"Bash(git commit *)\" \"Bash(git push *)\"",
                    );
                }
                // Claude's native custom agent (#222). Unlike Copilot, Claude
                // takes the whole block **inline** — `--agents '<json>'` defines
                // it, `--agent <id>` activates it — so loomux can hand a
                // synthesized persona straight to the CLI with zero repo files
                // and zero trust problem. (This replaces PR #105's
                // `--append-system-prompt-file`, which predates the flag.)
                //
                // The payload rides inside SINGLE quotes. That is the whole
                // quoting story: in both PowerShell and POSIX sh a single-quoted
                // string is literal except for `'` itself — which
                // `workflow::sanitize_persona` has already removed — and
                // `workflow::ascii_escape_json` has made the payload pure ASCII,
                // so a non-UTF-8 pane code page cannot mangle it either.
                //
                // The shell-string form assumes the fallback shell is PowerShell
                // (Windows) or sh (POSIX) — the same assumption the copilot branch
                // above already documents at its `@"` here-string note, and the
                // one `default_shell()` makes: powershell.exe ships with every
                // supported Windows build. Bare `cmd.exe` gives `'` no meaning and
                // would mangle this, but reaching it requires powershell.exe to be
                // absent from PATH. The primary path is unaffected either way:
                // direct spawn uses `build_agent_argv`, where the payload is one
                // literal token and no shell parses it at all.
                if let Some(json) = &persona.claude_agents_json {
                    cmd.push_str(&format!(" --agents '{json}'"));
                }
                if let Some(agent) = &persona.claude_agent {
                    cmd.push_str(&format!(" --agent {agent}"));
                }
                cmd
            }
        }
    }

    /// The **structured** form of [`build_agent_command`] — the same invocation
    /// as a program + literal-argument vector instead of a shell command line
    /// (issue #78). Direct-CLI pane spawn hands this to `spawn_pty` so the agent
    /// executable becomes the ConPTY child with no pwsh/sh wrapper; the string
    /// form is still emitted alongside it as the shell fallback (shim CLIs,
    /// unresolved programs, or the `LOOMUX_NO_DIRECT_SPAWN` escape hatch).
    ///
    /// Each element is a literal argv token: no surrounding shell quotes, spaces
    /// inside a token preserved (`Bash(git *)` is ONE element). Built from the
    /// same flag atoms as `build_agent_command`; a consistency test
    /// (`build_agent_argv_matches_command_line`) tokenizes the string form and
    /// asserts it equals this vector across the full matrix, so the two can't
    /// drift.
    #[allow(clippy::too_many_arguments)]
    #[doc(hidden)] // pub for integration tests
    pub fn build_agent_argv(
        &self,
        cli: &str,
        model: &str,
        auto_ops: bool,
        cfg: &Path,
        group_dir: &Path,
        workdir: &Path,
        session: Option<&str>,
        resume: bool,
        read_only: bool,
        persona: &PersonaInject,
    ) -> Vec<String> {
        let unattended = auto_ops || read_only;
        let mut a: Vec<String> = Vec::new();
        let push = |a: &mut Vec<String>, s: &str| a.push(s.to_string());
        match cli {
            "copilot" => {
                push(&mut a, "copilot");
                if let (Some(s), true) = (session, resume) {
                    push(&mut a, "--resume");
                    push(&mut a, s);
                }
                push(&mut a, "--additional-mcp-config");
                // The @ marker rides on the path as a single argv element (no
                // shell here-string hazard once it's not a shell string at all).
                a.push(format!("@{}", cfg.display()));
                push(&mut a, "--model");
                push(&mut a, model);
                push(&mut a, "--add-dir");
                a.push(group_dir.display().to_string());
                push(&mut a, "--add-dir");
                a.push(workdir.display().to_string());
                push(&mut a, "--allow-tool");
                push(&mut a, "loomux");
                push(&mut a, "--no-auto-update");
                if unattended {
                    // Reuse the atom directly: no quotes/embedded spaces, so the
                    // whitespace split yields exactly the argv tokens.
                    for t in COPILOT_GROUP_AUTOPILOT_FLAGS.split_whitespace() {
                        push(&mut a, t);
                    }
                } else {
                    push(&mut a, "--allow-tool");
                    push(&mut a, "shell(git:*)");
                    push(&mut a, "--allow-tool");
                    push(&mut a, "shell(gh:*)");
                }
                if read_only {
                    push(&mut a, "--deny-tool");
                    push(&mut a, "write");
                    push(&mut a, "--deny-tool");
                    push(&mut a, "edit");
                    push(&mut a, "--deny-tool");
                    push(&mut a, "shell(git commit)");
                    push(&mut a, "--deny-tool");
                    push(&mut a, "shell(git push)");
                }
                if let Some(agent) = &persona.copilot_agent {
                    push(&mut a, "--agent");
                    push(&mut a, agent);
                }
                for pat in &persona.extra_allow {
                    push(&mut a, "--allow-tool");
                    push(&mut a, pat);
                }
            }
            // "claude" and the explicit fallback for anything unrecognized.
            _ => {
                push(&mut a, "claude");
                match (session, resume) {
                    (Some(s), true) => {
                        push(&mut a, "--resume");
                        push(&mut a, s);
                    }
                    (Some(s), false) => {
                        push(&mut a, "--session-id");
                        push(&mut a, s);
                    }
                    (None, _) => {}
                }
                push(&mut a, "--mcp-config");
                a.push(cfg.display().to_string());
                push(&mut a, "--strict-mcp-config");
                push(&mut a, "--model");
                push(&mut a, model);
                push(&mut a, "--permission-mode");
                push(&mut a, claude_permission_mode(unattended));
                push(&mut a, "--add-dir");
                a.push(group_dir.display().to_string());
                push(&mut a, "--allowedTools");
                push(&mut a, "mcp__loomux");
                if unattended {
                    // == CLAUDE_UNATTENDED_ALLOW, as literal (unquoted) tokens.
                    push(&mut a, "Bash(git *)");
                    push(&mut a, "Bash(gh *)");
                }
                // Still inside --allowedTools' value list — before the deny list.
                for pat in &persona.extra_allow {
                    push(&mut a, pat);
                }
                if read_only {
                    push(&mut a, "--disallowedTools");
                    push(&mut a, "Edit");
                    push(&mut a, "Write");
                    push(&mut a, "MultiEdit");
                    push(&mut a, "NotebookEdit");
                    push(&mut a, "Bash(git commit *)");
                    push(&mut a, "Bash(git push *)");
                }
                if let Some(json) = &persona.claude_agents_json {
                    push(&mut a, "--agents");
                    // One literal argv token: no shell, so no quoting at all.
                    // The string form wraps this same payload in single quotes.
                    push(&mut a, json);
                }
                if let Some(agent) = &persona.claude_agent {
                    push(&mut a, "--agent");
                    push(&mut a, agent);
                }
            }
        }
        a
    }

    /// Register an agent, emit the pane spawn request, wait for the frontend
    /// bind, then type the kickoff prompt. Enforces the group guardrails.
    /// `task` empty = idle agent awaiting assignment.
    pub fn spawn_agent(
        &self,
        group_id: &str,
        role: Role,
        name: &str,
        task: &str,
        use_worktree: bool,
        branch: Option<String>,
    ) -> Result<AgentEntry, String> {
        self.spawn_agent_ex(group_id, role, None, name, task, use_worktree, branch, None, None, None, None)
    }

    /// Full spawn: `block` names a workflow block explicitly (#222) — that block
    /// *is* the agent's identity, and its `kind` becomes the capability class,
    /// so an orchestrator picks `rev-security` rather than "a reviewer". `None`
    /// takes the default block for `role`, which for the built-in roster is the
    /// only one. `resume_session` reopens a previous session (follow-ups on a
    /// finished task) instead of cold-starting; `cwd_override` places the pane
    /// where that work originally happened (e.g. its worktree).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_agent_ex(
        &self,
        group_id: &str,
        role: Role,
        block: Option<String>,
        name: &str,
        task: &str,
        use_worktree: bool,
        branch: Option<String>,
        base: Option<String>,
        resume_session: Option<String>,
        cwd_override: Option<String>,
        restore_name_source: Option<NameSource>,
    ) -> Result<AgentEntry, String> {
        let group = self.group(group_id).ok_or("unknown group")?;

        // Identity first: which block is this agent? A named block's `kind` is
        // authoritative from here on — the capability class comes from the
        // roster, never from the caller's guess.
        let named = block.as_deref().map(str::trim).filter(|b| !b.is_empty());
        let block = match named {
            Some(id) => group.guardrails.block(id).cloned().ok_or_else(|| {
                let known: Vec<&str> =
                    group.guardrails.blocks.iter().map(|b| b.id.as_str()).collect();
                format!("unknown block {id:?}. Blocks in this group: {}", known.join(", "))
            })?,
            None => group.guardrails.block_for(role).cloned().ok_or_else(|| {
                format!("this group's workflow declares no {} block", role.as_str())
            })?,
        };
        // A workflow file must not be able to hand an agent a second
        // orchestrator: an orchestrator-kind spawn is exempt from the live-agent
        // cap and the spawn-rate backstop (both below) and resolves to the
        // privileged MCP tool set.
        //
        // This is the *block* half of that rule. The *kind* half lives in
        // `mcp::call_tool` ("spawn_agent"), which refuses `kind: orchestrator`
        // outright — that is the only agent-reachable entry point, and it has to
        // be the enforcement point because this function's `role ==
        // Role::Orchestrator` path is still legitimately used to register the
        // group's own orchestrator in tests. Neither check is redundant: this one
        // catches `block: "<an orchestrator block>"`, which arrives with
        // `kind: worker` and would otherwise be promoted by `role = block.kind`.
        if block.kind == Role::Orchestrator && named.is_some() {
            return Err(format!(
                "block {:?} is an orchestrator block — a group has exactly one orchestrator, opened at launch",
                block.id
            ));
        }
        let role = block.kind;

        // Guardrail: live delegate cap (the orchestrator itself is exempt).
        if role != Role::Orchestrator {
            let live = self.live_delegate_count(group_id);
            if live >= group.guardrails.max_agents {
                // #203: name who holds the slots so a rejected orchestrator can
                // see which delegate to reuse/kill (idle ones first) instead of
                // being told only a count. Without this the orchestrator's first
                // clue that a zombie planner is squatting a slot is this bare
                // rejection.
                let roster = self.live_delegate_roster(group_id);
                return Err(format!(
                    "guardrail: {live} live agents already (max {}). Reuse an idle agent or kill one first. Live delegates: {roster}.",
                    group.guardrails.max_agents
                ));
            }
            // Guardrail: spawn-rate backstop against a runaway orchestrator.
            // Checked (and the timestamp recorded only when the check passes)
            // before any pane/worktree work so a burst fast-fails. A refused
            // spawn is not counted; one admitted here but later aborted
            // (worktree/bind failure) still counts toward the hour.
            self.check_and_record_spawn(group_id, group.guardrails.max_spawns_per_hour)?;
        }

        // Guardrail: the CLI and model are pinned per block (#4, now #222).
        // Reject an unknown CLI at spawn rather than silently downgrading it —
        // the launcher only offers supported CLIs and the workflow parser
        // rejects unknown ones, so an unsupported one here means a hand-edited
        // group.json.
        let cli = workflow::cli_of(&block, &group.guardrails.agent_cli);
        if !SUPPORTED_CLIS.contains(&cli) {
            return Err(format!(
                "guardrail: unsupported agent CLI {cli:?} for block {} — supported: {}",
                block.id, SUPPORTED_CLIS.join(", ")
            ));
        }
        let cli = cli.to_string();
        let model = workflow::model_of(&block, &group.guardrails.agent_cli).to_string();

        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let agent_id = format!("{}-{seq}", block.prefix());
        let token = new_token();
        // Name precedence (#95r): a caller-supplied name is the orchestrator's
        // choice; an empty one means "no meaningful name", so we derive the
        // default from the minted id — "worker 2" for `w-2` — which agrees with
        // the pane's "W 2" badge (#75) and the roster id instead of the old
        // per-launch "worker N" counter that drifted from the seq.
        // The id-derived default now names the BLOCK, not the class (#222) — a
        // "rev-security 7" pane says which reviewer it is. For the built-in
        // roster a block's name IS its class name ("worker"), so this is
        // byte-identical to the pre-block default.
        let (display, derived_source) = {
            let cleaned = sanitize_agent_name(name);
            if cleaned.is_empty() {
                (format!("{} {seq}", block.name), NameSource::Default)
            } else {
                (cleaned, NameSource::Orchestrator)
            }
        };
        // A session rejoin re-spawns with the roster name (non-empty, so the
        // derived tier would be `Orchestrator`); `restore_name_source` carries
        // the persisted tier instead, so a human-renamed pane comes back at the
        // `Human` tier and a later `rename_agent` still cannot clobber it.
        let name_source = restore_name_source.unwrap_or(derived_source);

        // Workspace: dedicated worktree (branch of the same name) or the repo
        // itself, where the worker is instructed to branch before touching
        // anything.
        // Session identity: resumes reuse the given id; fresh Claude agents
        // get a pre-assigned UUID so their session is resumable later.
        let resume = resume_session.is_some();
        let session_id = match resume_session {
            Some(s) => Some(sanitize_session(&s).ok_or("invalid resume session id")?),
            None => (cli == "claude").then(new_session_uuid),
        };

        // Copilot mints its own session id after boot (no `--session-id`), so
        // snapshot the sessions that already exist now, before this pane's
        // copilot starts — the watcher then identifies the newly appeared one.
        let copilot_baseline = (!resume && cli == "copilot")
            .then(|| {
                crate::sessions::copilot_session_state_root()
                    .map(|root| crate::sessions::copilot_session_ids(&root))
                    .unwrap_or_default()
            });

        let branch_name = branch
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| format!("agent/{agent_id}"));
        // Explicit worktree base (default: the repo's default branch, resolved
        // in git_worktree_add). Normalized once for both the worktree cut and
        // the audit record (#204).
        let base = base.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
        let cwd_override = cwd_override.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        let (cwd, branch_note) = if let Some(c) = cwd_override {
            if !Path::new(&c).is_dir() {
                return Err(format!("cwd does not exist: {c}"));
            }
            (c, String::new())
        } else if use_worktree && role != Role::Orchestrator && role != Role::Planner {
            // Cut the branch from the default branch (or an explicit `base`),
            // never the primary checkout's incidental HEAD (#204).
            let wt = crate::git::git_worktree_add(group.repo.clone(), branch_name.clone(), base.clone())?;
            (wt.clone(), format!(
                "Your working directory is a dedicated git worktree at {wt} already checked out on branch '{branch_name}'."
            ))
        } else if role == Role::Orchestrator {
            (group.repo.clone(), String::new())
        } else if role == Role::Reviewer {
            (group.repo.clone(), "You review; you do not create branches or push. Inspect PRs via gh (checking out the PR branch locally is fine).".to_string())
        } else if role == Role::Planner {
            // Planners explore read-only in the repo itself and never branch,
            // worktree, commit, or PR — so a worktree is never created for
            // them even if `use_worktree` was set (the CLI-level write/commit
            // denials in `build_agent_command` back this note structurally).
            (group.repo.clone(), PLANNER_READONLY_NOTE.to_string())
        } else {
            (group.repo.clone(), format!(
                "Work in the repo itself; create branch '{branch_name}' off the default branch before changing anything. Never commit to the default branch."
            ))
        };

        if cli == "copilot" {
            pre_trust_copilot_folder(&cwd);
        }

        // The block's persona, compiled to this CLI's native custom-agent flags
        // (#222). Resolved ONCE, here, and used for both the instruction file and
        // the launch flags — re-resolving would let a persona edited mid-spawn
        // produce a file and a command line that disagree. Fresh per spawn, so an
        // edited persona applies to the next agent without restarting the group.
        let persona = self.resolve_persona_or_audit(&group, &block);
        // Refresh the block's instruction file so a `mode: replace` swap (or a
        // persona edit) is reflected in what the kickoff points at.
        let max = group.guardrails.max_agents.to_string();
        let vars: Vec<(&str, &str)> = vec![
            ("REPO", group.repo.as_str()),
            ("GROUP_ID", group.id.as_str()),
            ("MAX_AGENTS", max.as_str()),
            ("WORKER_MODEL", group.guardrails.model_for(Role::Worker)),
            ("REVIEWER_MODEL", group.guardrails.model_for(Role::Reviewer)),
            ("PLANNER_MODEL", group.guardrails.model_for(Role::Planner)),
        ];
        // Audited, not swallowed: the kickoff below hands the agent this file's
        // path as "read your role instructions", so a failed write means an agent
        // booting against a stale or missing loomux contract. Not fatal (the
        // previous content is usually still correct), but never silent.
        if let Err(e) = self.write_block_instructions(&group, &block, persona.as_ref(), &vars) {
            self.audit(group_id, "loomux", "error", json!({
                "what": "could not write the block's role-instruction file",
                "block": block.id, "file": block.instructions_file(), "err": e,
            }));
        }
        let inject = self.persona_inject(group_id, &block, &cli, persona.as_ref());

        let cfg = self.write_mcp_config(group_id, &agent_id, &token, &cli)?;
        let command = self.build_agent_command(
            &cli,
            &model,
            group.guardrails.auto_ops,
            &cfg,
            &self.group_dir(group_id),
            Path::new(&cwd),
            session_id.as_deref(),
            resume,
            role.is_read_only(), // deny writes/commits at the CLI level
            &inject,
        );
        let argv = self.build_agent_argv(
            &cli,
            &model,
            group.guardrails.auto_ops,
            &cfg,
            &self.group_dir(group_id),
            Path::new(&cwd),
            session_id.as_deref(),
            resume,
            role.is_read_only(),
            &inject,
        );

        let entry = AgentEntry {
            id: agent_id.clone(),
            group: group_id.to_string(),
            name: display.clone(),
            name_source,
            block: block.id.clone(),
            role,
            token: token.clone(),
            status: AgentStatus::Starting,
            pty_id: None,
            task: task.to_string(),
            session_id: session_id.clone(),
            cwd: cwd.clone(),
            // An agent spawned without a task starts the idle clock; one
            // given work does not (the orchestrator is exempt regardless).
            idle_since_ms: (role != Role::Orchestrator && task.trim().is_empty()).then(now_ms),
            started_ms: now_ms(),
            last_progress_ms: now_ms(),
            last_output_total: 0,
            watchdog_notified: false,
            idle_tick_notified: false,
        };
        {
            // Re-check the cap under the same lock as the insert: the early
            // check above fast-fails before worktree creation, but only this
            // one is race-free against concurrent spawns.
            let mut agents = self.agents.lock_safe();
            if role != Role::Orchestrator {
                let live = agents
                    .values()
                    .filter(|a| {
                        a.group == group_id
                            && a.role != Role::Orchestrator
                            && a.status != AgentStatus::Dead
                    })
                    .count() as u32;
                if live >= group.guardrails.max_agents {
                    // #203: emit the same roster the fast path does — this is the
                    // check that actually fires under concurrent spawns, so it's
                    // the one an orchestrator most needs the squatter list from.
                    // Formatted off the already-held guard to avoid re-locking.
                    let roster = format_delegate_roster(
                        agents
                            .values()
                            .filter(|a| {
                                a.group == group_id
                                    && a.role != Role::Orchestrator
                                    && a.status != AgentStatus::Dead
                            })
                            .map(|a| (a.id.clone(), a.role.as_str(), a.idle_since_ms.is_some()))
                            .collect(),
                    );
                    let _ = fs::remove_file(&cfg);
                    return Err(format!(
                        "guardrail: {live} live agents already (max {}). Reuse an idle agent or kill one first. Live delegates: {roster}.",
                        group.guardrails.max_agents
                    ));
                }
            }
            agents.insert(agent_id.clone(), entry.clone());
        }
        self.by_token.lock_safe().insert(token, agent_id.clone());
        self.persist_agent_record(&entry, "running");
        self.audit(group_id, "loomux", "agent-spawn", json!({
            "agent": agent_id, "role": role, "name": display, "cwd": cwd,
            "cli": cli, "model": model, "worktree": use_worktree, "branch": branch_name, "task": task,
            "base": base, "session": session_id, "resume": resume,
            // #222: which block this agent is, and how its persona reached the
            // CLI — so a run stays reproducible after the workflow file changes.
            "block": block.id,
            "persona": persona.as_ref().map(|p| json!({
                "source": if block.profile.is_some() { "profile" } else { "prompt" },
                "mode": p.mode.as_str(),
                "delivery": if inject.copilot_agent.is_some() { "copilot --agent" }
                    else if inject.claude_agent.is_some() { "claude --agents" }
                    else if inject.kickoff.is_some() { "kickoff" }
                    else { "none" },
            })),
        }));
        // Breadcrumb (no prompt/task text): ids + role only.
        crate::obs::breadcrumb(
            "agent-spawn",
            &format!("group={group_id} agent={agent_id} role={role:?} worktree={use_worktree}"),
        );

        let request = SpawnRequest {
            group_id: group_id.to_string(),
            agent_id: agent_id.clone(),
            role,
            name: display,
            cwd: cwd.clone(),
            command,
            // Expire the request when our own bind wait would (#106).
            deadline_ms: now_ms() + BIND_TIMEOUT.as_millis() as u64,
            argv,
            // Agent pane: inject the gh-shim PATH + LOOMUX_GROUP_DIR so the merge
            // gate is enforced structurally (#83).
            env: self.agent_pane_env(group_id),
        };

        let app = self.app.lock_safe().clone();
        let Some(app) = app else {
            // Test mode: no frontend. Mark running so guardrail/authz logic
            // can be exercised without panes. Handle a vanished entry (a
            // concurrent reap between insert and here) instead of unwrapping —
            // a panic here would fire while holding the agents lock.
            if let Some(a) = self.agents.lock_safe().get_mut(&agent_id) {
                a.status = AgentStatus::Running;
            }
            return self
                .agent(&agent_id)
                .ok_or_else(|| "agent vanished during spawn".to_string());
        };

        let (tx, rx) = mpsc::channel::<u32>();
        self.pending_binds.lock_safe().insert(agent_id.clone(), tx);
        app.emit("orch-spawn-request", &request).map_err(|e| e.to_string())?;

        match rx.recv_timeout(BIND_TIMEOUT) {
            Ok(pty_id) => {
                {
                    let mut agents = self.agents.lock_safe();
                    if let Some(a) = agents.get_mut(&agent_id) {
                        a.status = AgentStatus::Running;
                        a.pty_id = Some(pty_id);
                    }
                }
                self.by_pty.lock_safe().insert(pty_id, agent_id.clone());
                self.audit(group_id, "loomux", "agent-bind", json!({ "agent": agent_id, "pty": pty_id }));
                crate::obs::breadcrumb("agent-bind", &format!("agent={agent_id} pty={pty_id}"));
                if resume {
                    // Resumed sessions already have their role and history;
                    // deliver only the follow-up (if any) instead of the
                    // full kickoff. ResumeKickoff waits for boot but skips the
                    // autopilot confirm — the consent is restored, no dialog.
                    if !task.trim().is_empty() {
                        self.deliver_prompt(&agent_id, task, "loomux", Delivery::ResumeKickoff)?;
                    }
                } else {
                    let a = self
                        .agent(&agent_id)
                        .ok_or("agent vanished during spawn")?;
                    let kickoff =
                        self.kickoff_prompt(&a, &group, &branch_note, inject.kickoff.as_deref());
                    self.deliver_prompt(&agent_id, &kickoff, "loomux", Delivery::FreshKickoff)?;
                }
                // Copilot minted a session as it booted; watch for it and bind
                // its id to this pane's roster record so the session becomes
                // resumable and shows in the session browser. Needs an owned
                // registry (background thread) — a no-op in unit tests, which
                // don't set the self-arc.
                if let Some(baseline) = copilot_baseline {
                    if let Some(reg) = self.arc() {
                        reg.spawn_copilot_session_watcher(
                            agent_id.clone(),
                            group_id.to_string(),
                            cwd.clone(),
                            baseline,
                        );
                    }
                }
                self.agent(&agent_id)
                    .ok_or_else(|| "agent vanished during spawn".into())
            }
            Err(_) => {
                self.pending_binds.lock_safe().remove(&agent_id);
                self.mark_dead(&agent_id, None);
                // Cancel the still-queued request frontend-side so a recovered
                // frontend doesn't service it as a zombie pane (#106).
                self.emit_spawn_cancelled(group_id, &agent_id);
                Err("frontend did not open the agent pane in time".into())
            }
        }
    }

    /// The first prompt typed into a freshly-booted agent pane.
    ///
    /// `persona` is the **kickoff fallback** (#222): the persona body of a block
    /// whose CLI has no inline custom-agent flag and no user-authored
    /// `.github/agents` file to name — i.e. Copilot with an inline `prompt:`.
    /// `None` on Claude (its persona rode in on `--agents`), on a native Copilot
    /// `--agent`, and for every block of the default roster — which is why a
    /// group with no workflow file gets the same kickoff text it always did.
    #[doc(hidden)] // pub for integration tests
    pub fn kickoff_prompt(
        &self,
        a: &AgentEntry,
        g: &GroupInfo,
        branch_note: &str,
        persona: Option<&str>,
    ) -> String {
        // The block's own contract file (`worker.md` for the built-in roster,
        // `<block-id>.md` for a declared block). Falls back to the class file if
        // the block is gone from the roster — a rejoined session must still boot.
        let instructions = self.group_dir(&g.id).join(
            g.guardrails
                .block(&a.block)
                .map(|b| b.instructions_file())
                .unwrap_or_else(|| a.role.instructions_file().to_string()),
        );
        // A persona delivered as text is framed as an ADDENDUM, never as a
        // replacement for the instructions file: the file is the loomux contract
        // (and, for a replace-mode persona, the non-overridable mechanics core),
        // and no repo text may talk an agent out of it.
        let persona_note = match persona.map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => format!(
                "\n\nThis repo's workflow gives you a persona. Adopt it, but it does not \
                 override the loomux mechanics in your instructions file above:\n\n{p}\n"
            ),
            None => String::new(),
        };
        let out = self.kickoff_body(a, g, branch_note, &instructions);
        format!("{out}{persona_note}")
    }

    /// The roster paragraph appended to an orchestrator's kickoff when the repo
    /// declares a workflow (#222). **Empty for the built-in roster** — that is
    /// what keeps a no-workflow group's kickoff text byte-for-byte what it was.
    ///
    /// Only the roster and the gates are declared; the *edges* are advisory and
    /// deliberately not handed over as a schedule. The orchestrator's judgment
    /// about what to run when (serialize a sprawling change, parallelize
    /// independent ones, plan first or go straight to a worker) is the thing
    /// that makes it good, and a static graph would replace it with something
    /// dumber. See doc/design/workflows.md.
    fn roster_note(&self, g: &GroupInfo) -> String {
        if !workflow::roster_is_custom(&g.guardrails.blocks) {
            return String::new();
        }
        let rows: Vec<String> = g
            .guardrails
            .blocks
            .iter()
            .filter(|b| b.kind != Role::Orchestrator)
            .map(|b| {
                format!(
                    "  - {} ({}, {}, {}){}",
                    b.id,
                    b.kind.as_str(),
                    workflow::cli_of(b, &g.guardrails.agent_cli),
                    workflow::model_of(b, &g.guardrails.agent_cli),
                    if b.has_persona() { " — has a persona" } else { "" },
                )
            })
            .collect();
        format!(
            "\nThis repo declares a custom workflow ({path}). Its blocks — pass `block: \"<id>\"` \
             to spawn_agent to open one (its kind, CLI, model and persona come from the file):\n{rows}\n\
             The workflow's edges are ADVISORY: they are the declared happy path, not a schedule. \
             You still decide what to run when.",
            path = workflow::WORKFLOW_PATH,
            rows = rows.join("\n"),
        )
    }

    fn kickoff_body(
        &self,
        a: &AgentEntry,
        g: &GroupInfo,
        branch_note: &str,
        instructions: &Path,
    ) -> String {
        match a.role {
            Role::Orchestrator => format!(
                "You are the orchestrator of loomux agent group {gid} for the repository {repo}.\n\
                 First read your role instructions: {ins}\n\
                 Guardrails (enforced by loomux): max {max} live agents, worker model {wm}, reviewer model {rm}, planner model {pm}.\n\
                 Group config: auto-merge is {automerge}; auto-release is {autorelease}; supervised dangerous mode is {dangerous} (see the merge-gate section of your instructions); autonomous idle-tick mode is {autonomous}.{roster}\n\
                 Start by calling get_state, run `gh issue list --label agent-managed --state open`, call list_agents, \
                 reconcile them, then give the human a short status summary and wait for direction.",
                gid = g.id, repo = g.repo, ins = instructions.display(),
                max = g.guardrails.max_agents, wm = g.guardrails.model_for(Role::Worker),
                rm = g.guardrails.model_for(Role::Reviewer), pm = g.guardrails.model_for(Role::Planner),
                // The declared roster (#222) — the orchestrator cannot spawn a
                // block it doesn't know exists. Empty for the built-in roster,
                // so a group with no workflow file gets the kickoff it always
                // got, to the byte.
                roster = self.roster_note(g),
                // Autonomous config the template's conditional sections read (#83).
                // Live toggles also deliver a mid-session notice; this covers a
                // fresh boot / resume, where there's no notice to have seen.
                automerge = if self.is_auto_merge(&g.id) { "ENABLED (you may merge adequately-tested PRs yourself)" } else { "disabled (human merge gate is absolute — never merge)" },
                autorelease = if self.is_auto_release(&g.id) { "ENABLED (you may publish releases/tags yourself while autonomous)" } else { "disabled (releases/tags need an explicit human grant — never publish)" },
                dangerous = if self.is_dangerous_mode(&g.id) { "ON — the human is present and supervising, and has authorized you to merge to the default branch AND publish releases/tags yourself without a per-item grant (audit + announce each; still hold anything genuinely risky)" } else { "off" },
                autonomous = if self.is_autonomous(&g.id) { "ON (you will get [loomux] idle tick wakes to run your cadence unattended)" } else { "off" },
            ),
            Role::Worker | Role::Reviewer | Role::Planner => {
                let head = format!(
                    "You are \"{name}\" ({id}), a {role} agent in loomux group {gid} for repository {repo}.\n\
                     First read your role instructions: {ins}\n{note}",
                    name = a.name, id = a.id, role = a.role.as_str(),
                    gid = g.id, repo = g.repo, ins = instructions.display(), note = branch_note,
                );
                if a.task.trim().is_empty() {
                    format!("{head}\nNo task is assigned yet. After reading the instructions, call report(\"progress\", \"ready\") and wait for prompts.")
                } else {
                    format!("{head}\nYour task:\n{}", a.task)
                }
            }
        }
    }

    /// Type `text` into an agent's CLI: audit, then bracketed paste + Enter
    /// on a background thread (serialized so deliveries never interleave).
    /// `delivery` classifies the call (see [`Delivery`]): a kickoff to a just-
    /// booted CLI holds the paste until the pane has painted its UI and gone
    /// quiet (input typed before the CLI's reader attaches is flushed and lost),
    /// and a *fresh* copilot boot additionally answers the autopilot consent
    /// dialog that its first submit triggers (#179); mid-session deliveries do
    /// neither.
    pub fn deliver_prompt(
        &self,
        agent_id: &str,
        text: &str,
        from: &str,
        delivery: Delivery,
    ) -> Result<(), String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        if a.status == AgentStatus::Dead {
            return Err(format!("agent {agent_id} is dead"));
        }
        // Pause guardrail: while a group is paused, loomux delivers nothing
        // to its panes so agents finish their turn and idle out. The attempt
        // is audited (nothing is silently lost from the record) and reported
        // as success so callers don't error or retry.
        if self.is_paused(&a.group) {
            self.audit(&a.group, from, "prompt-suppressed-paused", json!({ "to": agent_id, "text": text }));
            return Ok(());
        }
        let pty_id = a.pty_id.ok_or("agent has no terminal yet")?;
        let app = self.app.lock_safe().clone().ok_or("no app handle")?;
        self.audit(&a.group, from, "prompt", json!({ "to": agent_id, "text": text }));

        // A freshly *booted* group copilot agent is launched with `--autopilot`,
        // so it opens the "Enable autopilot mode" consent dialog; the worker
        // thread answers it before pasting the brief. Only a fresh boot shows it
        // (resume restores the consent from the session log; mid-session is past
        // boot), and only an unattended copilot agent passes --autopilot — so
        // the confirm (and its fail-soft wait) is gated to exactly that case.
        let confirm_autopilot = {
            let groups = self.groups.lock_safe();
            groups.get(&a.group).is_some_and(|g| {
                should_confirm_copilot_autopilot(
                    g.guardrails.cli_for(a.role),
                    g.guardrails.auto_ops || a.role == Role::Planner,
                    delivery.is_fresh_boot(),
                )
            })
        };
        let wait_ready = delivery.wait_ready();

        let paste = bracketed_paste(text);
        // The Enter that submits the paste is chosen per CLI: Copilot ignores a
        // bare CR on an unfocused pane, so its sequence prefixes a focus-in
        // report (#98). Resolved here — through the same per-role `cli_for` the
        // registry already uses — so the delivery thread carries the right bytes.
        let cli = self
            .group(&a.group)
            .map(|g| g.guardrails.cli_for(a.role).to_string())
            .unwrap_or_else(|| "claude".to_string());
        let submit = submit_sequence(&cli);
        let lock = self
            .delivery
            .lock_safe()
            .entry(pty_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let (root, group, agent) = (self.root.clone(), a.group.clone(), a.id.clone());
        let last_delivery = self.last_delivery.clone();
        // Captured for the unconfirmed-delivery notice (#103): whether this
        // target is the orchestrator (a notice to it would loop, so it's
        // suppressed) and an owned registry handle so the detached thread can
        // deliver the notice once it knows the submit outcome.
        let target_is_orchestrator = a.role == Role::Orchestrator;
        let reg = self.arc();
        std::thread::spawn(move || {
            let _guard = lock.lock_safe();
            let ptys = app.state::<crate::pty::PtyManager>();

            let start = std::time::Instant::now();
            if wait_ready {
                let mut last_len = 0usize;
                let mut last_change = std::time::Instant::now();
                loop {
                    std::thread::sleep(READY_POLL);
                    let Some(out) = ptys.output_tail(pty_id) else {
                        append_audit(&root, &group, "loomux", "prompt-failed",
                            json!({ "to": agent, "reason": "terminal closed while waiting for CLI to become ready" }));
                        return;
                    };
                    if out.len() != last_len {
                        last_len = out.len();
                        last_change = std::time::Instant::now();
                    }
                    if cli_ready(last_len, last_change.elapsed(), start.elapsed()) {
                        break;
                    }
                    if start.elapsed() >= READY_MAX_WAIT {
                        // Paste anyway — better a visible prompt the human
                        // can re-submit than one silently withheld.
                        break;
                    }
                }
            }

            // Copilot autopilot consent (#101/#179): the "Enable autopilot mode"
            // dialog does NOT open at boot — verified live against copilot 1.0.69,
            // a fresh --autopilot pane paints a normal input box, and the consent
            // dialog is triggered by the FIRST message submit. So the confirm is
            // answered AFTER the kickoff Enter (below), not here: selecting its
            // default "Enable all permissions" both enables autopilot and delivers
            // the pending brief in one step. Watching at boot (as this used to)
            // only burned the fail-soft wait on a dialog that never shows.

            // Human-typing backstop (#43, option A): if a human is typing
            // directly in this pane, hold the paste until they go quiet so a
            // report can't land inside their half-typed line. Capped so a long
            // compose session can't starve the queue.
            if let Some(held_ms) = wait_for_user_quiet(&ptys, pty_id) {
                append_audit(&root, &group, "loomux", "delivery-held-for-user", json!({
                    "to": agent, "stage": "pre-paste", "held_ms": held_ms,
                    "capped": held_ms >= USER_QUIET_MAX_HOLD.as_millis() as u64,
                }));
            }

            // Stranded-text flush (#81/#84): if the PREVIOUS delivery to this
            // pane was never confirmed as submitted, its text may still be
            // sitting in the input box — pasting now would append to it and the
            // two prompts would merge. Press submit once to clear it first, but
            // only if no human has typed since that delivery (else the box may
            // hold a person's line, which must never be blind-submitted — the
            // pre-paste hold above already waited for them to go quiet).
            let prev = last_delivery.lock_safe().get(&pty_id).copied();
            let human_typed_since = prev
                .map(|o| ptys.last_user_input_ms(pty_id).unwrap_or(0) > o.submit_sent_ms)
                .unwrap_or(false);
            if should_flush_before_paste(prev.map(|o| o.confirmed), human_typed_since)
                && ptys.write_bytes(pty_id, submit).is_ok()
            {
                append_audit(&root, &group, "loomux", "delivery-flush",
                    json!({ "to": agent, "reason": "previous delivery unconfirmed" }));
                std::thread::sleep(FLUSH_SETTLE);
            }

            // Human-input paste guard (#111): the quiet backstop above only waits
            // out ACTIVE typing — it doesn't stop a paste landing on top of a line
            // the human typed and LEFT sitting in the box. Pasting there and
            // pressing Enter merge-submits their line with the prompt (the live
            // `/model` + task-text collision). So hold for the box to clear
            // (they submit or clear it); if it never does, abort WITHOUT pasting
            // and nudge the orchestrator to re-send once the pane is clear.
            match wait_for_box_clear(&ptys, pty_id) {
                PasteDecision::Paste { held_ms } if held_ms > 0 => {
                    append_audit(&root, &group, "loomux", "delivery-held-for-input", json!({
                        "to": agent, "held_ms": held_ms, "outcome": "cleared",
                    }));
                }
                PasteDecision::Paste { .. } => {}
                PasteDecision::Abort { held_ms } => {
                    append_audit(&root, &group, "loomux", "delivery-aborted-human-input", json!({
                        "to": agent, "held_ms": held_ms,
                    }));
                    // Best-effort one-shot nudge so the orchestrator re-sends once
                    // the human's line is gone. Nothing was pasted, so there is no
                    // outcome to record for the next delivery's flush.
                    if let Some(reg) = reg {
                        reg.notify_delivery_held(&group, &agent, target_is_orchestrator);
                    }
                    return;
                }
            }

            // Echo-verified typing: paste, then require the TUI to emit
            // output (its input box redrawing). No echo means the CLI
            // flushed the paste with its startup stdin buffer — retype.
            let mut echoed = false;
            let mut attempts = 0u32;
            while attempts < ECHO_ATTEMPTS {
                attempts += 1;
                let Some(before) = ptys.output_total(pty_id) else {
                    append_audit(&root, &group, "loomux", "prompt-failed",
                        json!({ "to": agent, "reason": "terminal closed before delivery" }));
                    return;
                };
                if ptys.write_bytes(pty_id, &paste).is_err() {
                    append_audit(&root, &group, "loomux", "prompt-failed",
                        json!({ "to": agent, "reason": "terminal closed before delivery" }));
                    return;
                }
                let echo_deadline = std::time::Instant::now() + ECHO_WINDOW;
                while std::time::Instant::now() < echo_deadline {
                    std::thread::sleep(Duration::from_millis(150));
                    match ptys.output_total(pty_id) {
                        Some(now_total) if now_total >= before + ECHO_MIN_BYTES => {
                            echoed = true;
                            break;
                        }
                        Some(_) => {}
                        None => {
                            append_audit(&root, &group, "loomux", "prompt-failed",
                                json!({ "to": agent, "reason": "terminal closed during delivery" }));
                            return;
                        }
                    }
                }
                if echoed {
                    break;
                }
                std::thread::sleep(ECHO_RETRY_DELAY);
            }
            std::thread::sleep(PASTE_SUBMIT_DELAY);

            // Wait for the pane to go quiet before Enter: a busy CLI
            // (mid-turn) ignores the submit and the prompt would sit in
            // the input box until a human presses Enter.
            let submit_start = std::time::Instant::now();
            let mut last_total = ptys.output_total(pty_id).unwrap_or(0);
            let mut last_change = std::time::Instant::now();
            // Whether the pane went quiet before we press Enter. If it never
            // does (busy CLI, hit SUBMIT_MAX_WAIT), the Enter lands mid-stream
            // and submit confirmation can't be trusted off that stream (rev-32).
            let mut reached_quiet = false;
            while submit_start.elapsed() < SUBMIT_MAX_WAIT {
                std::thread::sleep(Duration::from_millis(200));
                match ptys.output_total(pty_id) {
                    Some(t) if t != last_total => {
                        last_total = t;
                        last_change = std::time::Instant::now();
                    }
                    Some(_) => {
                        if last_change.elapsed() >= SUBMIT_QUIET {
                            reached_quiet = true;
                            break;
                        }
                    }
                    None => {
                        append_audit(&root, &group, "loomux", "prompt-failed",
                            json!({ "to": agent, "reason": "terminal closed before submit" }));
                        return;
                    }
                }
            }
            // Re-check right before the first Enter: the human may have
            // started typing during the quiet-wait above, and a blind Enter
            // would submit their line. Hold again until they're quiet (#43).
            if let Some(held_ms) = wait_for_user_quiet(&ptys, pty_id) {
                append_audit(&root, &group, "loomux", "delivery-held-for-user", json!({
                    "to": agent, "stage": "pre-enter", "held_ms": held_ms,
                    "capped": held_ms >= USER_QUIET_MAX_HOLD.as_millis() as u64,
                }));
            }
            let submit_sent_ms = now_ms();
            // Baseline just before the first Enter, so the confirmation window
            // below measures only the burst that Enter produces.
            let submit_baseline = ptys.output_total(pty_id).unwrap_or(last_total);
            let _ = ptys.write_bytes(pty_id, submit);

            // Copilot autopilot consent (#101/#179): a fresh --autopilot copilot
            // opens its "Enable autopilot mode" dialog in response to this FIRST
            // submit (not at boot). Answer it now — Enter selects the default
            // "Enable all permissions", which enables autopilot AND lets the brief
            // we just submitted proceed (verified live: the pending message is not
            // discarded). Gated to a fresh unattended copilot boot; fail-soft, so
            // if the dialog never shows (already consented, flow changed) delivery
            // just continues to the retries. Must run before the confirm window so
            // the dialog's Enter has landed before we judge whether the turn began.
            if confirm_autopilot {
                confirm_copilot_autopilot_dialog(&ptys, pty_id, &root, &group, &agent);
            }

            // Confirm the submit landed: watch for the output burst of the box
            // clearing / the turn starting (#81/#84). Measured off the first
            // Enter, before the spaced retries, so the signal is that Enter's
            // effect and not a retry's. Only trusted when the pane reached quiet
            // first — on a busy pane that never did, the Enter landed mid-stream
            // and that stream would false-confirm (rev-32), so we skip the
            // window and leave it unconfirmed. A miss here is safe: the next
            // delivery's flush just no-ops on an empty box.
            let confirm_deadline = std::time::Instant::now() + SUBMIT_CONFIRM_WINDOW;
            let mut confirmed = false;
            while reached_quiet && std::time::Instant::now() < confirm_deadline {
                std::thread::sleep(Duration::from_millis(100));
                match ptys.output_total(pty_id) {
                    Some(t) => {
                        if submit_confirmed(reached_quiet, submit_baseline, t) {
                            confirmed = true;
                            break;
                        }
                    }
                    None => break,
                }
            }

            for delay in SUBMIT_RETRY_DELAYS {
                std::thread::sleep(delay);
                // A human typing in this pane means the box may hold THEIR
                // half-written text — a blind Enter would submit it.
                if ptys.last_user_input_ms(pty_id).unwrap_or(0) > submit_sent_ms {
                    append_audit(&root, &group, "loomux", "submit-retries-skipped",
                        json!({ "to": agent, "reason": "human typing in pane" }));
                    break;
                }
                if ptys.write_bytes(pty_id, submit).is_err() {
                    break;
                }
            }
            // Record the outcome so the next delivery to this pane can flush a
            // prompt still stranded in the box (#81/#84).
            last_delivery
                .lock_safe()
                .insert(pty_id, DeliveryOutcome { confirmed, submit_sent_ms });
            append_audit(&root, &group, "loomux", "prompt-typed", json!({
                "to": agent,
                "cli": cli,
                "waited_ms": start.elapsed().as_millis() as u64,
                "attempts": attempts,
                "echoed": echoed,
                "submit_waited_ms": submit_start.elapsed().as_millis() as u64,
                "submit_confirmed": confirmed,
            }));
            // Delivery outcome breadcrumb — timing + flags only, never the text.
            crate::obs::breadcrumb(
                "delivery",
                &format!(
                    "agent={agent} pty={pty_id} outcome=typed echoed={echoed} confirmed={confirmed} attempts={attempts} waited_ms={}",
                    start.elapsed().as_millis() as u64
                ),
            );
            // Close the loop (#103): an unconfirmed delivery to a worker/reviewer
            // may be stranded in its input box — nudge the orchestrator once so it
            // can read the pane back and re-send. Exactly one notice per delivery:
            // this is the single emission point, past all the submit retries.
            if let Some(reg) = reg {
                reg.notify_unconfirmed_delivery(&group, &agent, target_is_orchestrator, confirmed);
            }
        });
        Ok(())
    }

    /// Human steering from the loomux compose strip (#43, option C): enqueue
    /// `text` to the group's orchestrator through the SAME per-pane serialized
    /// delivery path worker reports use. Rejects empty text and a paused group
    /// up front so the strip can tell the human why nothing was sent — a paused
    /// group's delivery is silently suppressed, so without this guard a steered
    /// message would vanish with no feedback. A dead/absent orchestrator
    /// surfaces as the "no live orchestrator" error from delivery.
    #[doc(hidden)] // pub for integration tests
    pub fn steer_orchestrator(&self, group: &str, text: &str) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err("empty steering message".into());
        }
        if self.is_paused(group) {
            return Err("group is paused — resume it before steering".into());
        }
        self.deliver_to_orchestrator(group, text, "human")
    }

    /// Close the delivery feedback loop (#103): when a delivery to a
    /// non-orchestrator agent finishes with its submit unconfirmed, tell the
    /// group's orchestrator once so it can `get_output` the pane and re-send if
    /// the prompt is stranded unsubmitted. No-op for orchestrator-target or
    /// confirmed deliveries (`should_notify_unconfirmed`), and — like the
    /// watchdog nudge — a paused group is skipped entirely: delivery is
    /// suppressed there anyway, so we must not spend the notice budget while
    /// paused. Best-effort (a dead orchestrator just drops it) and audited. The
    /// notice is itself a delivery TO the orchestrator, so it can never trigger a
    /// notice of its own — no loops.
    #[doc(hidden)] // pub for integration tests
    pub fn notify_unconfirmed_delivery(
        &self,
        group: &str,
        agent_id: &str,
        target_is_orchestrator: bool,
        confirmed: bool,
    ) {
        if !should_notify_unconfirmed(target_is_orchestrator, confirmed) || self.is_paused(group) {
            return;
        }
        self.audit(group, "loomux", "delivery-unconfirmed-notice", json!({ "to": agent_id }));
        let _ = self.deliver_to_orchestrator(group, &unconfirmed_delivery_notice(agent_id), "loomux");
    }

    /// Notify the orchestrator that a delivery to `agent_id` was HELD and
    /// aborted because the pane holds a human's unsubmitted line (#111) — the
    /// prompt was never pasted, so the orchestrator must re-send once the box is
    /// clear. Same discipline as `notify_unconfirmed_delivery`: skipped for an
    /// orchestrator target (`should_notify_paste_held` — a notice to it would
    /// loop) and for a paused group (delivery is suppressed there anyway, so we
    /// must not spend the notice on it). Best-effort and audited; exactly one
    /// notice per aborted delivery (the caller invokes this once, at the abort).
    #[doc(hidden)] // pub for integration tests
    pub fn notify_delivery_held(&self, group: &str, agent_id: &str, target_is_orchestrator: bool) {
        if !should_notify_paste_held(target_is_orchestrator) || self.is_paused(group) {
            return;
        }
        self.audit(group, "loomux", "delivery-held-notice", json!({ "to": agent_id }));
        let _ = self.deliver_to_orchestrator(group, &paste_held_notice(agent_id), "loomux");
    }

    /// Deliver to the group's orchestrator (worker reports, exit notices).
    pub fn deliver_to_orchestrator(&self, group: &str, text: &str, from: &str) -> Result<(), String> {
        let orch = self
            .agents
            .lock_safe()
            .values()
            .find(|a| a.group == group && a.role == Role::Orchestrator && a.status != AgentStatus::Dead)
            .map(|a| a.id.clone())
            .ok_or("no live orchestrator in this group")?;
        self.deliver_prompt(&orch, text, from, Delivery::MidSession)
    }

    pub fn list_agents(&self, group: &str) -> Value {
        let agents = self.agents.lock_safe();
        let mut list: Vec<Value> = agents
            .values()
            .filter(|a| a.group == group)
            .map(|a| {
                // Registry hygiene (#106): a dead agent keeps its identity
                // (id/name/role/session/status/cwd) so the orchestrator can
                // still resume its session, but sheds its task brief — dead
                // records accumulate across a run and the full briefs pushed
                // one group's roster to ~86KB. Live agents keep `task` so the
                // orchestrator sees what each is working on.
                let mut o = json!({
                    "id": a.id, "name": a.name, "role": a.role,
                    // #222: which block this agent is. An orchestrator reading
                    // its roster needs the identity, not just the class — three
                    // reviewers all report `role: reviewer`.
                    "block": a.block,
                    "status": a.status,
                    "session": a.session_id, "cwd": a.cwd,
                    "idle_since_ms": a.idle_since_ms,
                });
                if a.status != AgentStatus::Dead {
                    o["task"] = json!(a.task);
                }
                o
            })
            .collect();
        list.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        json!(list)
    }

    pub fn agent_output_tail(&self, agent_id: &str, lines: usize) -> Result<String, String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        let pty_id = a.pty_id.ok_or("agent has no terminal")?;
        let app = self.app.lock_safe().clone().ok_or("no app handle")?;
        let ptys = app.state::<crate::pty::PtyManager>();
        let raw = ptys.output_tail(pty_id).ok_or("terminal already closed")?;
        let text = strip_ansi(&raw);
        let all: Vec<&str> = text.lines().collect();
        let n = lines.clamp(1, 500);
        let start = all.len().saturating_sub(n);
        Ok(all[start..].join("\n"))
    }

    pub fn kill_agent(&self, agent_id: &str) -> Result<(), String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        if a.role == Role::Orchestrator {
            return Err("refusing to kill the orchestrator; close its pane instead".into());
        }
        let app = self.app.lock_safe().clone().ok_or("no app handle")?;
        if let Some(pty) = a.pty_id {
            app.state::<crate::pty::PtyManager>().kill(pty);
        }
        self.audit(&a.group, "loomux", "agent-kill", json!({ "agent": agent_id }));
        Ok(())
    }

    /// #203: a planner's contract is one plan → one report → exit, but the CLI
    /// session lingers idle after its final report, silently holding a delegate
    /// slot until idle-kill — and the orchestrator only finds out when a later
    /// spawn is rejected at the cap. When a planner reports `done`, loomux closes
    /// its pane deterministically here so the slot frees the moment the plan is
    /// posted; the planner role-template exit instruction is only belt-and-braces.
    ///
    /// Ordering: the caller (the MCP `report` handler) hands the done report to
    /// the orchestrator *first*, so the completion exit notice this delivers is
    /// *enqueued* after it — phrased as a normal finish, not a crash. On the live
    /// path both are pasted by detached threads serialized by the per-pty
    /// delivery mutex, which is not FIFO-fair, so "enqueued after" is
    /// overwhelmingly-but-not-strictly "delivered after" — the same semantics
    /// every back-to-back notice pair in loomux has; both always arrive.
    ///
    /// Claiming the close: [`mark_dead`](Self::mark_dead) is the atomic gate. It
    /// returns `Some` only for the caller that transitions the agent live→dead
    /// (idempotent under the agents lock), so two concurrent `done` reports yield
    /// exactly one exit notice and one kill. Marking dead first also frees the
    /// slot immediately and drops the `by_pty` mapping, so the real pane exit
    /// lands as a no-op in `on_pty_exit` rather than a duplicate generic "exited
    /// (code …)" notice. No-op for a non-planner, an already-dead agent, or an
    /// unknown id.
    ///
    /// Known edges (rare, documented not fixed — see PR #209): (a) if a human's
    /// unsubmitted line is sitting in the orchestrator pane, the *report* paste
    /// aborts and its re-send nudge is suppressed for orchestrator targets (the
    /// #103 anti-loop rule), so the exit notice can arrive without the report —
    /// but the plan is durable as a GitHub issue comment and the notice says so.
    /// (b) A crash or human-kill of the planner pty in the microsecond window
    /// between the report handoff and `mark_dead` lets `on_pty_exit` win and add
    /// a generic crash notice; a *voluntary* exit can't race here (the CLI is
    /// blocked awaiting this MCP response).
    pub fn close_completed_planner(&self, agent_id: &str) {
        // Role gate first — only a planner is auto-closed. Role is immutable, so
        // reading it before the claim is safe; the claim below stays atomic.
        match self.agent(agent_id) {
            Some(a) if a.role == Role::Planner => {}
            _ => return,
        }
        // Atomic claim: only the winner of the live→dead transition proceeds, so
        // a concurrent double `done` delivers one notice and one kill (see doc).
        let Some(snapshot) = self.mark_dead(agent_id, Some(0)) else { return };
        let _ = self.deliver_to_orchestrator(
            &snapshot.group,
            &format!(
                "[loomux] planner {} ({}) posted its plan and exited — its delegate slot is free.",
                snapshot.name, snapshot.id
            ),
            "loomux",
        );
        // Terminate the actual CLI pane. Best-effort: unit tests run without an
        // app handle or a bound pty.
        if let (Some(app), Some(pty)) = (self.app.lock_safe().clone(), snapshot.pty_id) {
            app.state::<crate::pty::PtyManager>().kill(pty);
        }
    }

    pub fn focus_agent(&self, agent_id: &str) -> Result<(), String> {
        let a = self.agent(agent_id).ok_or("unknown agent")?;
        let app = self.app.lock_safe().clone().ok_or("no app handle")?;
        app.emit("orch-focus", json!({ "agent_id": agent_id, "pty_id": a.pty_id }))
            .map_err(|e| e.to_string())
    }

    /// Rename an agent's pane title and durable roster entry, respecting the
    /// name-source precedence ladder (#95r): the rename applies only when
    /// `source` ranks at least as high as whoever set the current name, so a
    /// human rename (highest) is never overwritten by the orchestrator's
    /// `rename_agent` (middle) or the id-derived default (lowest); the
    /// orchestrator can still relabel an id-default or its own earlier name.
    /// Rejects a dead/unknown target. On success the pane title follows via an
    /// `orch-rename` event, the roster is updated, the change is audited, and
    /// the applied (trimmed/truncated) name is returned. Caller scopes the
    /// target to its group (see the MCP `rename_agent` tool).
    pub fn rename_agent(&self, agent_id: &str, name: &str, source: NameSource) -> Result<String, String> {
        let name = sanitize_agent_name(name);
        if name.is_empty() {
            return Err("name must not be empty".into());
        }
        let entry = {
            let mut agents = self.agents.lock_safe();
            let a = agents.get_mut(agent_id).ok_or("unknown agent")?;
            if a.status == AgentStatus::Dead {
                return Err("agent is not alive".into());
            }
            if source.rank() < a.name_source.rank() {
                // Only the orchestrator-vs-human case reaches here in practice.
                return Err(format!(
                    "not overriding {agent_id}: its name \"{}\" was set by the human and takes precedence",
                    a.name
                ));
            }
            a.name = name.clone();
            a.name_source = source;
            a.clone()
        };
        self.persist_agent_record(&entry, "running");
        if let Some(app) = self.app.lock_safe().clone() {
            let _ = app.emit(
                "orch-rename",
                json!({ "agent_id": entry.id, "pty_id": entry.pty_id, "name": name }),
            );
        }
        self.audit(&entry.group, "loomux", "agent-rename",
            json!({ "agent": agent_id, "name": name, "source": source.as_str() }));
        Ok(name)
    }

    #[doc(hidden)] // pub for integration tests
    pub fn mark_dead(&self, agent_id: &str, exit_code: Option<u32>) -> Option<AgentEntry> {
        let mut agents = self.agents.lock_safe();
        let a = agents.get_mut(agent_id)?;
        if a.status == AgentStatus::Dead {
            return None;
        }
        a.status = AgentStatus::Dead;
        let snapshot = a.clone();
        drop(agents);
        self.by_token.lock_safe().remove(&snapshot.token);
        if let Some(p) = snapshot.pty_id {
            self.by_pty.lock_safe().remove(&p);
            self.delivery.lock_safe().remove(&p);
        }
        // Attention bookkeeping is per-live-agent; drop this one's entries.
        self.attn_reports.lock_safe().remove(agent_id);
        self.attn_quiet.lock_safe().remove(agent_id);
        self.attn_waiting_ack.lock_safe().remove(agent_id);
        self.attn_emitted.lock_safe().remove(agent_id);
        let _ = fs::remove_file(
            self.group_dir(&snapshot.group).join("configs").join(format!("{agent_id}.json")),
        );
        self.audit(&snapshot.group, "loomux", "agent-exit",
            json!({ "agent": agent_id, "exit_code": exit_code }));
        crate::obs::breadcrumb(
            "agent-dead",
            &format!("agent={agent_id} pty={:?} code={exit_code:?}", snapshot.pty_id),
        );
        self.persist_agent_record(&snapshot, "dead");
        // Durably capture final usage before the pane is fully torn down, so a
        // recycled/killed agent still counts toward the group's lifetime total
        // (issue #42). The transcript remains readable after exit; the
        // statusline does not, but token usage is the source we rely on.
        let cli = self
            .group(&snapshot.group)
            .map(|g| g.guardrails.cli_for(snapshot.role).to_string())
            .unwrap_or_else(|| "claude".to_string());
        let usage = self.compute_usage_snapshot(&snapshot, &cli);
        self.upsert_usage_snapshot(&snapshot.group, usage);
        Some(snapshot)
    }

    /// Called from the pty waiter thread when any pty exits. No-op for ptys
    /// that aren't orchestration agents.
    pub fn on_pty_exit(&self, pty_id: u32, exit_code: Option<u32>) {
        let agent_id = match self.by_pty.lock_safe().get(&pty_id).cloned() {
            Some(id) => id,
            None => return,
        };
        if let Some(a) = self.mark_dead(&agent_id, exit_code) {
            if a.role != Role::Orchestrator {
                let _ = self.deliver_to_orchestrator(
                    &a.group,
                    &format!(
                        "[loomux] agent {} ({}) exited (code {:?}). Update your plan and state accordingly.",
                        a.name, a.id, exit_code
                    ),
                    "loomux",
                );
            }
        }
    }

    #[doc(hidden)] // pub for integration tests
    pub fn state_root(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn bind(&self, agent_id: &str, pty_id: u32) -> Result<(), String> {
        let tx = self
            .pending_binds
            .lock_safe()
            .remove(agent_id)
            .ok_or_else(|| format!("no pending bind for agent {agent_id}"))?;
        tx.send(pty_id).map_err(|_| "spawner is gone (bind timed out)".to_string())
    }

    /// Tell a live-but-slow frontend to drop a queued `orch-spawn-request`
    /// whose backend bind wait just timed out (#106): the minted config has
    /// been cleaned and the pending bind removed, so a pane opened for this
    /// agent now would boot a CLI against a dead config and its late
    /// `bind_agent` would error. Emitting here lets a frontend that received
    /// the request but hasn't opened the pane yet cancel it before that
    /// happens; the deadline stamp (`spawn_request_expired`) and the frontend's
    /// bind-rejection handling are the belt-and-braces for the other orderings.
    /// Best-effort and a no-op in unit tests (no app handle).
    fn emit_spawn_cancelled(&self, group_id: &str, agent_id: &str) {
        if let Some(app) = self.app.lock_safe().clone() {
            let _ = app.emit(
                "orch-spawn-cancelled",
                json!({ "group_id": group_id, "agent_id": agent_id }),
            );
        }
    }
}

/// Background loop that enforces the idle-worker auto-kill guardrail: every
/// `IDLE_REAP_INTERVAL` it kills any worker/reviewer whose idle time has
/// crossed its group's `idle_kill_minutes` (groups with the guardrail off
/// are skipped inside `reap_idle_agents`). Started once at app setup.
pub fn start_idle_reaper(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(IDLE_REAP_INTERVAL);
        reg.reap_idle_agents(now_ms());
    });
}

/// Background loop for the stalled-agent watchdog: every `WATCHDOG_INTERVAL`
/// it nudges the orchestrator (once per stall) about any working agent that
/// has gone silent — no terminal output, no report — past its group's
/// `watchdog_stall_minutes`. Groups with the guardrail off and paused groups
/// are skipped inside `run_watchdog`. Started once at app setup.
pub fn start_watchdog(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(WATCHDOG_INTERVAL);
        reg.run_watchdog(now_ms());
    });
}

/// Background loop for autonomous mode (#83): every `IDLE_TICK_INTERVAL` it
/// enforces autonomy token budgets (suspending a group that has overspent) and
/// then delivers one `[loomux] idle tick` to any autonomous group's orchestrator
/// that has been output-quiet past `IDLE_TICK_MINUTES`, so the template's
/// idle-cadence intake/monitoring actually runs unattended. Non-autonomous and
/// paused groups are skipped inside `run_idle_tick`; the tick is self-regulating
/// (any orchestrator action resets the quiet clock) and hard-capped per hour.
/// Started once at app setup.
pub fn start_idle_tick(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(IDLE_TICK_INTERVAL);
        reg.run_idle_tick(now_ms());
    });
}

/// Free bytes on the disk that hosts `path`: the mounted volume whose mount
/// point is the longest prefix of `path`. `None` if no volume matches (or the
/// listing is empty), so the caller no-ops rather than guessing.
fn free_disk_bytes(path: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

/// Background loop for the low-disk backstop (#134): every `DISK_CHECK_INTERVAL`
/// it samples free space on the workspace drive and, on crossing below the
/// threshold, sends one latched notice per group orchestrator. Started once at
/// app setup. Slow cadence keeps the sysinfo scan negligible.
pub fn start_disk_monitor(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(DISK_CHECK_INTERVAL);
        reg.run_disk_monitor();
    });
}

/// Background loop for the debounced cap-change notice (#79): every
/// `MAX_NOTICE_FLUSH_INTERVAL` it delivers any coalesced max-agents notice
/// whose quiet window has elapsed, so a burst of stepper clicks reaches the
/// orchestrator as one re-plan prompt instead of one per click. Started once
/// at app setup.
pub fn start_max_notice_flusher(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(MAX_NOTICE_FLUSH_INTERVAL);
        reg.flush_due_max_notices(now_ms());
    });
}

/// Background loop for attention routing (#6): every `ATTENTION_INTERVAL` it
/// recomputes which panes need the human (idle-with-prompt, worker reports,
/// human merge gates), pushes the set to the frontend for pane badges, and
/// toasts newly-attention panes in notification-enabled groups. Started once at
/// app setup.
pub fn start_attention(reg: Arc<OrchRegistry>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(ATTENTION_INTERVAL);
        reg.run_attention(now_ms());
    });
}

// ---------- tauri commands ----------

/// The flags the single-pane launcher appends to a `program` command when its
/// "autopilot / allow all" toggle is on (#101). Empty for CLIs with no known
/// unattended surface. Shares `single_pane_autopilot_flags` with the group
/// spawn path so the two can't drift. Stateless (no registry needed).
#[tauri::command]
pub fn agent_autopilot_flags(program: String) -> String {
    single_pane_autopilot_flags(&program)
}

/// Create (or reattach to) an orchestration group and register its
/// orchestrator. Returns the pane spec the frontend opens directly; initial
/// idle workers are spawned in the background once the orchestrator binds.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // launcher-collected guardrails, one field each
pub fn create_orchestration(
    reg: tauri::State<Arc<OrchRegistry>>,
    repo: String,
    initial_workers: u32,
    max_agents: u32,
    agent_cli: String,
    // Per-role CLI overrides (issue #4). Empty inherits `agent_cli`; the
    // launcher sends the picked CLI for each role.
    orchestrator_cli: String,
    worker_cli: String,
    reviewer_cli: String,
    planner_cli: String,
    worker_model: String,
    reviewer_model: String,
    orchestrator_model: String,
    planner_model: String,
    auto_ops: bool,
    idle_kill_minutes: u32,
    max_spawns_per_hour: u32,
    watchdog_stall_minutes: u32,
    // The advanced-orchestrator toggle (#222). Off = this group never opens the
    // repo's `.loomux/workflow.yml` and runs the roster below, exactly as loomux
    // did before workflows existed.
    advanced_orchestrator: bool,
) -> Result<SpawnRequest, String> {
    // The launcher still collects one CLI + model per role — that IS the
    // built-in 4-block roster (#222), just spelled as flat form fields. Convert
    // it here, at the boundary, so the launcher's wire shape is untouched and
    // the backend has blocks from this point on. A repo that declares
    // `.loomux/workflow.yml` overrides this roster in `create_group` — but only
    // when `advanced_orchestrator` is on.
    let blocks = workflow::default_roster(&[
        (Role::Orchestrator, &orchestrator_cli, &orchestrator_model),
        (Role::Worker, &worker_cli, &worker_model),
        (Role::Reviewer, &reviewer_cli, &reviewer_model),
        (Role::Planner, &planner_cli, &planner_model),
    ]);
    create_orchestration_group(
        reg.inner(),
        &repo,
        Guardrails {
            max_agents,
            agent_cli,
            blocks,
            advanced_orchestrator,
            auto_ops,
            idle_kill_minutes,
            max_spawns_per_hour,
            watchdog_stall_minutes,
            // #83: no autonomous budget at launch; the human sets it live via
            // orch_set_autonomy_budget (W2 adds the launcher knob later). 0 = no cap.
            autonomy_budget_tokens: 0,
            // #83: 0 → clamped() applies DEFAULT_IDLE_TICK_MINUTES; live-settable via
            // orch_set_idle_tick_minutes.
            idle_tick_minutes: 0,
            // #83: 0 → clamped() applies DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES; live-settable
            // via orch_set_idle_activity_floor.
            idle_activity_floor_bytes: 0,
        },
        None,
        None,
        initial_workers,
    )
}

/// What turning the **advanced orchestrator** on for `repo` would actually run
/// (#222) — asked by the launcher *before* the human hits Create, so they see the
/// roster they are enabling rather than discovering it in four spawned panes.
///
/// This is deliberately not a second implementation of the schema. It runs the
/// same `load_workflow` + `Guardrails::clamped` that `create_group` runs, on a
/// throwaway `Guardrails`, and reports the resolved blocks — so a preview that
/// disagrees with the launch is a bug in one shared path, not a drift between two.
/// (The workflow *pane* validates the file too, in TypeScript, but that pane is an
/// editor giving live feedback on text; this is the launcher asking the engine.)
///
/// `agent_cli` is the group's default CLI, because a block may inherit from it —
/// the same picker feeds this and the launch.
///
/// Never fails: a broken file is `{ valid: false, errors: [...] }` and the group
/// would fall back to the built-in roster, which is precisely what the launcher
/// needs to say. Nothing here is persisted and no group is created.
#[tauri::command]
pub fn orch_workflow_preview(repo: String, agent_cli: String) -> Value {
    let present = workflow::workflow_file_exists(&repo);
    let (name, blocks, gates, errors, capacity) = match workflow::load_workflow(&repo) {
        Ok(Some(wf)) => {
            let gates: Vec<String> = wf.gates.keys().cloned().collect();
            // #255: same derivation `create_group_ex` records at load time, so the
            // launcher's warning and the audit trail can never disagree about what
            // a launch would compute.
            let capacity = workflow::recommend_capacity(&wf.blocks, wf.gates.get("merge"));
            (wf.name, wf.blocks, gates, Vec::new(), Some(capacity))
        }
        Ok(None) => (String::new(), Vec::new(), Vec::new(), Vec::new(), None),
        Err(errors) => (String::new(), Vec::new(), Vec::new(), errors, None),
    };
    // Resolve exactly as a launch would: `clamped()` is what fills in an
    // inherited CLI's default model, guarantees the orchestrator block, and drops
    // a row a hand-edit could have made unreachable. Without it the preview would
    // show `model: ""` for every block that inherits — i.e. most of them.
    let resolved = if blocks.is_empty() {
        Vec::new()
    } else {
        Guardrails { agent_cli: agent_cli.clone(), blocks, ..Guardrails::default() }
            .clamped()
            .blocks
    };
    let agent_cli = if SUPPORTED_CLIS.contains(&agent_cli.as_str()) { agent_cli } else { "claude".into() };
    json!({
        "path": workflow::WORKFLOW_PATH,
        "present": present,
        "valid": errors.is_empty(),
        "name": name,
        "errors": errors,
        "gates": gates,
        // #255: null when there's no declared workflow to derive from (absent or
        // invalid file) — the launcher has nothing to warn about in that case,
        // since the group would run the built-in roster.
        "min_agents": capacity.map(|c| c.minimum),
        "recommended_agents": capacity.map(|c| c.recommended),
        "blocks": resolved.iter().map(|b| json!({
            "id": b.id,
            "name": b.name,
            "kind": b.kind.as_str(),
            "cli": workflow::cli_of(b, &agent_cli),
            "model": workflow::model_of(b, &agent_cli),
            // What the human is really being asked to consent to: whether this
            // block carries repo-authored instructions for the agent.
            //
            // Asked through the same predicate the SPAWN asks (rev-11's nit): an
            // orchestrator block's persona is denied at `resolve_persona`, so
            // reporting one here would advertise instructions that will never
            // reach an agent. Unreachable from a parsed workflow file, which
            // rejects it outright — but a preview must not be able to claim what a
            // launch would drop, whatever produced the block.
            "persona": if !workflow::persona_allowed(b) {
                "none"
            } else if b.profile.is_some() {
                "profile"
            } else if b.prompt.is_some() {
                "prompt"
            } else {
                "none"
            },
        })).collect::<Vec<_>>(),
    })
}

/// Pause a group: loomux stops delivering prompts/kickoffs so its agents
/// idle out (cost containment). Human action from the pane UI.
#[tauri::command]
pub fn orch_pause_group(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Result<(), String> {
    reg.pause_group(&group_id)
}

/// Resume a paused group: prompt/kickoff delivery flows again.
#[tauri::command]
pub fn orch_resume_group(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Result<(), String> {
    reg.resume_group(&group_id)
}

/// Whether a group is currently paused (drives the pause/resume button state).
#[tauri::command]
pub fn orch_group_paused(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> bool {
    reg.is_paused(&group_id)
}

// ---------- attention routing (human side) ----------

/// The human focused/handled an attention-badged pane: drop its latched report
/// so the badge clears. Live reasons (waiting/gate) are recomputed each scan.
#[tauri::command]
pub fn orch_ack_attention(reg: tauri::State<Arc<OrchRegistry>>, agent_id: String) {
    reg.ack_attention(&agent_id);
}

/// The human turned to a plain (non-agent) pane flagged `waiting` (#40): ack it
/// by pty id, since it has no agent identity to key on.
#[tauri::command]
pub fn orch_ack_attention_pty(reg: tauri::State<Arc<OrchRegistry>>, pty_id: u32) {
    reg.ack_attention_pty(pty_id);
}

/// Whether desktop notifications are enabled for a group (toggle button state).
#[tauri::command]
pub fn orch_notify_enabled(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> bool {
    reg.notify_enabled(&group_id)
}

/// Enable/disable desktop notifications for a group (durable, per-group).
#[tauri::command]
pub fn orch_set_notify(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    enabled: bool,
) -> Result<(), String> {
    reg.set_notify(&group_id, enabled)
}

/// Change a live group's max live-agent cap (durable, bounds-checked, audited).
/// Takes effect on the next spawn; lowering it below the current live count
/// blocks new spawns until attrition rather than killing anyone. Returns the
/// applied value. Human action from the GroupView overlay.
#[tauri::command]
pub fn orch_set_max_agents(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    max_agents: u32,
) -> Result<u32, String> {
    reg.set_max_agents(&group_id, max_agents, "human")
}

/// Aggregate per-pane session cost/usage into one group summary for the UI.
#[tauri::command]
pub fn orch_group_usage(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Value {
    reg.group_usage(&group_id)
}

// ---------- autonomous mode (#83): toggles + budget + state read ----------
//
// FROZEN COMMAND CONTRACT (W2 builds the group-panel UI against this):
//   orch_set_autonomous(group_id, enabled: bool) -> Result<(), String>
//     Flip autonomous idle-tick mode. Enabling anchors the budget meter at the
//     group's current spend; disabling is the explicit consent needed to resume
//     after a budget suspension.
//   orch_set_auto_merge(group_id, enabled: bool) -> Result<(), String>
//     Flip the merge gate. Default OFF = human approval required (today's
//     behavior). ON lets the orchestrator merge adequately-tested PRs itself.
//   orch_set_autonomy_budget(group_id, tokens: u64) -> Result<u64, String>
//     Per-group autonomous-era token budget; 0 = no cap. Returns the applied value.
//   orch_set_idle_tick_minutes(group_id, minutes: u32) -> Result<u32, String>
//     Per-group idle-tick quiet window; 0 → default (5), floored at 1, clamped to
//     1440. Returns the applied value. Lets the human set 1–2 min to verify fast.
//   orch_set_idle_activity_floor(group_id, bytes: u64) -> Result<u64, String>
//     Per-group per-tick byte floor separating a real turn from idle repaint noise;
//     0 → default (2048), floored at 1, clamped to 1 MiB. Returns the applied value.
//     The runtime remedy if a chatty CLI's idle repaints starve the tick.
//   orch_autonomy(group_id) -> Value
//     The whole panel state in one read:
//       { autonomous: bool, auto_merge: bool, budget_tokens: u64,
//         budget_anchor_tokens: u64, spend_since_enable_tokens: u64 | null,
//         suspended: bool, idle_tick_minutes: u32, idle_activity_floor_bytes: u64,
//         quiet_secs: u64 | null, eligible_in_secs: u64 | null,
//         tick_status: "off"|"starting"|"paused"|"counting_down"|"eligible"|"waiting_for_activity"|"rate_capped" }
//     `spend_since_enable_tokens` is null when autonomous is off (no live meter).
//     `suspended` is true iff autonomous is off *because the budget enforcer
//     flipped it* (durable `autonomy_suspended` marker), vs a plain user toggle-off
//     — so the UI shows "budget spent, raise it or re-enable" without parsing the
//     audit log. Always false while autonomous is on.
//     `idle_tick_minutes`/`idle_activity_floor_bytes` are the active knobs.
//     `tick_status` is the honest idle-tick state; `eligible_in_secs` is a REAL
//     countdown ONLY for `counting_down`/`eligible`/`rate_capped` (it never shows a
//     lying 0 while the latch gates the tick — then it is null with status
//     `waiting_for_activity`). Both are null while off / no live orchestrator.

/// Enable/disable autonomous idle-tick mode for a group (durable, audited).
#[tauri::command]
pub fn orch_set_autonomous(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    enabled: bool,
) -> Result<(), String> {
    reg.set_autonomous(&group_id, enabled)
}

/// Enable/disable the auto-merge gate for a group (durable, audited). Default OFF
/// = human merges; ON lets the orchestrator merge adequately-tested PRs itself.
#[tauri::command]
pub fn orch_set_auto_merge(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    enabled: bool,
) -> Result<(), String> {
    reg.set_auto_merge(&group_id, enabled)
}

/// Enable/disable the auto-release gate for a group (durable, audited, independent
/// of auto-merge). Default OFF = releases/tags need a per-tag human grant; ON lets
/// the orchestrator publish releases itself while autonomous. Rejects enable unless
/// autonomous is on.
#[tauri::command]
pub fn orch_set_auto_release(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    enabled: bool,
) -> Result<(), String> {
    reg.set_auto_release(&group_id, enabled)
}

/// Enable/disable supervised dangerous mode for a group (#83, durable, audited).
/// Lets the human — present and supervising — authorize the orchestrator to merge
/// to the default branch and publish releases/tags itself, WITHOUT autonomous mode.
/// Mutually exclusive with autonomous: rejects enable while autonomous is on, and
/// enabling autonomous force-clears it.
#[tauri::command]
pub fn orch_set_dangerous_mode(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    enabled: bool,
) -> Result<(), String> {
    reg.set_dangerous_mode(&group_id, enabled)
}

/// Set a group's autonomous-era token budget (0 = no cap; durable, audited).
/// Returns the applied value.
#[tauri::command]
pub fn orch_set_autonomy_budget(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    tokens: u64,
) -> Result<u64, String> {
    reg.set_autonomy_budget(&group_id, tokens)
}

/// Set a group's idle-tick quiet window in minutes (0 → default; floored at 1,
/// clamped to the max; durable, audited). Returns the applied value. Lets the
/// human drop it to 1–2 min to verify autonomous mode fires quickly.
#[tauri::command]
pub fn orch_set_idle_tick_minutes(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    minutes: u32,
) -> Result<u32, String> {
    reg.set_idle_tick_minutes(&group_id, minutes)
}

/// Set a group's idle-tick activity floor in bytes (0 → default; floored at 1,
/// clamped to 1 MiB; durable, audited). Returns the applied value. The runtime
/// remedy if a chatty CLI's idle repaints exceed the default and starve the tick.
#[tauri::command]
pub fn orch_set_idle_activity_floor(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    bytes: u64,
) -> Result<u64, String> {
    reg.set_idle_activity_floor(&group_id, bytes)
}

/// The group's autonomous-mode state for the panel: toggles, budget, anchor, and
/// spend-since-enable. Single read the UI renders all three controls from.
#[tauri::command]
pub fn orch_autonomy(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Value {
    reg.autonomy_state(&group_id)
}

/// Live-agent count, role breakdown, and uptime for the lifecycle panel.
#[tauri::command]
pub fn orch_group_summary(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Value {
    reg.group_summary(&group_id)
}

/// End a whole orchestration: kill all its agents and (optionally) remove
/// their worktrees. Human-initiated, destructive, audited — the frontend
/// confirms before calling this.
#[tauri::command]
pub fn orch_end_group(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    cleanup_worktrees: bool,
) -> Result<Value, String> {
    reg.end_group(&group_id, cleanup_worktrees)
}

/// Create (or reattach to) a group and register its orchestrator, under the
/// creation lock: the group id is picked by liveness, and a group only
/// becomes live once its orchestrator is registered, so id selection and
/// registration must be atomic against concurrent launches.
/// `expect_group` pins restores to their recorded group id.
pub fn create_orchestration_group(
    reg: &Arc<OrchRegistry>,
    repo: &str,
    guardrails: Guardrails,
    resume_session: Option<String>,
    expect_group: Option<&str>,
    initial_workers: u32,
) -> Result<SpawnRequest, String> {
    // Paths are interpolated into a quoted shell line; a quote inside one
    // would escape it. (Windows filesystems forbid `"` in names; this
    // guards the Unix builds and hand-typed paths.)
    if repo.contains('"') {
        return Err("repository path must not contain a quote character".into());
    }
    if !Path::new(repo).is_dir() {
        return Err(format!("repository path does not exist: {repo}"));
    }
    let _creation = reg.creation.lock_safe();
    // A resume reopens a recorded orchestrator session; anything else is the human
    // at the launcher, who has just been shown what the advanced orchestrator would
    // run. Only the latter reads the repo's workflow file (#222) — see [`Launch`].
    let launch = if resume_session.is_some() { Launch::Resume } else { Launch::Fresh };
    let group = reg.create_group_ex(repo, guardrails, launch)?;
    if let Some(want) = expect_group {
        if group.id != want {
            return Err(format!(
                "group id mismatch (recorded {want}, resolved {}) — another orchestration is live on this repo",
                group.id
            ));
        }
    }
    register_orchestrator_pane(reg, &group, resume_session, initial_workers)
}

/// Register a group's orchestrator and hand back the pane spec the frontend
/// opens. `resume_session` reopens a prior orchestrator conversation (with
/// fresh MCP wiring) instead of starting cold. A background thread waits
/// for the pane bind, types the kickoff/re-sync prompt, and brings up any
/// initial idle workers.
fn register_orchestrator_pane(
    reg: &Arc<OrchRegistry>,
    group: &GroupInfo,
    resume_session: Option<String>,
    initial_workers: u32,
) -> Result<SpawnRequest, String> {
    // The orchestrator is a block like any other (#222) — it just isn't spawned
    // through `spawn_agent_ex`, because a group has exactly one and it is minted
    // at launch. A workflow file may still give it a persona and its own
    // CLI/model.
    let block = group
        .guardrails
        .block_for(Role::Orchestrator)
        .cloned()
        .ok_or("this group's workflow declares no orchestrator block")?;
    let model = workflow::model_of(&block, &group.guardrails.agent_cli).to_string();
    // It must be a supported CLI; the launcher only offers supported ones and
    // the workflow parser rejects unknown ones, so an unknown value here is a
    // hand-edited group.json.
    let cli = workflow::cli_of(&block, &group.guardrails.agent_cli);
    if !SUPPORTED_CLIS.contains(&cli) {
        return Err(format!(
            "unsupported orchestrator CLI {cli:?} — supported: {}",
            SUPPORTED_CLIS.join(", ")
        ));
    }
    let cli = cli.to_string();
    let token = new_token();
    let agent_id = format!("orch-{}", reg.seq.fetch_add(1, Ordering::SeqCst) + 1);
    if cli == "copilot" {
        pre_trust_copilot_folder(&group.repo);
    }
    let cfg = reg.write_mcp_config(&group.id, &agent_id, &token, &cli)?;
    let resume = resume_session.is_some();
    let session_id = match resume_session {
        Some(s) => Some(sanitize_session(&s).ok_or("invalid resume session id")?),
        None => (cli == "claude").then(new_session_uuid),
    };
    // Copilot mints its own id on boot; snapshot existing sessions now so the
    // orchestrator's newly created one can be tracked (this is what gives a
    // copilot orchestration its ORCH chip and restore).
    let copilot_baseline = (!resume && cli == "copilot")
        .then(|| {
            crate::sessions::copilot_session_state_root()
                .map(|root| crate::sessions::copilot_session_ids(&root))
                .unwrap_or_default()
        });
    // The orchestrator block's persona, if the workflow file gave it one. A
    // broken one is audited and dropped, never fatal.
    let persona = reg.resolve_persona_or_audit(group, &block);
    let inject = reg.persona_inject(&group.id, &block, &cli, persona.as_ref());
    let command = reg.build_agent_command(
        &cli,
        &model,
        group.guardrails.auto_ops,
        &cfg,
        &reg.group_dir(&group.id),
        Path::new(&group.repo),
        session_id.as_deref(),
        resume,
        false, // the orchestrator is never read-only
        &inject,
    );
    let argv = reg.build_agent_argv(
        &cli,
        &model,
        group.guardrails.auto_ops,
        &cfg,
        &reg.group_dir(&group.id),
        Path::new(&group.repo),
        session_id.as_deref(),
        resume,
        false,
        &inject,
    );
    let entry = AgentEntry {
        id: agent_id.clone(),
        group: group.id.clone(),
        name: "orchestrator".into(),
        // A stable, meaningful single-orchestrator label — treated as the
        // id-default tier so it never blocks anything (the rename tool targets
        // worker/reviewer panes, not the orchestrator).
        name_source: NameSource::Default,
        block: block.id.clone(),
        role: Role::Orchestrator,
        token: token.clone(),
        status: AgentStatus::Starting,
        pty_id: None,
        task: String::new(),
        session_id,
        cwd: group.repo.clone(),
        idle_since_ms: None, // the orchestrator is never idle-reaped
        started_ms: now_ms(),
        last_progress_ms: now_ms(), // watchdog ignores the orchestrator; the
        // idle-tick (#83) reuses this as the orchestrator's output-quiet clock.
        last_output_total: 0,
        watchdog_notified: false,
        idle_tick_notified: false,
    };
    reg.agents.lock_safe().insert(agent_id.clone(), entry.clone());
    reg.by_token.lock_safe().insert(token, agent_id.clone());
    reg.persist_agent_record(&entry, "running");
    reg.audit(&group.id, "loomux", "agent-spawn",
        json!({ "agent": agent_id, "role": "orchestrator", "model": model,
                "session": entry.session_id, "resume": resume }));

    let request = SpawnRequest {
        group_id: group.id.clone(),
        agent_id: agent_id.clone(),
        role: Role::Orchestrator,
        name: "orchestrator".into(),
        cwd: group.repo.clone(),
        command,
        // Expire the request when the background bind wait below would (#106).
        deadline_ms: now_ms() + BIND_TIMEOUT.as_millis() as u64,
        argv,
        // The orchestrator is the pane the incident implicated — inject the
        // gh-shim env so its merge gate is enforced too (#83).
        env: reg.agent_pane_env(&group.id),
    };

    crate::obs::breadcrumb(
        "agent-spawn",
        &format!("group={} agent={agent_id} role=Orchestrator resume={resume}", group.id),
    );

    if reg.app.lock_safe().is_none() {
        // Test mode: no frontend; mark running without a pane. Tolerate a
        // vanished entry rather than unwrapping under the agents lock.
        if let Some(a) = reg.agents.lock_safe().get_mut(&agent_id) {
            a.status = AgentStatus::Running;
        }
        return Ok(request);
    }

    // Background: wait for the orchestrator pane to bind, type its kickoff,
    // then bring up the initial idle workers one by one.
    let (tx, rx) = mpsc::channel::<u32>();
    reg.pending_binds.lock_safe().insert(agent_id.clone(), tx);
    let reg2 = reg.clone();
    let group2 = group.clone();
    // Moved into the bind thread: the kickoff-injection fallback for an
    // orchestrator block whose persona can't ride on a native flag (copilot +
    // an inline `prompt:`). `None` for the built-in roster.
    let kickoff_persona = inject.kickoff.clone();
    std::thread::spawn(move || {
        let Ok(pty_id) = rx.recv_timeout(BIND_TIMEOUT) else {
            reg2.pending_binds.lock_safe().remove(&agent_id);
            reg2.mark_dead(&agent_id, None);
            // Cancel the queued request frontend-side (#106) — see spawn_agent_ex.
            reg2.emit_spawn_cancelled(&group2.id, &agent_id);
            return;
        };
        {
            let mut agents = reg2.agents.lock_safe();
            if let Some(a) = agents.get_mut(&agent_id) {
                a.status = AgentStatus::Running;
                a.pty_id = Some(pty_id);
            }
        }
        reg2.by_pty.lock_safe().insert(pty_id, agent_id.clone());
        reg2.audit(&group2.id, "loomux", "agent-bind", json!({ "agent": agent_id, "pty": pty_id }));
        crate::obs::breadcrumb("agent-bind", &format!("agent={agent_id} pty={pty_id} role=Orchestrator"));
        let kickoff = if resume {
            "[loomux] Orchestration restored: your MCP tools, the task board, and the audit log are live again in this session. Re-sync now: list_tasks, list_agents, get_state. Your previous worker panes are gone; resume a task session with spawn_agent(resume_session, cwd) when follow-ups need it. Then give the human a short status summary.".to_string()
        } else {
            match reg2.agent(&agent_id) {
                Some(a) => reg2.kickoff_prompt(&a, &group2, "", kickoff_persona.as_deref()),
                None => return, // agent reaped before bind; nothing to kick off
            }
        };
        let delivery = if resume { Delivery::ResumeKickoff } else { Delivery::FreshKickoff };
        let _ = reg2.deliver_prompt(&agent_id, &kickoff, "loomux", delivery);
        // Track the copilot session this orchestrator just minted.
        if let Some(baseline) = copilot_baseline {
            reg2.clone().spawn_copilot_session_watcher(
                agent_id.clone(),
                group2.id.clone(),
                group2.repo.clone(),
                baseline,
            );
        }
        // The launcher's "initial workers" count assumes the group HAS a worker
        // block. A repo whose `.loomux/workflow.yml` declares only reviewers
        // (a review-only workflow) has none (#222) — and then every spawn below
        // would fail with "declares no worker block", the human would get zero
        // panes, and the only trace would be an audit line they'd have to go
        // looking for. Say it out loud in the orchestrator's pane instead.
        let starters = initial_workers.min(group2.guardrails.max_agents);
        if starters > 0 && group2.guardrails.block_for(Role::Worker).is_none() {
            reg2.audit(&group2.id, "loomux", "initial-workers-skipped", json!({
                "requested": starters,
                "why": "this repo's workflow declares no worker block",
            }));
            let _ = reg2.deliver_to_orchestrator(
                &group2.id,
                &format!(
                    "[loomux] the launcher asked for {starters} initial worker(s), but this repo's \
                     {} declares no worker block — none were opened. Spawn the blocks it does \
                     declare instead (they are listed above).",
                    workflow::WORKFLOW_PATH
                ),
                "loomux",
            );
        } else {
            for _ in 0..starters {
                // Empty name → derived from the minted id ("worker 2" for `w-2`),
                // so the pane title agrees with its "W 2" badge instead of the old
                // per-launch counter that drifted from the seq (#95r).
                if let Err(e) = reg2.spawn_agent(&group2.id, Role::Worker, "", "", false, None)
                {
                    reg2.audit(&group2.id, "loomux", "error",
                        json!({ "what": "initial worker spawn failed", "err": e }));
                    break;
                }
            }
        }
    });

    Ok(request)
}

/// Restore orchestration for a recorded session id (from the session
/// browser). An orchestrator session of a dead group relaunches the whole
/// control plane — group, MCP identity, task board — resuming that
/// conversation, and returns the pane spec for the frontend to open. A
/// worker/reviewer session rejoins its live group; its pane arrives via the
/// normal orch-spawn-request event (the spawn must not block this IPC
/// thread, which also serves the bind), so `None` is returned.
pub fn resume_recorded_session(
    reg: &Arc<OrchRegistry>,
    session_id: &str,
    hint: Option<(String, String)>, // (group_id, role) from transcript signatures
) -> Result<Option<SpawnRequest>, String> {
    let record = reg
        .session_roles()
        .into_iter()
        .filter(|r| r.session_id == session_id)
        .last()
        .or_else(|| {
            // Sessions from before the roster (and before session-id
            // tracking) are identified by loomux signatures in their own
            // transcript; trust the hint if that group exists on disk.
            let (group_id, role) = hint?;
            if !reg.group_dir(&group_id).join("group.json").is_file() {
                return None;
            }
            let group_live = reg.group_is_live(&group_id);
            Some(SessionRole {
                session_id: session_id.to_string(),
                agent_name: if role == "orchestrator" { "orchestrator".into() } else { "agent".into() },
                group_id,
                role,
                group_live,
            })
        })
        .ok_or("this session is not part of a recorded orchestration")?;

    if record.role == "orchestrator" {
        if record.group_live {
            return Err(format!(
                "group {} already has a live orchestrator — focus its pane instead",
                record.group_id
            ));
        }
        let (repo, guardrails) = reg
            .load_group_file(&record.group_id)
            .ok_or("group.json is missing for this orchestration")?;
        return create_orchestration_group(
            reg,
            &repo,
            guardrails,
            Some(session_id.to_string()),
            Some(&record.group_id),
            0,
        )
        .map(Some);
    }

    // Worker / reviewer: only meaningful inside a live group.
    if !record.group_live {
        return Err(
            "this agent's group is not running — restart its orchestrator session (marked ORCH) first"
                .into(),
        );
    }
    // #222: an unrecognized role is REJECTED, not silently coerced to worker.
    // This was the second of the two coercion sites (the other was the MCP
    // `spawn_agent` kind parser); a persisted role loomux cannot name means the
    // roster row is corrupt or from a future build, and rejoining it as a worker
    // would hand it a worktree and write access on nothing but a guess.
    let role = workflow::kind_from_str(&record.role).ok_or_else(|| {
        format!(
            "this session's recorded role {:?} is not a known capability class ({}) — refusing to rejoin it",
            record.role,
            workflow::kind_names()
        )
    })?;
    // Pull the durable roster row for this session: its cwd (where the work
    // happened) and its name tier — so a human-renamed pane rejoins at the
    // `Human` tier and stays un-clobberable, not silently demoted to
    // orchestrator (#95r). Absent (hint-restored, pre-roster) → `None`, and
    // spawn derives the tier from the name as usual.
    let matched = reg
        .merged_records(&record.group_id)
        .into_iter()
        .find(|r| r.session.as_deref() == Some(session_id));
    let cwd = matched.as_ref().map(|r| r.cwd.clone()).filter(|c| Path::new(c).is_dir());
    let restore_source = matched.as_ref().map(|r| r.name_source);
    let reg2 = reg.clone();
    let sid = session_id.to_string();
    let (group_id, name) = (record.group_id.clone(), record.agent_name.clone());
    // Rejoin as the same BLOCK, not just the same class (#222) — a resumed
    // `rev-security` session must come back with its persona, not as a generic
    // reviewer. Absent (a roster row from before blocks) → `None` → the class's
    // default block, which for the built-in roster is the same thing.
    //
    // A recorded block that is no longer in the roster (the workflow file renamed
    // or dropped it since that session ran) degrades to `None` — the class default
    // — rather than failing the rejoin. Losing the persona is a downgrade; losing
    // the *session* is data loss, and the human has no other way to reach it.
    // `spawn_agent_ex` is deliberately strict about an unknown block id, because
    // for `spawn_agent(block:)` a typo should be an error — so the fallback has to
    // happen here, where "stale" and "wrong" are distinguishable. (`kickoff_prompt`
    // already falls back the same way for the instructions path.)
    let block = matched
        .as_ref()
        .map(|r| r.block.clone())
        .filter(|b| !b.trim().is_empty())
        .filter(|b| {
            let known = reg
                .group(&record.group_id)
                .is_some_and(|g| g.guardrails.block(b).is_some());
            if !known {
                reg.audit(&record.group_id, "loomux", "rejoin-block-missing", json!({
                    "session": session_id, "block": b,
                    "action": "rejoining as the default block for its capability class",
                }));
            }
            known
        });
    std::thread::spawn(move || {
        if let Err(e) = reg2.spawn_agent_ex(
            &group_id, role, block, &name, "", false, None, None, Some(sid.clone()), cwd, restore_source,
        ) {
            reg2.audit(&group_id, "loomux", "error",
                json!({ "what": "session rejoin failed", "session": sid, "err": e.clone() }));
            let _ = reg2.deliver_to_orchestrator(
                &group_id,
                &format!("[loomux] failed to resume session {sid} into this group: {e}"),
                "loomux",
            );
        }
    });
    Ok(None)
}

#[tauri::command]
pub fn bind_agent(reg: tauri::State<Arc<OrchRegistry>>, agent_id: String, pty_id: u32) -> Result<(), String> {
    reg.bind(&agent_id, pty_id)
}

/// The human renamed an agent pane in-place (F2 / double-click). Sync the
/// backend so the roster name matches the pane title AND the rename is
/// recorded at the highest precedence tier — an orchestrator `rename_agent`
/// afterwards will not override it (#95r). Best-effort: the pane already shows
/// the new name locally, so a stale/unknown id just fails silently here.
#[tauri::command]
pub fn orch_agent_renamed(
    reg: tauri::State<Arc<OrchRegistry>>,
    agent_id: String,
    name: String,
) -> Result<(), String> {
    reg.rename_agent(&agent_id, &name, NameSource::Human).map(|_| ())
}

/// Session ↔ orchestration-role mapping for the session browser badges.
#[tauri::command]
pub fn orch_session_roles(reg: tauri::State<Arc<OrchRegistry>>) -> Vec<SessionRole> {
    reg.session_roles()
}

/// Restore a recorded orchestration session (see `resume_recorded_session`).
/// Returns the orchestrator pane spec, or null when the pane will arrive
/// via `orch-spawn-request` (worker/reviewer rejoin).
#[tauri::command]
pub fn resume_orch_session(
    reg: tauri::State<Arc<OrchRegistry>>,
    session_id: String,
    group_hint: Option<String>,
    role_hint: Option<String>,
) -> Result<Option<SpawnRequest>, String> {
    let hint = group_hint.zip(role_hint);
    resume_recorded_session(reg.inner(), &session_id, hint)
}

// ---------- merge-gate link resolution ----------
// The board stores issue/PR references as the orchestrator typed them
// (`#12`, a bare number, or a full URL). To make the chips clickable we
// resolve those to a web URL against the repo's `origin` remote.

/// Normalize a git remote URL (`git@`, `ssh://`, `https://`, with or without
/// a trailing `.git`) into its browsable web base, e.g.
/// `https://github.com/owner/repo`. None for anything that doesn't look like
/// a host/path we can turn into a link.
#[doc(hidden)] // pub for integration tests
pub fn normalize_remote_web_base(url: &str) -> Option<String> {
    let u = url.trim();
    if u.is_empty() {
        return None;
    }
    // Split into host and path, covering the three shapes git emits.
    let (host, path) = if let Some(rest) = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .or_else(|| u.strip_prefix("ssh://"))
    {
        // scheme://[user@]host[:port]/owner/repo
        let rest = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        // Drop any :port from the host part (ssh URLs may carry one).
        let host = host.split(':').next().unwrap_or(host);
        (host.to_string(), path.to_string())
    } else if let Some(rest) = u.strip_prefix("git@") {
        // scp-like: git@host:owner/repo.git
        let (host, path) = rest.split_once(':')?;
        (host.to_string(), path.to_string())
    } else {
        return None;
    };
    let host = host.trim().trim_end_matches('/');
    let path = path.trim().trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() || !host.contains('.') {
        return None;
    }
    Some(format!("https://{host}/{path}"))
}

/// Web base for a repo's `origin` remote (falling back to any remote), or
/// None when the repo has no usable remote.
fn git_remote_web_base(repo: &str) -> Option<String> {
    if !Path::new(repo).is_dir() {
        return None;
    }
    let run = |args: &[&str]| -> Option<String> {
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(repo).args(args).env("GIT_TERMINAL_PROMPT", "0");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let url = run(&["remote", "get-url", "origin"]).or_else(|| {
        // No `origin` — take the first remote git lists, if any.
        let name = run(&["remote"])?.lines().next()?.trim().to_string();
        (!name.is_empty()).then_some(name).and_then(|n| run(&["remote", "get-url", &n]))
    })?;
    normalize_remote_web_base(&url)
}

/// Resolve a stored issue/PR reference to a URL. `value` may already be a
/// full URL (used verbatim); otherwise it's a `#N`/`N` reference resolved
/// against `base`. `kind` is `"issue"` or `"pr"`. None when there's nothing
/// clickable (no number, or a bare number with no known remote).
#[doc(hidden)] // pub for integration tests
pub fn resolve_ref_url(base: Option<&str>, kind: &str, value: &str) -> Option<String> {
    let v = value.trim();
    if v.starts_with("https://") || v.starts_with("http://") {
        return Some(v.to_string());
    }
    // Pull the first run of digits out of `#12`, `12`, `GH-12`, etc.
    let num: String = v
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    if num.is_empty() {
        return None;
    }
    // GitHub redirects /issues/N <-> /pull/N, so a kind mismatch still lands.
    let seg = if kind == "issue" { "issues" } else { "pull" };
    Some(format!("{}/{seg}/{num}", base?.trim_end_matches('/')))
}

/// WinRT toast script (see `notify_desktop`). Title/body come in via
/// environment variables — never interpolated into the script — so agent/board
/// text can't inject PowerShell. XML-escaped before templating. The AppUserModel
/// id is the stock PowerShell shortcut, which lets an unpackaged process raise a
/// toast on Windows 10; it renders attributed to PowerShell, which is fine for
/// an optional signal.
#[cfg(target_os = "windows")]
const TOAST_PS1: &str = r#"
$ErrorActionPreference='SilentlyContinue'
[void][Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime]
[void][Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom,ContentType=WindowsRuntime]
$t=[System.Security.SecurityElement]::Escape($env:LOOMUX_TOAST_TITLE)
$b=[System.Security.SecurityElement]::Escape($env:LOOMUX_TOAST_BODY)
$xml="<toast><visual><binding template='ToastGeneric'><text>$t</text><text>$b</text></binding></visual></toast>"
$doc=New-Object Windows.Data.Xml.Dom.XmlDocument
$doc.LoadXml($xml)
$toast=New-Object Windows.UI.Notifications.ToastNotification $doc
$app='{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show($toast)
"#;

/// Best-effort OS desktop notification (attention routing #6). On Windows this
/// spawns a hidden PowerShell that raises a WinRT toast, passing the title/body
/// as environment variables (injection-proof — see `TOAST_PS1`). Deliberately
/// no notification crate: those pull getrandom, which this project's Windows 10
/// baseline can't load (0xc0000139 — see the Cargo.toml note). Silently a no-op
/// on failure and on non-Windows; the pane badges and board highlight are the
/// primary signal regardless.
#[cfg(target_os = "windows")]
fn notify_desktop(title: &str, body: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", TOAST_PS1])
        .env("LOOMUX_TOAST_TITLE", title)
        .env("LOOMUX_TOAST_BODY", body)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

#[cfg(not(target_os = "windows"))]
fn notify_desktop(_title: &str, _body: &str) {}

/// Open an http(s) URL in the user's default browser. The URL is passed to
/// the OS handler as a single process argument (never a shell line), and is
/// validated first so a crafted board reference can't smuggle anything.
fn open_external_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("refusing to open a non-http(s) URL".into());
    }
    if url.len() > 2048 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("unsafe URL".into());
    }
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // rundll32 takes the URL as one argument, sidestepping cmd.exe's
        // `start` metacharacter handling.
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| format!("could not open browser: {e}"))
}

// ---------- task board (human side) ----------
// The pane overlay edits the same tasks.json the orchestrator manages via
// MCP. Human edits are audited as actor "human" and (except reorders, which
// are too chatty) surface in the orchestrator pane as a typed notice.

#[tauri::command]
pub fn orch_tasks(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Vec<Task> {
    reg.tasks(&group_id)
}

/// Audit-log timeline for the pane's audit-viewer overlay (read-only). Oldest
/// first; the frontend filters, expands prompt texts, and — in follow mode —
/// re-polls this command.
#[tauri::command]
pub fn orch_audit(reg: tauri::State<Arc<OrchRegistry>>, group_id: String) -> Vec<AuditEntry> {
    reg.audit_log(&group_id)
}

/// Human steering from the loomux compose strip (#43, option C): enqueue
/// `text` to the group's orchestrator through the SAME per-pane serialized
/// delivery path worker reports use, so loomux is the single writer to the
/// pane's stdin and messages land whole (never interleaved; relative order of
/// near-simultaneous sends is best-effort — the per-pty delivery mutex is not
/// FIFO). Empty text, a paused group, and a dead orchestrator all surface as
/// errors the strip shows the human.
#[tauri::command]
pub fn orch_steer(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    text: String,
) -> Result<(), String> {
    reg.steer_orchestrator(&group_id, &text)
}

/// Result of saving a steering-strip attachment: the absolute file path plus
/// the resolved orchestrator CLI, so the frontend can format the in-prompt
/// reference the way that CLI consumes it (Claude reads a plain path; Copilot
/// documents an `@<path>` mention — #72 review note 3).
#[derive(serde::Serialize)]
pub struct SavedAttachment {
    pub path: String,
    pub cli: String,
}

/// Save an image pasted/attached into the steering strip (#72). The image rides
/// over IPC as base64 (`data_b64`) — same wire form as the OSC 52 clipboard
/// bridge — so it survives any webview that won't hand raw bytes through
/// `invoke`. Returns the saved path and the group's orchestrator CLI; the
/// frontend turns those into the per-CLI "Attached image" reference line before
/// sending through `orch_steer`.
#[tauri::command]
pub fn orch_save_attachment(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    ext: String,
    data_b64: String,
) -> Result<SavedAttachment, String> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    // Reject an oversize payload before decoding — see MAX_ATTACHMENT_B64_LEN.
    if data_b64.len() > MAX_ATTACHMENT_B64_LEN {
        return Err(format!(
            "attachment too large (max {MAX_ATTACHMENT_BYTES} bytes)"
        ));
    }
    let bytes = B64
        .decode(data_b64.as_bytes())
        .map_err(|e| format!("invalid attachment encoding: {e}"))?;
    let path = reg.save_attachment(&group_id, &ext, &bytes)?;
    Ok(SavedAttachment {
        path: path.to_string_lossy().to_string(),
        cli: reg.orchestrator_cli(&group_id),
    })
}

#[tauri::command]
pub fn orch_upsert_task(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: Option<String>,
    title: Option<String>,
    status: Option<String>,
    note: Option<String>,
) -> Result<Task, String> {
    let task = reg.upsert_task(
        &group_id,
        "human",
        id.as_deref(),
        TaskPatch { title, status, note, ..Default::default() },
    )?;
    reg.notify_board_edit(&group_id, &format!("{} \"{}\" is now {}", task.id, task.title, task.status));
    Ok(task)
}

#[tauri::command]
pub fn orch_delete_task(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: String,
) -> Result<(), String> {
    reg.delete_task(&group_id, "human", &id)?;
    reg.notify_board_edit(&group_id, &format!("deleted task {id}"));
    Ok(())
}

/// Delete all `done` tasks in one action. The single board-change notice is
/// emitted inside `delete_done_tasks` (coalesced for the batch, #120), so —
/// unlike the single-delete command — none is fanned out here. Returns the ids
/// removed so the frontend can confirm what it cleared.
#[tauri::command]
pub fn orch_delete_done_tasks(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
) -> Result<Vec<String>, String> {
    reg.delete_done_tasks(&group_id, "human")
}

/// Delete a specific set of tasks by id — the board's multi-select "delete
/// selected" action. Mirrors `orch_delete_done_tasks`: the single coalesced
/// board-change notice is emitted inside `delete_tasks` (#120), so — unlike the
/// single-delete command — none is fanned out here. Unknown ids are skipped
/// (the board may have changed under the selection), not errored. Returns the
/// ids actually removed so the frontend can confirm what it cleared.
#[tauri::command]
pub fn orch_delete_tasks(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    ids: Vec<String>,
) -> Result<Vec<String>, String> {
    reg.delete_tasks(&group_id, "human", &ids)
}

#[tauri::command]
pub fn orch_reorder_tasks(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    ids: Vec<String>,
) -> Result<(), String> {
    // No typed notice: reorders come in bursts; board order is read via
    // list_tasks whenever the orchestrator plans.
    reg.reorder_tasks(&group_id, "human", &ids)
}

// ---------- merge-gate actions (human side) ----------
// The human's gatekeeping touchpoints on `pr` / `human-testing` items. Each
// records on the board (audited, actor "human") and delivers a purpose-built
// typed notice into the orchestrator's CLI so it can act on the decision.

/// Open a task's issue or PR reference in the default browser. `kind` is
/// `"issue"` or `"pr"`; `value` is the stored reference (`#12`, `12`, or a
/// full URL).
#[tauri::command]
pub fn orch_open_ref(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    kind: String,
    value: String,
) -> Result<(), String> {
    let repo = reg
        .group(&group_id)
        .map(|g| g.repo)
        .or_else(|| reg.load_group_file(&group_id).map(|(repo, _)| repo))
        .ok_or("unknown group")?;
    let base = git_remote_web_base(&repo);
    let url = resolve_ref_url(base.as_deref(), &kind, &value)
        .ok_or("no URL for this reference — the repo may have no GitHub remote")?;
    reg.audit(&group_id, "human", "open-ref", json!({ "kind": kind, "url": url }));
    open_external_url(&url)
}

/// Approve a merge-gate item: mark it done and notify the orchestrator to
/// merge. The human's direct sign-off, so the status change is applied here.
#[tauri::command]
pub fn orch_approve_task(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: String,
    // Optional approve-with-comment note (#83): delivered to the orchestrator with
    // the one-time merge grant, e.g. "approved — also bump the changelog first".
    comment: Option<String>,
) -> Result<Task, String> {
    reg.approve_task(&group_id, &id, comment.as_deref())
}

/// Issue a one-time human merge grant for a PR (#83), independent of the board —
/// a human-pane path to authorize exactly one default-branch merge. Optional
/// comment is delivered to the orchestrator with the grant. HUMAN-ONLY (Tauri
/// command; no MCP tool reaches grant-writing).
#[tauri::command]
pub fn orch_grant_merge(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    pr: String,
    comment: Option<String>,
) -> Result<u64, String> {
    reg.grant_merge(&group_id, &pr, comment.as_deref(), "human")
}

/// Issue a one-time human release/tag grant for `tag` (#83): authorizes one
/// `gh release …`/tag-push of that tag. Releases are never blanket-allowed by
/// autonomous mode, so this explicit grant is the only path. Optional comment
/// delivered to the orchestrator. HUMAN-ONLY.
#[tauri::command]
pub fn orch_grant_release(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    tag: String,
    comment: Option<String>,
) -> Result<(), String> {
    reg.grant_release(&group_id, &tag, comment.as_deref(), "human")
}

/// Request changes on a merge-gate item: record the findings and deliver them
/// to the orchestrator to route back to a worker.
#[tauri::command]
pub fn orch_request_changes(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: String,
    findings: String,
) -> Result<Task, String> {
    reg.request_changes(&group_id, &id, &findings)
}

/// Start a queued item: record a human-attributed note and tell the
/// orchestrator to begin work. Does not flip the status — the orchestrator
/// moves it to `in-progress` when it actually assigns a worker.
#[tauri::command]
pub fn orch_start_task(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: String,
) -> Result<Task, String> {
    reg.start_task(&group_id, &id)
}

/// Proceed on a prototype item (#147): flip it to `in-progress`, record the
/// human's sign-off, and tell the orchestrator to promote the prototype to a
/// full production build. The human's demo-gate verdict, so the status change
/// is applied here (mirrors `orch_approve_task`).
#[tauri::command]
pub fn orch_proceed_task(
    reg: tauri::State<Arc<OrchRegistry>>,
    group_id: String,
    id: String,
) -> Result<Task, String> {
    reg.proceed_task(&group_id, &id)
}

#[cfg(test)]
mod hold_tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(4);
    const CAP: Duration = Duration::from_secs(90);

    #[test]
    fn holds_while_human_typed_recently() {
        // Typed 1s ago (< 4s window), well under the cap: keep holding.
        assert!(should_hold_for_user(9_000, 10_000, Duration::from_secs(5), WINDOW, CAP));
    }

    #[test]
    fn proceeds_once_human_is_quiet() {
        // Last keystroke was 5s ago (> 4s window): deliver.
        assert!(!should_hold_for_user(5_000, 10_000, Duration::from_secs(2), WINDOW, CAP));
    }

    #[test]
    fn proceeds_when_nobody_typed() {
        // 0 == no keystroke ever recorded for this pane.
        assert!(!should_hold_for_user(0, 10_000, Duration::ZERO, WINDOW, CAP));
    }

    #[test]
    fn cap_forces_delivery_even_if_still_typing() {
        // Human is still typing (0ms ago) but the hold hit the 90s cap:
        // deliver anyway so reports aren't starved forever.
        assert!(!should_hold_for_user(10_000, 10_000, CAP, WINDOW, CAP));
        // One tick over the cap also delivers.
        assert!(!should_hold_for_user(10_000, 10_000, CAP + Duration::from_millis(1), WINDOW, CAP));
    }

    #[test]
    fn boundary_at_exactly_the_window_proceeds() {
        // `since == window` is not "< window", so it proceeds (quiet enough).
        assert!(!should_hold_for_user(6_000, 10_000, Duration::from_secs(1), WINDOW, CAP));
    }

    #[test]
    fn future_timestamp_does_not_underflow() {
        // A clock skew where last_input is "after" now must not panic or wrap;
        // saturating_sub yields 0 → within window → hold.
        assert!(should_hold_for_user(11_000, 10_000, Duration::from_secs(1), WINDOW, CAP));
    }
}

#[cfg(test)]
mod max_notice_tests {
    use super::*;

    const DEB: Duration = Duration::from_secs(3);

    #[test]
    fn burst_coalesces_to_one_span() {
        // Three rapid clicks 4→3, 3→2, 2→1 inside the window: one pending entry
        // spanning the whole burst, its deadline riding the LAST click.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "g", 4, 3, 1_000, DEB);
        record_max_notice(&mut p, "g", 3, 2, 1_500, DEB);
        record_max_notice(&mut p, "g", 2, 1, 2_000, DEB);
        assert_eq!(p.len(), 1, "a burst stays one pending notice");
        // Not yet due (last click at 2_000 → due 5_000): nothing flushes.
        assert!(take_due_max_notices(&mut p, 4_999).is_empty());
        // Past the window: exactly one notice, from the burst's first value to
        // its last — 4→1, never the intermediate 4→3 / 3→2.
        assert_eq!(take_due_max_notices(&mut p, 5_000), vec![("g".to_string(), 4, 1)]);
        assert!(p.is_empty(), "delivered notices are drained");
    }

    #[test]
    fn each_click_pushes_the_deadline_out() {
        // A click landing before the prior one's window elapses must reset the
        // deadline, or a long slow drag would fire mid-burst.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "g", 4, 3, 1_000, DEB); // due 4_000
        record_max_notice(&mut p, "g", 3, 2, 3_900, DEB); // due 6_900
        // At 4_000 the first click's deadline has passed, but the second reset
        // it — so nothing is due yet.
        assert!(take_due_max_notices(&mut p, 4_000).is_empty());
        assert_eq!(take_due_max_notices(&mut p, 6_900), vec![("g".to_string(), 4, 2)]);
    }

    #[test]
    fn spaced_changes_deliver_separately() {
        // Two changes far enough apart that the first flushes before the second
        // arrives: two distinct notices, each its own span.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "g", 4, 3, 1_000, DEB);
        assert_eq!(take_due_max_notices(&mut p, 4_000), vec![("g".to_string(), 4, 3)]);
        record_max_notice(&mut p, "g", 3, 2, 10_000, DEB);
        assert_eq!(take_due_max_notices(&mut p, 13_000), vec![("g".to_string(), 3, 2)]);
    }

    #[test]
    fn net_noop_burst_delivers_nothing() {
        // 4→3→4 nets to no change: no orchestrator tokens spent on a no-op.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "g", 4, 3, 1_000, DEB);
        record_max_notice(&mut p, "g", 3, 4, 1_500, DEB);
        assert!(take_due_max_notices(&mut p, 5_000).is_empty());
        assert!(p.is_empty(), "the netted-out entry is still drained, not left pending");
    }

    #[test]
    fn groups_debounce_independently() {
        // Two groups clicking at once don't share a deadline or a span.
        let mut p = HashMap::new();
        record_max_notice(&mut p, "a", 4, 2, 1_000, DEB); // due 4_000
        record_max_notice(&mut p, "b", 5, 6, 3_000, DEB); // due 6_000
        // Only group a is due at 4_000.
        assert_eq!(take_due_max_notices(&mut p, 4_000), vec![("a".to_string(), 4, 2)]);
        assert!(p.contains_key("b"), "b keeps waiting out its own window");
        assert_eq!(take_due_max_notices(&mut p, 6_000), vec![("b".to_string(), 5, 6)]);
    }
}
