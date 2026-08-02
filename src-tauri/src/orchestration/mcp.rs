//! Minimal MCP server (Streamable HTTP transport, JSON responses) for
//! orchestration groups.
//!
//! Hand-rolled JSON-RPC-over-POST instead of an SDK: every tool here is a
//! quick request/response (no server→client streaming), so the whole
//! protocol surface is `initialize`, `ping`, `tools/list`, and `tools/call`.
//! Identity comes from the `X-Loomux-Agent` token header written into each
//! agent's `--mcp-config` file; the token maps to (group, agent, role) and
//! every tool is scoped to the caller's group — panes without a token can't
//! reach this server's state at all, and group A can never see group B.

use super::report;
use super::{Caller, Delivery, NameSource, OrchRegistry, Role};
use serde_json::{json, Value};
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

const MAX_BODY: usize = 1024 * 1024;

/// Bind on an ephemeral localhost port, record it in the registry, and serve
/// forever (one thread per request; tool calls that wait on pane binds can
/// block their thread without stalling other agents).
pub fn serve(reg: Arc<OrchRegistry>) {
    let server = match tiny_http::Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("loomux: MCP server failed to bind: {e}");
            return;
        }
    };
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    reg.set_port(port);
    loop {
        let req = match server.recv() {
            Ok(r) => r,
            Err(_) => break,
        };
        let reg = reg.clone();
        std::thread::spawn(move || handle(reg, req));
    }
}

fn respond(req: tiny_http::Request, code: u16, body: String) {
    let ct = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static header");
    let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(code).with_header(ct));
}

fn rpc_error(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn handle(reg: Arc<OrchRegistry>, mut req: tiny_http::Request) {
    if !req.url().starts_with("/mcp") {
        respond(req, 404, json!({ "error": "not found" }).to_string());
        return;
    }
    if req.method() != &tiny_http::Method::Post {
        // Streamable HTTP allows GET for server-initiated streams; we have none.
        respond(req, 405, json!({ "error": "POST only" }).to_string());
        return;
    }

    let token = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Loomux-Agent"))
        .map(|h| h.value.as_str().to_string());

    let mut body = String::new();
    if req.as_reader().take(MAX_BODY as u64 + 1).read_to_string(&mut body).is_err()
        || body.len() > MAX_BODY
    {
        respond(req, 400, json!({ "error": "bad body" }).to_string());
        return;
    }
    let msg: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            respond(req, 400, rpc_error(&Value::Null, -32700, "parse error"));
            return;
        }
    };

    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();

    // Notifications (no id) need no body — ack and move on.
    if msg.get("id").is_none() {
        respond(req, 202, String::new());
        return;
    }

    let caller = match token.as_deref().and_then(|t| reg.resolve_token(t)) {
        Some(c) => c,
        None => {
            // Breadcrumb the rejection (method + whether a token was present),
            // never the token value or body.
            crate::obs::breadcrumb(
                "mcp-auth-fail",
                &format!("method={method} token_present={}", token.is_some()),
            );
            respond(req, 200, rpc_error(&id, -32000,
                "unknown or missing X-Loomux-Agent token — this MCP server only serves loomux-managed agents"));
            return;
        }
    };

    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match dispatch(&reg, &caller, &method, &params) {
        Ok(result) => respond(req, 200,
            json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()),
        Err((code, m)) => respond(req, 200, rpc_error(&id, code, &m)),
    }
}

/// Protocol dispatch, separated from HTTP so tests can drive it directly.
pub fn dispatch(
    reg: &OrchRegistry,
    caller: &Caller,
    method: &str,
    params: &Value,
) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            Ok(json!({
                "protocolVersion": requested,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "loomux-orchestration", "version": env!("CARGO_PKG_VERSION") },
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_defs(caller.role, caller.role_hint.as_deref()) })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            reg.audit(&caller.group, &caller.agent_id, "tool-call",
                json!({ "tool": name, "args": args }));
            // #535: the shared agent-acknowledgment clock. Stamped HERE — the
            // one funnel every `tools/call` passes through — so it covers
            // every tool and every role, with no per-tool opt-in to forget the
            // way `note_agent_activity` (one tool, orchestrator excluded) has.
            // Before `call_tool`, deliberately: the claim is "this agent's own
            // process is alive and executing", which a rejected call proves
            // just as well as an accepted one. See `note_agent_ack`.
            reg.note_agent_ack(&caller.agent_id);
            let out = call_tool(reg, caller, name, &args);
            let (text, is_error) = match out {
                Ok(t) => (t, false),
                Err(t) => (t, true),
            };
            if is_error {
                // Failure only, and only the tool name + caller — no args/output.
                crate::obs::breadcrumb(
                    "mcp-tool-fail",
                    &format!("group={} agent={} tool={name}", caller.group, caller.agent_id),
                );
            }
            reg.audit(&caller.group, &caller.agent_id, "tool-result", json!({
                "tool": name, "ok": !is_error,
                "text": text.chars().take(500).collect::<String>(),
            }));
            let mut content = vec![json!({ "type": "text", "text": text })];
            // #578: an orchestrator pane has no in-band channel for its own
            // queue notices — a delivery announcing that pane's blocked
            // delivery would queue behind the block it reports, which is why
            // `notify_queue` suppresses it outright. This is the channel that
            // is not a delivery: notices parked for this group ride back on a
            // call the orchestrator itself made. Attached HERE, at the one
            // funnel every `tools/call` passes through, for the same reason
            // `note_agent_ack` is stamped above — no per-tool opt-in to
            // forget.
            //
            // A SECOND content block, never appended to the first: several
            // tools return JSON their caller parses (`queue_orphans`,
            // `get_state`, `group_usage`), and a notice glued onto that string
            // would corrupt it. Attached on an `isError` result too — the
            // orchestrator is demonstrably alive and reading either way, and
            // dropping the relay because its unrelated call failed would put
            // the notice back in the hole this exists to fill.
            if caller.role == Role::Orchestrator {
                if let Some(relay) = reg.take_orchestrator_notices(&caller.group) {
                    content.push(json!({ "type": "text", "text": relay }));
                }
            }
            Ok(json!({ "content": content, "isError": is_error }))
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required },
    })
}

/// `channel_send`/`channel_status` tool defs, shared by the standard tier
/// (below) and `Role::Solo`'s standalone two-tool surface — one definition,
/// so the two listings can never drift apart.
fn channel_tool_defs() -> [Value; 2] {
    [
        tool("channel_send",
            "Send a message to everyone you're currently connected to in your cross-workspace channel (a human connects panes together; you cannot). It is typed as a visible prompt into each peer's pane, prefixed with your identity so they know it's from you. Directional: the channel's SENDER may call this any time and it broadcasts to every receiver; a RECEIVER may call this only after the sender has messaged it (a one-time reply credit) and it goes to the sender ONLY, never another receiver. Errors if you aren't connected to anyone, or (as a receiver) haven't been messaged yet — check with channel_status().",
            json!({ "text": { "type": "string", "description": "The message to send. Sanitized before delivery: control characters are stripped and you cannot forge a [loomux] system notice." } }),
            &["text"]),
        tool("channel_status",
            "Check whether you're connected to a cross-workspace channel: the sender's agent id, who else is in it (agent id, role, name, repo, direction, whether each can currently talk back), and whether YOU can currently channel_send (always true if you're the sender; true for a receiver only while it holds the reply credit). Read-only.",
            json!({}), &[]),
    ]
}

/// The tool surface is role-filtered so workers never even see privileged
/// tools; `call_tool` re-checks anyway (listing is cosmetic, not security).
/// `role_hint` additionally scopes `session_digest` to `process`-hinted
/// worker blocks (#250/#324 slice D) — every other tool ignores it.
fn tool_defs(role: Role, role_hint: Option<&str>) -> Vec<Value> {
    // A standalone pane's ENTIRE surface, full stop (#271 W3 addendum, part
    // A1): a solo token must confer zero group-scoped power. Returned here,
    // before any of the tiers below, so no future addition to the shared or
    // orchestrator/delegate tiers can ever silently leak onto it.
    if role == Role::Solo {
        return channel_tool_defs().to_vec();
    }
    let mut tools = vec![
        tool("list_agents", "List the agents in your orchestration group with role, status, and task.",
            json!({}), &[]),
        tool("get_state", "Read the group's durable orchestration state (JSON string). Survives sessions.",
            json!({}), &[]),
        tool("list_tasks",
            "Read the group's task board (JSON array, order = priority) as COMPACT rows: id, title, status, issue, pr, pr_base, assignee, session, updated_ms, note_count — NO note text. The human sees and edits the full board (with notes) beside your pane. Use note_count to tell whether a task has history worth pulling, then call get_task(id) for that task's full notes. Rows also carry the task's links and a derived `ready`: `deps` (ids this task is BLOCKED ON — ids only) and `related` (non-blocking see-also), plus `ready: true` when the task is `queued` AND every one of its deps is `done`. `ready` is what makes this call the answer to \"what is startable right now\" — top-of-board first among the ready rows — instead of re-deriving the order from prose after a compact. Both link arrays are omitted from a row that has none. Nothing here auto-flips a status: a queued task with unmet deps simply reads `ready: false`, and because every row's own status is in the same response, WHICH dep is holding it is directly readable — no second call needed.",
            json!({}), &[]),
        tool("get_task",
            "Read ONE task's full record, including its note history (capped: only the newest notes are kept verbatim, older ones collapse into one placeholder — the full text of every note is always in this group's audit log regardless). Use this after list_tasks's compact row shows a note_count worth reading.",
            json!({ "id": { "type": "string", "description": "Task id, e.g. t-3" } }),
            &["id"]),
        tool("list_verdicts",
            "Read the recorded review verdicts for a PR: which reviewer block recorded what (pass | fail | escalate), when, and its summary — plus, when this repo's .loomux/workflow.yml declares a merge gate, whether that gate is satisfied. This is STATE, not a notification: it is what the loomux gh interceptor reads when it decides whether to allow `gh pr merge`. Each verdict also carries `body_changed` when loomux can tell whether the PR body moved since it was recorded (absent = it cannot tell): on a `pass` that means the text a squash merge would commit is not what was approved — send the reviewer back; on a `fail`/`escalate` it means the body was edited afterwards, so check whether the finding is already fixed before routing it to a worker. Omit pr to list every PR with a recorded verdict.",
            json!({
                "pr": { "type": "string", "description": "PR number, #n, or URL. Omit to list all PRs with verdicts." },
            }),
            &[]),
        tool("request_compact",
            "Call this as the LAST action of a turn, at a natural lull (a feature just merged, before pulling new work, going idle on an external wait) — never mid-task. It does NOT compact you right now: it flags THIS pane so loomux pastes /compact for you the moment you go idle at your input prompt, same as it would on its own timer, just sooner and on your judgment instead of a heuristic. Self-scoped: it can only ever affect the pane that calls it. Supported on Claude Code and Copilot CLI (both have a /compact command) — errors clearly on any other CLI rather than typing a command it won't understand. Before calling this, offload everything you'll need after the summary: reconcile the task board, call set_state with anything mid-decision, and push plan/progress context living only in this conversation to the relevant GitHub issues/PRs — the post-compact re-sync (list_tasks + get_state + list_agents, plus a mandatory re-grounding in your role instructions) restores only what was made durable first.",
            json!({}), &[]),
        tool("note_directive",
            "Record a one-line diary entry in YOUR OWN directive ledger — call this BEFORE acting whenever the human gives you a directive, a scope decision, or feedback. Self-scoped, like request_compact: it can only ever touch the pane that calls it. The point is timing: the CLI's own emergency auto-compact can strike with no warning turn, so this is a diary kept at the moment you RECEIVE something, not a summary you write later from memory once the risk has already passed. Your ledger is embedded verbatim (tail, size-capped) in the mandatory post-compact re-grounding notice, so a directive survives a compact you never saw coming. Pass replace: true to rewrite the WHOLE ledger instead of appending one line — use this to curate right after a compact re-grounds you in your own ledger tail: drop entries that are done or no longer relevant so it stays a living record instead of an ever-growing dump.",
            json!({
                "text": { "type": "string", "description": "The directive/decision/feedback to record (append mode), or the full curated ledger text (replace mode)" },
                "replace": { "type": "boolean", "description": "true = rewrite the whole ledger with text; default false = append text as one new entry" },
            }),
            &["text"]),
    ];
    // Notification backend (#243): self-addressed — there is no `agent_id`
    // parameter, and a notice can only ever land in the caller's own pane, so
    // this belongs in the shared tier, not the orchestrator-only one. Denied
    // to a planner: its pane closes the instant it reports `done` (#203), and
    // a watch that outlives its owner is garbage. `call_tool` re-checks this
    // (`require_not_planner`) — this filter is cosmetic, not the gate.
    if role != Role::Planner {
        tools.extend([
            tool("notify_when",
                "Register a background watch on a CI/run condition and get a [loomux] notice IN THIS PANE the moment it fires — never another agent's. Register and immediately go do other work; do not sleep or re-poll `gh pr checks`/`gh run view` yourself, loomux polls every 30s. kind: \"pr_checks\" (a PR's checks reach SUCCESS/FAILURE — pass pr; if the PR goes CONFLICTING, it resolves immediately with that notice instead — GitHub never creates check-suites for a conflicted PR, so waiting for SUCCESS/FAILURE there would hang until expiry) or \"workflow_run\" (a specific `gh run` id completes — pass run). expires_minutes defaults to 60, clamped to 5-240. Capped at 4 live per agent / 12 per group; cancel one with cancel_notification or let it fire/expire to free a slot.",
                json!({
                    "kind": { "type": "string", "enum": ["pr_checks", "workflow_run"], "description": "Unrecognized values are rejected, never defaulted" },
                    "pr": { "type": "string", "description": "PR number, #n, or URL — required for pr_checks" },
                    "run": { "type": "string", "description": "gh run id (number or run URL) — required for workflow_run" },
                    "note": { "type": "string", "description": "Echoed back in the notice so you remember what to do when it fires, e.g. \"merge if green, else route back to w-2\"" },
                    "expires_minutes": { "type": "integer", "description": "default 60, clamped to 5-240" },
                }),
                &["kind"]),
            tool("list_notifications",
                "List your OWN live notifications (id, kind, target, note, registered/expiry times), read fresh from the live registry. A loomux restart empties the registry, so a watch is gone and must be re-registered from scratch; a /compact only drops YOUR memory of it — the watch is still live, and this call recovers what it was. Call it on session start and after a /compact, and re-register anything a restart actually lost.",
                json!({}), &[]),
            tool("cancel_notification",
                "Cancel one of your own live notifications by id (e.g. because the PR it watched got closed).",
                json!({ "id": { "type": "string" } }), &["id"]),
            // Cross-workspace channels (#271): a HUMAN connects your pane to
            // another agent's pane (possibly in a different orchestration
            // group/repo, or a standalone launcher pane) via a context-menu
            // gesture — there is no tool here to open, close, or join a
            // channel yourself. Once connected, these two tools are your
            // whole surface. Denied to a planner for the same #203 reason as
            // the notification tools — `call_tool`'s `require_not_planner`
            // re-checks this listing. Shared with `Role::Solo`'s standalone
            // surface (`channel_tool_defs`) so the two listings never drift.
        ]);
        tools.extend(channel_tool_defs());
    }
    if role == Role::Orchestrator {
        tools.extend([
            tool("spawn_agent",
                "Open a new worker, reviewer, or planner agent pane in this group. A FRESH SPAWN MUST NAME ITS CAPABILITY CLASS: pass kind (worker | reviewer | planner) or block (a block id from this group's roster). Omitting both is REFUSED — there is no default class (#544). Before that refusal existed, forgetting `kind` handed you the MOST-privileged class: three reviewer-shaped briefs (\"review PR #536\", \"record your verdict\") were spawned as read-write worker panes, with edit tools and git commit/push, and nothing objected. A capability class is only ever acquired deliberately; the only spawn that may omit both is a resume_session, which INHERITS the resumed session's own block (see below) rather than defaulting to anything. Guardrails apply: live-agent cap and per-role pinned CLI + model. Give branch a meaningful name. Empty task spawns an idle agent awaiting prompts. A planner explores the codebase read-only and writes an implementation plan as a GitHub issue comment, then reports and exits. Its read-only contract is enforced structurally where the CLI allows it — it never gets a worktree, and its file-editing tools plus git commit/push are denied at the CLI level — so it cannot edit files or push code; not opening PRs is asked of it in its instructions (gh stays available so it can post the plan comment). WORKTREE DEFAULTS ON FOR WORKERS AND REVIEWERS AND CANNOT BE TURNED OFF (#338/#359): the main clone is the human's environment, and neither a worker (branching/committing there) nor a reviewer (contending on its checkout state with another reviewer or your own fetch/merge traffic — two concurrent reviewers colliding in the shared clone is the incident #359 names) may conflict with it — passing worktree=false for either (or a worker-/reviewer-kind block) is rejected outright, not silently coerced. A reviewer's own worktree is scratch space cut from the default branch, not a checkout of the PR it's reviewing (that branch may already be checked out in the worker's own worktree) — its kickoff note and reviewer.md cover the `gh pr checkout <n> --detach` convention for inspecting the PR's actual code locally. A planner is unaffected: it never gets one under any circumstance. For your OWN mechanical work (rebases, conflict fixes) that would otherwise mean checking out a branch in the main clone, use a staging worktree of your own instead of spawning a worker or reviewer just to get one. THE SAME GUARANTEE COVERS A FRESH SPAWN'S cwd, not just worktree: passing cwd on a worker or reviewer spawn with no resume_session is rejected too (it would override the worktree exactly like worktree=false would) — cwd only has a role once resume_session is set; a planner still honors an explicit cwd on a fresh spawn, unchanged. For a FOLLOW-UP on a finished task, pass resume_session (from list_agents/the task board) plus cwd (where that work happened) — the pane reopens that conversation with its context instead of cold-starting, and the worktree default/guard above does not apply (the resume's cwd is what governs its workspace). cwd is optional on a resume: omit it and loomux INHERITS the session's recorded workspace from this group's roster (the same last-touched-record lookup the block inheritance below uses) rather than guessing — but if nothing is recorded for that session AND the resumed agent is a worker or reviewer, the spawn is a hard error rather than a silent fall-back into the main clone (#338/#359 again: neither's workspace is ever the human's own checkout). A planner is unaffected by that guard; pass cwd explicitly whenever you have it, which you almost always will. A resume with no kind/block INHERITS the resumed session's original block (and therefore its persona, model and capability class) from this group's roster — it never re-derives a default from `kind`, so a reviewer resumed bare comes back a reviewer, not a worker. An unrecognized session id with no block is a hard error, never a silent worker spawn. To deliberately re-role a resumed session into a different capability class, pass `block` explicitly — same as any other spawn, and audited the same way (the agent-spawn record always carries block + session + resume).",
                json!({
                    "name": { "type": "string", "description": "Short display name for the pane" },
                    "kind": { "type": "string", "enum": ["worker", "reviewer", "planner"], "description": "Capability class. REQUIRED on a fresh spawn unless `block` names one instead (#544) — there is NO default, so omitting both is refused with a message naming what to pass, never silently a worker. An unrecognized value is rejected too, never treated as a worker. On a resume_session, passing this ALSO defeats block inheritance — same as passing block — and re-derives the default block for that kind instead; omit both there to inherit the resumed session's own block." },
                    "block": { "type": "string", "description": "Id of a block declared in the repo's .loomux/workflow.yml — e.g. 'rev-security'. The block supplies the persona, CLI, model and capability class (so `kind` is ignored when this is set). Your kickoff lists the blocks this group has; omit it to get the default block for `kind`, which then has to be set — a fresh spawn naming NEITHER is refused (#544). UNLESS resume_session is set, in which case omitting both inherits that session's own original block instead (see resume_session). Set it explicitly on a resume only when you mean to re-role that conversation into a different capability class." },
                    "task": { "type": "string", "description": "Full task brief; empty = idle. With resume_session, this is the follow-up prompt." },
                    "worktree": { "type": "boolean", "description": "Create a dedicated git worktree + branch. Defaults ON for workers AND reviewers (and cannot be set false for either — rejected, see above); a planner never gets one regardless of this flag." },
                    "branch": { "type": "string", "description": "Branch name (default agent/<id>)" },
                    "base": { "type": "string", "description": "Start-point for the worktree branch (default: the repo's default branch, fetched fresh from origin). Pass a feature branch (e.g. 'feat/x' or 'origin/feat/x') to deliberately stack this worktree on top of it. Ignored without worktree=true. When 'branch' already exists, that branch is checked out as-is (its history stands on its own) — but if it does NOT descend from the requested base, the spawn fails loudly (#227) rather than silently handing back a wrong-base worktree." },
                    "resume_session": { "type": "string", "description": "Session id to resume instead of starting fresh. A truncated id resolves if it's an unambiguous prefix of exactly one session in THIS group's roster; ambiguous or unknown prefixes fail with the matching candidates (never picked silently)." },
                    "cwd": { "type": "string", "description": "Existing directory to run in — the original workspace, with resume_session. Optional there: omitted, it's inherited from the session's recorded roster entry; a worker or reviewer with nothing recorded and no cwd given is rejected rather than defaulting to the main clone (#338/#359). On a FRESH spawn (no resume_session), it is REJECTED for a worker or reviewer (or a worker-/reviewer-kind block) — it would override the worktree the same way worktree=false would, so let loomux cut the worktree instead; a fresh planner spawn still honors it as a raw override, unchanged." },
                }),
                &["task"]),
            tool("send_prompt",
                "Type a prompt into an agent's CLI. The human sees it verbatim in that pane.",
                json!({
                    "agent_id": { "type": "string" },
                    "text": { "type": "string" },
                }),
                &["agent_id", "text"]),
            tool("get_output", "Read the last N lines of an agent's terminal AS RENDERED — the pane's output is replayed onto a composed screen, so a TUI's in-place redraws (spinner frames, cycling status verbs, a repainted input box) overwrite each other exactly as they do on a human's screen instead of piling up as text. N means N distinct content lines. Hard-capped at 8KB per call whatever N you ask for; if it binds, the reply says so and how many bytes were dropped.",
                json!({
                    "agent_id": { "type": "string" },
                    "lines": { "type": "integer", "description": "default 60, max 500" },
                }),
                &["agent_id"]),
            tool("kill_agent", "Terminate an agent and close its pane.",
                json!({ "agent_id": { "type": "string" } }), &["agent_id"]),
            tool("focus_agent", "Bring an agent's pane into focus for the human.",
                json!({ "agent_id": { "type": "string" } }), &["agent_id"]),
            tool("rename_agent",
                "Rename an agent's pane title (and roster entry) to reflect the work it is doing — e.g. rename w-2 to \"w-2: gitwatch fix\" when you assign it that task. Keep it short. A human who later renames the pane themselves takes precedence: your rename will not override theirs.",
                json!({
                    "agent_id": { "type": "string" },
                    "name": { "type": "string", "description": "New short display name for the pane" },
                }),
                &["agent_id", "name"]),
            tool("set_state",
                "Persist the group's orchestration state (must be a valid JSON string). Call after every queue/plan change; this is your memory across sessions.",
                json!({ "state": { "type": "string" } }), &["state"]),
            tool("upsert_task",
                "Create (omit id, title required) or update a task on the shared board. status: queued | in-progress | review | pr | prototype | human-testing | done | blocked. Use `prototype` for a demo-gated draft the human will decide whether to promote — the board shows them a Proceed button, and clicking it prompts you to run the full production build. Keep the board current — it is the human's window into your queue. Record `pr_base` (the branch the PR targets) in the SAME call you record `pr`: the board reads it to tell a merge into the default branch from a sub-PR into an integration branch, and without it the human is shown the conservative default-branch warning either way. note appends a timestamped note. `deps`/`related` record ORDERING STRUCTURE that would otherwise live only in your context and your set_state prose: set `deps` whenever a plan implies one task must finish before another, and read it back as `ready` on list_tasks instead of re-deriving the queue after a compact. Both arrays REPLACE (they are not appends): omit one to leave it untouched, pass [] to clear it. Every id must name a live task on this board — an unknown id, a self-link, or a dep edge that would close a CYCLE is rejected outright (the error names the cycle path), and deleting a task strips its id from every other task's links in the same write. Only `done` satisfies a dep; `related` never blocks anything. `claim: true` (id required) is how you assign work: it refuses unless the task is still `queued`, is unassigned or already assigned to this same agent, and has every dep `done` — then sets assignee + status:in-progress in ONE guarded write, so a re-read after a compact can never hand the same task to a second worker. Re-claiming a task the same agent already holds is an idempotent no-op, so \"did my claim land before the compact?\" is safe to just ask again. A refused claim is the board telling you the task is taken or blocked; read the error, don't retry it as a plain assignee write.",
                json!({
                    "id": { "type": "string", "description": "Existing task id; omit to create" },
                    "title": { "type": "string" },
                    "status": { "type": "string", "enum": ["queued", "in-progress", "review", "pr", "prototype", "human-testing", "done", "blocked"] },
                    "issue": { "type": "string", "description": "GitHub issue ref, e.g. #12" },
                    "pr": { "type": "string", "description": "PR ref or URL" },
                    "pr_base": { "type": "string", "description": "Branch the PR targets, as gh reports it (`gh pr view --json baseRefName`): `main`, `integration/581`, … Record it whenever you record `pr` — it is what lets the human's board say \"sub-PR into integration/581\" instead of warning about the default-branch merge gate on a PR that isn't one. DISPLAY METADATA ONLY: nothing gates on it, loomux re-resolves the real base ref live for every merge decision, so a wrong value here misleads a human rather than opening a merge." },
                    "assignee": { "type": "string", "description": "Agent id working on it" },
                    "session": { "type": "string", "description": "Worker session id for this task (enables follow-up resume)" },
                    "note": { "type": "string", "description": "Note to append" },
                    "deps": { "type": "array", "items": { "type": "string" }, "description": "Task ids this task is BLOCKED ON, e.g. [\"t-3\",\"t-5\"]. Replaces the whole array; omit = untouched, [] = clear. Must name live tasks on this board; cycles are rejected." },
                    "related": { "type": "array", "items": { "type": "string" }, "description": "Non-blocking see-also task ids. Same replace/untouched/clear rule as deps; never affects readiness." },
                    "claim": { "type": "boolean", "description": "Atomically claim this task (needs id): guarded on queued + unassigned-or-mine + all deps done, then sets assignee (defaults to you) and status:in-progress in one write. Don't pass a conflicting status with it." },
                }),
                &[]),
            tool("remove_task", "Delete a task from the shared board.",
                json!({ "id": { "type": "string" } }), &["id"]),
            tool("group_usage",
                "Aggregate the group's token usage and estimated dollar cost into one summary, split live vs lifetime (killed/recycled agents still count). Tokens come from each agent's session transcript and are exact; dollars are estimated from a model price table (subscription/Max accounts show $0 in the CLI, so cite tokens). Fold it into your status updates so the human sees spend at a glance.",
                json!({}), &[]),
            tool("queue_orphans",
                "Deliveries nobody ever received, in TWO lists: `orphans` — queued but never delivered when loomux last restarted, and unable to re-bind to a live pane; and `refused` — declined at the front door by loomux, so they were never queued at all. Call it once on session start, with the rest of your re-sync. You no longer have to poll it to learn about refusals to YOUR OWN pane: when that pane's queue drains back below its cap, loomux relays a bounded roster of what it refused while full — sender, preview, reason, and whether the sender has since got it through — on the result of your next tool call (#658). This tool is the whole group's history and the other lists; it is not your only path to your own. Returns {count, orphans:[{id, to, queued_minutes_ago, reason, source, text, text_bytes, truncated}], refused_count, refused_omitted, refused_window_truncated, refused:[{from, to, refused_minutes_ago, reason, queue_depth, enqueue_reason, payload, bytes, preview, text, truncated, consequence}]}, oldest ask first in both. `text` is the payload verbatim (capped at 8KB, with `truncated: true` and the full copy on that delivery's `prompt` line in the audit log) when it came from the durable queue snapshot — `source: \"snapshot\"`. `text` is null in exactly two cases, both meaning \"re-derive this one, don't guess\": `source: \"audit\"` (an entry queued by a loomux build older than the durable snapshot — id and target known, payload not), and `reason: \"stranded-submit-not-replayable\"` (the text had already been typed into that pane and was waiting only for Enter when loomux restarted; the pane is gone, so no bytes remain — the audit log's `prompt` line for that delivery is the only record of what it said). THESE ARE LOST WORK, NOT A LOG: each is something you or an agent sent that nobody ever received, so treat a non-empty result as a to-do list — re-send what still applies (the pane it was for is gone, so re-target it: a resumed session, or a fresh agent), and say what you dropped as stale rather than dropping it silently. An empty result is the normal case and needs no comment. Deliveries that DID re-bind (this group's orchestrator pane, or an agent resumed onto the same session id) were already re-queued automatically in their original order and are not listed here. EACH REFUSAL'S `reason` SAYS WHAT TO DO WITH IT, and they are not interchangeable: `queue-full-at-call` — the target pane was at its 8-deep cap; the pane is alive, so this is the one worth re-sending once it drains (`queue_depth` is how full it was). `agent-dead-at-call` — the target was already dead when this was sent; that pane will NEVER take it, so re-target it at a live or resumed agent or drop it as stale, and do not re-send it as-is. `no-terminal-at-call` — the target existed but had no terminal bound yet (a delivery that arrived during the spawn-to-bind window); it was simply too early, so re-send it now if the agent has since bound. `no-app-handle` / `registry-not-shared` — loomux itself could not process the pane's queue and withdrew the admission; these should never appear in a running build, so treat one as a loomux defect worth reporting to the human, not just as a payload to re-send. `queue_depth` and `enqueue_reason` are null for every reason except `queue-full-at-call`, which is the only one that reached the queue at all — null there means \"no measurement was taken\", not \"the pane was empty\". THE `refused` LIST IS DIFFERENT IN THREE WAYS, and each changes what you do with it. (1) A refusal does not need a restart to happen — a pane at capacity refuses every arrival for as long as it stays there — so this list can be non-empty on a perfectly ordinary session, and `refused_count` counts everything in the readable audit window with only the most recent 8 listed (`refused_omitted` says how many were left in `audit.jsonl`). `refused_window_truncated: true` means that window was ITSELF cut at 5000 entries, so `refused_count` counts only the readable tail and older refusals may exist that this scan never saw — read `audit.jsonl` directly (action `delivery-dropped`, and the `reason` values above) if you need the whole history. When it is false, `refused_count` really is all of them. (2) The SENDER was told synchronously (`delivery queue for … full — NOT queued`), so many of these were already handled by whoever sent them; the ones that matter are those whose sender then died, or where `from` is `loomux` itself and nobody was listening. Check before re-sending, and prefer asking the sender over guessing. (3) `text` is the payload the refusal recorded — carried on the refusal's own audit line for a refusal that never reached the queue, and for a `queue-full-at-call` one recovered from that delivery's `prompt` audit line and verified against the refusal's recorded byte count and preview. Either way, when it is non-null it is re-sendable verbatim; when it is null, `preview` (a bounded one-liner) and `bytes` are what you have — re-derive, do not guess. `payload: \"stranded-submit\"` is the one kind that never had text at all: its bytes were already pasted into that pane and only the Enter was refused, so the pane is sitting with an unsubmitted prompt in its input box (`consequence` says so) — recover it by looking at the pane, not by re-sending. NOTHING IS RE-ADMITTED BY READING THIS: a refused delivery was explicitly declined and stays declined, because slipping it back into a queue now would put it behind — or ahead of — everything the pane has accepted since. Re-sending is your call, deliberately made.",
                json!({}), &[]),
            // The bisecting merge queue (#581 §11.1). Orchestrator-only, and
            // re-checked in `call_tool` — this listing is cosmetic, the dispatch
            // check is the gate. Off unless the repo declares `merge_queue:
            // enabled: true`, in which case every call refuses `queue-disabled`.
            tool("queue_merge",
                "Put an APPROVED sub-PR into this group's speculative merge queue, instead of merging it by hand. The queue exists because a green sub-PR is evidence about a PR and not about a BRANCH: N individually-green PRs can still produce a red integration branch, and when that happens nobody can say which one did it. loomux batches the queued PRs onto a scratch ref, opens a draft PR so the repo's OWN CI judges that exact object, fast-forwards it onto the target on green, and on red bisects and kicks back the one PR that broke the combination — the survivors are re-queued automatically, at the front, so they are not punished for a neighbour's failure. THE COMMIT THAT WAS TESTED IS THE COMMIT THAT LANDS; nothing is rebuilt after CI. You keep merging authority: the queue never touches the default branch (structurally — it cannot construct a refspec for it), never calls `gh pr merge`, and NEVER grants what the merge gate would not. It re-enforces that gate itself, at batch build AND again at the moment of submit, so a reviewer's `fail` or a rebase in between still stops the landing. REFUSALS ARE A CLOSED SET and each says what to do: `queue-disabled` (the repo has no `merge_queue:` block — merge by hand as before), `base-is-default` (that PR targets the default branch; the queue only lands on integration branches), `base-unverifiable` (loomux could not resolve the PR's base or the repo default — unknown is never treated as safe), `base-not-target` (this queue is already landing on a different branch; drain it first, entries already queued were approved against that other branch), `gate-not-configured` (no merge gate covers this target, and the queue will not push approved-by-nobody PRs under its own authority), `gate-not-met` (the reviewers this repo names have not passed the PR's CURRENT head, or its body moved after a pass), `already-queued`, `queue-full`. THREE FURTHER REASONS MEAN LOOMUX ITSELF FAILED, not that the queue declined you, and they are worth reporting to the human rather than working around: `queue-state-unreadable` (the queue is there and loomux cannot read it -- NOT \"nothing is queued\"), `queue-state-unwritable` (the change was computed and could not be saved, so it did not happen), `queue-unavailable` (loomux could not resolve this group at all). None of the three should appear in a running build. Call it once per PR, after its review has passed. Check merge_queue_status() to see where it got to.",
                json!({
                    "pr": { "type": "string", "description": "PR number, #n, or URL — the approved sub-PR to queue." },
                    "target": { "type": "string", "description": "OPTIONAL, and an ASSERTION rather than a choice: if you pass it, it must equal the branch the PR's base actually resolves to, and a mismatch is refused with `base-not-target`. It can narrow what happens, never widen it — you cannot retarget a PR by passing a different branch. Omit it unless you want that assertion checked." },
                }),
                &["pr"]),
            tool("merge_queue_status",
                "Where this group's merge queue stands: {enabled, target, entries:[{pr, state, since_ms, blocked_reason?}], batch?}. `target` is the branch the queue is landing on — established by the first successful queue_merge from that PR's live base, and RELEASED when the queue drains, so it is a property of the work in the queue rather than a setting. Entry states are queued | batching | ci-wait | landing | bisecting; terminal entries (landed, kicked-back, cancelled) are not listed. `blocked_reason` on a `queued` entry means it is not batchable RIGHT NOW — almost always because the PR was rebased, which kills its verdicts until a re-review covers the new head; it clears by itself, so re-review rather than re-queue. `since_ms` is an AGE, not a timestamp. `batch` appears only while one is in flight and names the draft PR whose checks are being watched — that PR is loomux's, so do not merge or close it by hand. Read-only: calling this never changes anything.",
                json!({}), &[]),
            tool("cancel_queued_merge",
                "Take a PR back out of the merge queue. Works on any entry that has not reached a terminal state — including one inside a batch that is currently in flight, in which case that batch is abandoned and rebuilt without it (nothing lands, and loomux cleans up its scratch ref and draft PR). Refuses `not-queued` if the PR is not in the queue or has already landed, been kicked back, or been cancelled — a landing that already happened cannot be called back, and you are told so rather than being given a success that means nothing. `queue-state-unreadable` and `queue-state-unwritable` are DIFFERENT and mean loomux itself failed — the first says loomux cannot read the queue at all (so it cannot tell whether your PR is in it, which is not the same as saying it isn't), the second says the cancel was computed and could not be saved (so it did not happen). Neither should appear in a running build; report one rather than working around it. Cancel when the PR needs more work; a kicked-back PR that gets fixed comes back through a fresh queue_merge as a NEW entry, so its refusals are all re-checked against the world as it is then.",
                json!({ "pr": { "type": "string", "description": "PR number, #n, or URL — the queued PR to cancel." } }),
                &["pr"]),
        ]);
    } else {
        tools.extend([
            tool("report",
                "Report to the orchestrator — decision-grade, not a narrative: it is a router whose next action depends on one bit plus a reference, and every paragraph beyond that is context it pays for on every future turn. Post your FULL detail to GitHub first (PR body/comment, issue comment — the system of record); this tool is the notification, not the record. Prefer the structured shape: `outcome` (done | blocked | approved | request_changes | progress — approved/request_changes are for a reviewer's report after `review_verdict`, and both count as this agent's turn being over, same as done), `ref` (the PR/issue this is about, e.g. \"#123\"), `detail_url` (the GitHub comment/PR where the full detail lives), and `note` — a short pointer (~1-2 lines), hard-capped at 500 characters and truncated WITH a stated marker if you go over, so the cap is enforced, not merely asked for. The legacy shape (`status` + free-text `summary`, no cap) still works — nothing breaks — but is soft-deprecated: write new reports the structured way. Give exactly one of `status`/`outcome` and one of `summary`/`note`.",
                json!({
                    "status": { "type": "string", "enum": ["progress", "done", "blocked"], "description": "Legacy — soft-deprecated. Prefer `outcome`." },
                    "summary": { "type": "string", "description": "Legacy free text, uncapped — soft-deprecated. Prefer `note`." },
                    "outcome": { "type": "string", "enum": ["done", "blocked", "approved", "request_changes", "progress"], "description": "Structured decision-grade outcome. approved/request_changes are a reviewer's report after review_verdict." },
                    "ref": { "type": "string", "description": "The PR/issue this report is about, e.g. \"#123\"." },
                    "detail_url": { "type": "string", "description": "URL of the GitHub PR/issue comment carrying the full detail — the system of record." },
                    "note": { "type": "string", "description": "Short pointer, hard-capped at ~500 chars (truncated with a stated marker if longer)." },
                }),
                &[]),
            tool("message_orchestrator", "Send a free-form message to the orchestrator.",
                json!({ "text": { "type": "string" } }), &["text"]),
        ]);
    }
    // Reviewers only: the verdict is the gate. Listed for the capability class, and
    // re-checked in `call_tool` — the listing is cosmetic, the dispatch check is the
    // enforcement (a worker that could file its own PASS would make the gate a prop).
    if role == Role::Reviewer {
        tools.push(tool("review_verdict",
            "Record your REVIEW OUTCOME for a pull request. This is durable, attributed state — not a notification — and when this repo's .loomux/workflow.yml declares a merge gate, it is what loomux's gh interceptor reads before allowing `gh pr merge`. Call it once you have finished reviewing, after posting your review on the PR, and then report() to the orchestrator as usual. verdict: `pass` (reviewed, nothing blocking), `fail` (blocking findings — fix and re-review), `escalate` (you will not decide this one: ambiguous requirement, out of your depth, a risk you won't sign off on — a human must look). fail and escalate BOTH refuse the merge, and one blocking verdict beats any number of passes, so never record `pass` to be agreeable or to unblock the queue. Your verdict is bound to the PR's CURRENT HEAD COMMIT: if the author pushes anything afterwards, your pass goes STALE and the gate reopens until you review the new commits and record again — so review the head as it stands, and expect to be asked again after a fix. Re-recording replaces your own earlier verdict (that is how you upgrade a `fail` to a `pass`, and how you refresh a stale one). loomux ALSO records a digest of the PR body as it stands when you call this — you never pass it and cannot forget it — because on a squash-merging repo that body becomes the permanent commit message: it is reviewed content, so review it, and expect to be asked again if it is edited after you pass. The summary must stand on its own for a human reading it a week later: what you reviewed, and what decided the verdict. Verdict words are lowercase.",
            json!({
                "pr": { "type": "string", "description": "PR number, #n, or URL — the PR you reviewed." },
                "verdict": { "type": "string", "enum": ["pass", "fail", "escalate"], "description": "pass | fail | escalate, lowercase. Never guessed: an unrecognized value is rejected." },
                "summary": { "type": "string", "description": "Why. One or two lines a human can act on." },
            }),
            &["pr", "verdict", "summary"]));
    }
    // `process`-hinted worker blocks only (#250/#324 slice D binding rider):
    // slice B shipped this gated to worker-kind generally, since `role_hint`
    // (slice A) was still landing in parallel; now that it's on this branch,
    // the tool is scoped to its actual owner, the process-pro. See the
    // matching note on the `call_tool` arm, which is the real (re-checked)
    // gate — this listing is cosmetic.
    if role == Role::Worker && role_hint == Some("process") {
        tools.push(tool("session_digest",
            "Read a FINISHED session's transcript, reduced to friction windows: the wall, the attempts, and the fix — never the raw transcript. Deterministic, no LLM in this step; each window names its signature (tool_error | near_duplicate_command | test_red_to_green | reverted_edit) plus the event range and a short summary. Also returns three anchors: initial_prompt (what the worker was asked), final_diff_ref (its PR/branch, if known), outcome (its task status, if known). windows is capped (oldest dropped first) — check dropped_windows to see if any were cut. Pass exactly ONE of task, agent, or pr to identify the session — the agent need not still be alive; this reads its recorded transcript cold. Use this instead of resuming or re-reading a worker's own session: the point is a fresh, cold read of the record, not the worker narrating itself. \
             \
             RECURRENCE — read this before proposing anything. Each window also carries `recurrence`: how many OTHER sessions in this group hit the SAME wall (matched on a normalized key, counted once per session), and `corroborated_by`, up to 5 of their agent ids. `recurrence: 0` means this wall was seen only here — that is a ONE-OFF, and a one-off is not a durable lesson however painful it looked; `recurrence >= 1` means a second session independently hit it, which is the evidence that a fresh worker would hit it too. This number is the answer to \"would a fresh worker on a different task in this repo hit the same wall?\" — use it instead of your own impression of how hard the session looked, which is exactly the self-assessment this cold read exists to avoid. Two counts bound it: `sessions_scanned` (how many other sessions were actually read — 0 means a young group with nothing to compare against, NOT a group of one-offs) and `corroboration_capped` (true = older sessions went unread, so every recurrence is a floor, not a total). \
             \
             Restricted to process-hinted blocks — this is the process-pro's tool, not a general worker one.",
            json!({
                "task": { "type": "string", "description": "Task id, e.g. t-3" },
                "agent": { "type": "string", "description": "Agent id, e.g. w-2" },
                "pr": { "type": "string", "description": "PR number, #n, or URL" },
            }),
            &[]));
    }
    tools
}

fn require_orchestrator(caller: &Caller) -> Result<(), String> {
    if caller.role == Role::Orchestrator {
        Ok(())
    } else {
        Err("permission denied: this tool is orchestrator-only".into())
    }
}

/// The notification tools' gate (#243): denied to a planner. `tool_defs`'s
/// role filter already keeps a planner from *seeing* these tools; this is the
/// real check — the listing is cosmetic, not security (a planner could still
/// try the call name directly).
fn require_not_planner(caller: &Caller) -> Result<(), String> {
    if caller.role == Role::Planner {
        Err("permission denied: planners cannot register notifications — a planner's pane \
             closes the moment it reports done (#203), and a watch that outlives its owner \
             is garbage".into())
    } else {
        Ok(())
    }
}

/// Which capability classes the #338/#359 dedicated-workspace guards apply
/// to: a worker (never touch the main clone by design) and, since #359, a
/// reviewer too (concurrent reviewers, or a reviewer plus the orchestrator's
/// own fetch/merge traffic, contend on the shared clone's checkout state —
/// the incident that named this: rev-36 restoring `main` mid-review under
/// rev-38 in the same clone). A planner is untouched — it never gets a
/// worktree under any circumstance, per its existing read-only contract —
/// and the orchestrator is exempt by construction (`spawn_agent` can never
/// name `kind: "orchestrator"`).
pub(crate) fn needs_dedicated_workspace(role: Role) -> bool {
    matches!(role, Role::Worker | Role::Reviewer)
}

/// Resolve a target agent and enforce that it belongs to the caller's group.
fn require_in_group(reg: &OrchRegistry, caller: &Caller, agent_id: &str) -> Result<super::AgentEntry, String> {
    let a = reg.agent(agent_id).ok_or_else(|| format!("unknown agent: {agent_id}"))?;
    if a.group != caller.group {
        // Same message as unknown: don't leak other groups' agent ids.
        return Err(format!("unknown agent: {agent_id}"));
    }
    Ok(a)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// A string-array argument (#582: `deps`/`related`). Absent or null is `None`
/// — "leave this field untouched" — and an empty array is `Some(vec![])`, the
/// explicit "clear it". Anything that isn't an array of strings is an ERROR,
/// not a silent skip: a caller that passed `"t-3"` (or `[3]`) meant to set a
/// link, and quietly leaving the old array in place would tell it the write
/// succeeded while the board disagreed.
fn arg_str_array(args: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{key} must be an array of task-id strings"))
            })
            .collect::<Result<Vec<String>, String>>()
            .map(Some),
        Some(_) => Err(format!("{key} must be an array of task-id strings")),
    }
}

/// A string argument that must actually BE a string when present (#324,
/// applying #582's rule above to `session_digest`'s three identifiers).
/// Absent or null is `None`; a present-but-wrong-typed value is an error, not
/// a silent `None`.
///
/// `arg_str` cannot make that distinction — it returns `None` for both — and
/// for an identifier argument the two mean opposite things. `session_digest`'s
/// own description invites `"PR number, #n, or URL"`, so `{"pr": 646}` is the
/// natural thing for a caller to send; through `arg_str` that reads as *no
/// identifier at all* and comes back "exactly one of task, agent, or pr is
/// required" — a message that contradicts what the caller plainly did, and
/// sends it looking for a bug in its own call shape rather than its arg type.
fn arg_str_strict<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// A boolean argument (#582: `claim`). Absent or null is `false`; a non-bool
/// is an error rather than a defaulted `false`, so `"claim": "true"` can never
/// read as "no claim requested" and hand the same task out twice.
fn arg_bool(args: &Value, key: &str) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{key} must be true or false")),
    }
}

fn call_tool(reg: &OrchRegistry, caller: &Caller, name: &str, args: &Value) -> Result<String, String> {
    // A standalone pane's token carries zero group-scoped power (#271 W3
    // addendum, part A1/concern 5) — `channel_send`/`channel_status` are its
    // WHOLE surface. Gated ONCE, here, rather than per-arm: an arm added
    // below later without its own role check (several already have none —
    // `list_agents`/`get_state`/`list_tasks`/`list_verdicts` trust
    // `tool_defs`'s listing alone) would otherwise silently grant a solo
    // token access to it. `tool_defs(Role::Solo)`'s two-tool listing is the
    // cosmetic half of this same #243-style double-gate; this is the real one.
    if caller.role == Role::Solo && !matches!(name, "channel_send" | "channel_status") {
        return Err("permission denied: a standalone pane's token can only channel_send / \
                     channel_status — it carries no group-scoped power"
            .into());
    }
    match name {
        "list_agents" => Ok(reg.list_agents(&caller.group).to_string()),
        "get_state" => Ok(reg.get_state(&caller.group)),
        "list_tasks" => Ok(serde_json::to_string(&reg.task_summaries(&caller.group)).unwrap_or_default()),
        "get_task" => {
            let id = arg_str(args, "id").ok_or("id required")?;
            let task = reg.get_task(&caller.group, id).ok_or_else(|| format!("unknown task: {id}"))?;
            Ok(serde_json::to_string(&task).unwrap_or_default())
        }

        "upsert_task" => {
            require_orchestrator(caller)?;
            let claim = arg_bool(args, "claim")?;
            let task = reg.upsert_task(
                &caller.group,
                &caller.agent_id,
                arg_str(args, "id"),
                super::TaskPatch {
                    title: arg_str(args, "title").map(str::to_string),
                    status: arg_str(args, "status").map(str::to_string),
                    issue: arg_str(args, "issue").map(str::to_string),
                    pr: arg_str(args, "pr").map(str::to_string),
                    pr_base: arg_str(args, "pr_base").map(str::to_string),
                    assignee: arg_str(args, "assignee").map(str::to_string),
                    session: arg_str(args, "session").map(str::to_string),
                    note: arg_str(args, "note").map(str::to_string),
                    deps: arg_str_array(args, "deps")?,
                    related: arg_str_array(args, "related")?,
                    claim,
                },
            )?;
            // A claim names its holder back: the whole point of the guarded
            // write is knowing WHO ended up with the task, not just that the
            // call succeeded.
            let claimed = if claim {
                format!(" (claimed by {})", task.assignee.as_deref().unwrap_or("?"))
            } else {
                String::new()
            };
            Ok(format!("{} \"{}\" — {}{claimed}", task.id, task.title, task.status))
        }
        // ---- the bisecting merge queue (#581 §11.1) ----
        //
        // Authorization is enforced HERE, not by the role-filtered listing
        // above: a tool omitted from a listing is still callable, which is the
        // double-gate precedent `review_verdict` sets. `queue_merge` can cause
        // the backend to push a ref and open a PR, so "only an orchestrator may
        // ask for that" must not depend on a JSON shim's cosmetic filter.
        //
        // The group is `caller.group` throughout — never an argument — so there
        // is no cross-group surface here to check: a caller cannot name another
        // group's queue.
        "queue_merge" => {
            require_orchestrator(caller)?;
            let raw = arg_str(args, "pr").ok_or("pr required")?;
            let pr = super::pr_number(raw).ok_or("pr must be a number, #n, or a PR URL")?;
            let target = arg_str_strict(args, "target")?;
            let out = reg.queue_merge(&caller.group, pr, target);
            Ok(serde_json::to_string(&out).unwrap_or_default())
        }
        "merge_queue_status" => {
            require_orchestrator(caller)?;
            let out = reg.merge_queue_status(&caller.group);
            Ok(serde_json::to_string(&out).unwrap_or_default())
        }
        "cancel_queued_merge" => {
            require_orchestrator(caller)?;
            let raw = arg_str(args, "pr").ok_or("pr required")?;
            let pr = super::pr_number(raw).ok_or("pr must be a number, #n, or a PR URL")?;
            let out = reg.cancel_queued_merge(&caller.group, pr);
            Ok(serde_json::to_string(&out).unwrap_or_default())
        }

        "remove_task" => {
            require_orchestrator(caller)?;
            let id = arg_str(args, "id").ok_or("id required")?;
            reg.delete_task(&caller.group, &caller.agent_id, id)?;
            Ok(format!("removed {id}"))
        }
        "group_usage" => {
            require_orchestrator(caller)?;
            Ok(reg.group_usage(&caller.group).to_string())
        }
        "queue_orphans" => {
            require_orchestrator(caller)?;
            Ok(reg.queue_orphans_json(&caller.group).to_string())
        }

        "spawn_agent" => {
            require_orchestrator(caller)?;
            // An unrecognized kind is REJECTED (#222). This used to be
            // `_ => Role::Worker` — so a typo'd or hallucinated kind silently
            // became a *worker*, complete with a worktree and write access. A
            // capability class is the one thing that must never be guessed.
            //
            // #544 closes the other half of that same door: an OMITTED kind used
            // to default to `Role::Worker` here, so forgetting the argument had
            // the identical effect a typo used to have — the most-privileged
            // class, silently. `Option` all the way down now; the fresh-spawn
            // requirement is enforced below (it needs `block` and `resume`,
            // which are parsed after this).
            let kind = match arg_str(args, "kind") {
                None => None,
                Some(k) => Some(super::workflow::kind_from_str(k).ok_or_else(|| {
                    format!(
                        "unknown kind {k:?} — must be one of {}",
                        super::workflow::kind_names()
                    )
                })?),
            };
            // ...but `orchestrator` is a kind loomux *can* name, and this tool is
            // the one place an agent chooses one. Delegates only.
            //
            // This check is load-bearing, and it is easy to lose: before #222 the
            // `_ => Role::Worker` catch-all above happened to swallow
            // `kind: "orchestrator"` too, so nothing else ever had to say no.
            // Making unknown kinds an error removed that accident — and an
            // orchestrator-kind spawn is exempt from the live-agent cap AND the
            // spawn-rate backstop (both sit inside `if role != Role::Orchestrator`
            // in `spawn_agent_ex`) AND resolves to `Caller.role == Orchestrator`,
            // which is what `require_orchestrator` gates the privileged tools on.
            // An orchestrator that called `spawn_agent(kind: "orchestrator")` in a
            // loop would fork-bomb the machine with fully-privileged panes.
            // The JSON-schema `enum` in `tool_defs` is advertisement; it is never
            // enforced against the incoming arguments. This is the enforcement.
            if kind == Some(Role::Orchestrator) {
                return Err(
                    "kind must be worker | reviewer | planner — a group has exactly one \
                     orchestrator (you), opened at launch"
                        .into(),
                );
            }
            // A block names one of the repo's declared personas (#222). Its
            // `kind` is authoritative when set, so `kind` above is only the
            // fallback for a plain spawn.
            //
            // Normalized the same way `mod.rs`'s own block resolution treats a
            // named block (trim + empty → absent): an empty-string `block` arg
            // must be indistinguishable from an omitted one, or
            // `{"resume_session": .., "block": ""}` would skip the #254
            // inheritance guard below (which only checks `is_none()`) and fall
            // straight through to `spawn_agent_ex`, which then discards the
            // empty id anyway and defaults to `block_for(Worker)` — reproducing
            // the exact silent re-role this fix exists to close.
            let block = arg_str(args, "block")
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(str::to_string);
            let task = arg_str(args, "task").unwrap_or("");
            let name = arg_str(args, "name").unwrap_or("");
            let requested_worktree = args.get("worktree").and_then(Value::as_bool);
            let branch = arg_str(args, "branch").map(str::to_string);
            let base = arg_str(args, "base").map(str::to_string);
            // #190: a hand-copied or logged session id is commonly a truncated
            // prefix (Claude Code ids are full UUIDs); resolve it against this
            // group's OWN roster before anything below treats it as the final
            // id — the block-inheritance lookup just below and the actual
            // resume in `spawn_agent_ex` must agree on the same full id.
            let resume = match arg_str(args, "resume_session") {
                Some(raw) => Some(super::resolve_session_ref(&reg.merged_records(&caller.group), raw)?),
                None => None,
            };
            let cwd = arg_str(args, "cwd").map(str::to_string);
            let resumed = resume.is_some();
            // #544: a FRESH spawn must name its capability class — `kind` or
            // `block`, deliberately, every time. There is no default any more.
            //
            // The incident: three reviewer-shaped briefs ("Fresh review of PR
            // #536 …", names literally starting with `rev:`, tasks telling the
            // agent to record a verdict with `review_verdict`) were spawned
            // with `kind` omitted and came back as WORKERS — read-write panes
            // with edit tools and git commit/push. Every containment guardrail
            // this repo has (#448/#462/#465) protects a pane that was correctly
            // *classified*; none of it helps when the classification itself was
            // acquired by forgetting an argument. Defaulting to the
            // most-privileged class makes omission fail OPEN on a capability
            // boundary, which is the one place it must never do that.
            //
            // A resume is deliberately exempt and keeps its #254 semantics:
            // omitting both there INHERITS the resumed session's own block
            // (below), which is a *stricter* answer than any default — it
            // re-derives nothing, and an unknown session is already a hard
            // error rather than a silent worker.
            if !resumed && kind.is_none() && block.is_none() {
                return Err(
                    "spawn_agent requires an explicit capability class on a fresh spawn (#544): \
                     pass kind (worker | reviewer | planner) — or block, naming one of this \
                     group's declared blocks, which carries its own kind. There is no default: \
                     a spawn that named neither used to become a WORKER, the most-privileged \
                     class, so forgetting the argument silently produced a read-write pane \
                     (edit tools, git commit/push) for a task you may have meant for a reviewer \
                     or planner. If this is a follow-up on an existing conversation, pass \
                     resume_session instead — that inherits the session's own class."
                        .into(),
                );
            }
            // The last-touched roster record naming this session, if any —
            // shared by #254's block inheritance and the cwd inheritance
            // below, so both agree on the same record instead of running two
            // independent lookups that could (in principle, if the roster
            // changed between them) disagree on which one is "last-touched".
            let owner: Option<super::AgentRecord> = resume.as_deref().and_then(|session_id| {
                reg.merged_records(&caller.group)
                    .into_iter()
                    .filter(|r| r.session.as_deref() == Some(session_id))
                    .max_by_key(|r| r.updated_ms)
            });
            // #338/#359: a worker or reviewer spawn always lands in a dedicated
            // workspace — the main clone is the human's environment, and neither
            // must conflict with it (a worker by branching/committing there; a
            // reviewer by contending on its checkout state with another reviewer
            // or the orchestrator's own fetch/merge traffic — the #359 incident).
            // Resolved against the EFFECTIVE role (the named block's kind wins
            // over `kind` — same precedence `spawn_agent_ex` itself applies) so a
            // `block: "worker-fast"` or `block: "rev-security"` spawn is covered
            // exactly like a bare `kind: "worker"`/`"reviewer"` one. Skipped
            // entirely for a resume: `spawn_agent_ex`'s `cwd_override` branch wins
            // over this flag whenever `cwd` is given (the resume's documented
            // shape), so gating `worktree` there would only reject a value that
            // can't actually do anything — but see the `cwd` check just below,
            // which closes the fresh-spawn side of the same door.
            let worktree = if resumed {
                requested_worktree.unwrap_or(false)
            } else {
                // With no block, the fresh-spawn requirement above guarantees
                // `kind` is Some here — this branch never has to invent one.
                let effective_role = match block.as_deref() {
                    Some(id) => reg.group(&caller.group).and_then(|g| g.guardrails.block(id).map(|b| b.kind)),
                    None => kind,
                };
                // A FRESH (non-resume) spawn's `cwd` is `spawn_agent_ex`'s
                // `cwd_override`, which wins over `worktree` unconditionally —
                // so a worker/reviewer spawn carrying an explicit `cwd` bypasses
                // the dedicated-workspace guarantee above just as completely as
                // `worktree: false` would, regardless of what `worktree` itself
                // says. Reject it the same way, before the worktree/false check
                // below even runs (an explicit `cwd` makes that check moot).
                if let Some(r) = effective_role.filter(|r| needs_dedicated_workspace(*r)) {
                    if cwd.is_some() {
                        return Err(format!(
                            "guardrail: a fresh {r} spawn may not override its workspace with \
                             `cwd` (#338/#359) — that bypasses the dedicated-workspace guarantee \
                             the same way worktree=false would. `cwd` only has a role on a \
                             resume_session (where it's optional and inherited, see \
                             resume_session); for a fresh {r} spawn, omit it and let loomux cut \
                             the worktree.",
                            r = r.as_str(),
                        ));
                    }
                }
                match (effective_role.filter(|r| needs_dedicated_workspace(*r)), requested_worktree) {
                    (Some(r), Some(false)) => {
                        return Err(format!(
                            "guardrail: {r} spawns always use a dedicated worktree (#338/#359) — \
                             the main clone is the human's environment and a {r} must not \
                             conflict with it. Omit `worktree` (it now defaults on for {r}s) \
                             instead of passing false. For your OWN mechanical work (rebases, \
                             conflict fixes), use your own staging worktree rather than spawning \
                             a {r} with worktree=false.",
                            r = r.as_str(),
                        ));
                    }
                    (Some(_), _) => true,
                    (_, requested) => requested.unwrap_or(false),
                }
            };
            // #254: a resume that names NEITHER `kind` NOR `block` inherits the
            // resumed session's original block from this group's roster
            // (`agents.json`'s session→agent→block mapping) instead of falling
            // through to `kind`'s default block. Before this fix, that fall-
            // through is exactly what silently re-roled a resumed reviewer to
            // `worker-deep` — wrong model, wrong persona, and (since
            // `review_verdict` is denied to non-reviewers below) structurally
            // incapable of recording its verdict, with no error anywhere. An
            // explicit `kind` or `block` on the call is a deliberate choice and
            // is left alone — only the fully block-less, kind-less resume (the
            // shape the tool description above documents as the whole
            // follow-up contract) gets inherited instead of guessed.
            let block = if block.is_none() && kind.is_none() {
                if resumed {
                    // A session can appear more than once (roster + audit
                    // backfill can both carry it, or it was re-spawned into
                    // a different block over its lifetime) — `owner` (above)
                    // already picked the last-touched record deliberately,
                    // since that is the agent's most recent identity, not its
                    // first one.
                    let session_id = resume.as_deref().expect("resumed implies Some");
                    let owner_rec = owner.as_ref().ok_or_else(|| {
                        format!(
                            "unknown session {session_id:?} — cannot resume without an \
                             explicit block or kind (no roster record maps this session \
                             to one). Pass block (or kind) explicitly if you are sure of \
                             its capability class."
                        )
                    })?;
                    let owner_block = if owner_rec.block.trim().is_empty() {
                        // Pre-#222 roster row: only a role was ever recorded,
                        // no block identity — inherit that role's default
                        // block instead, since there is no block id to name.
                        //
                        // #544: an UNPARSEABLE recorded role used to fall back
                        // to `kind` — which, in this branch, is by construction
                        // the omitted-`kind` default, i.e. Worker. So a corrupt
                        // or future-version roster row resumed bare handed back
                        // the most-privileged class with nothing said. Refuse
                        // instead: this is the same "never guess a capability
                        // class" rule the fresh-spawn requirement above states.
                        let owner_role = super::workflow::kind_from_str(&owner_rec.role)
                            .ok_or_else(|| {
                                format!(
                                    "session {session_id:?} is recorded under an unrecognized \
                                     role {:?} and carries no block id, so its capability class \
                                     cannot be inherited (#544) — pass block (or kind) \
                                     explicitly. It is not assumed to be a worker.",
                                    owner_rec.role
                                )
                            })?;
                        reg.group(&caller.group)
                            .and_then(|g| g.guardrails.block_for(owner_role).map(|b| b.id.clone()))
                            .ok_or_else(|| {
                                format!(
                                    "this group's workflow declares no {} block",
                                    owner_role.as_str()
                                )
                            })?
                    } else {
                        owner_rec.block.clone()
                    };
                    Some(owner_block)
                } else {
                    None
                }
            } else {
                block
            };
            // rev-13 finding on #345 (extended for #359 to cover reviewers too):
            // a worker/reviewer RESUME that omits `cwd` fell through silently to
            // `spawn_agent_ex`'s per-role default — the main clone for anything
            // that isn't Orchestrator/Planner — which is exactly the conflict
            // #338/#359 exist to prevent, just reached via a resume instead of a
            // fresh spawn. When `cwd` is omitted on a resume, inherit the
            // session's recorded workspace from the roster (`owner`, above — the
            // same last-touched record #254's block inheritance uses) instead.
            //
            // #412: the roster's cwd can be STALE (a worktree that moved or was
            // deleted since the session ran) as well as merely absent — either
            // way, before giving up, fall back to locating the session directly
            // in its CLI's own store by id (`resolve_worker_resume_cwd`, shared
            // with `resume_recorded_session`'s worker/reviewer path so the two
            // resume entry points — MCP-driven and session-browser-driven —
            // resolve identically) rather than trusting a directory that may no
            // longer exist. Only when THAT also comes up empty, for a role that
            // needs a dedicated workspace, is this rejected loudly — the same
            // style as an explicit worktree=false — rather than guessing a
            // workspace or defaulting to the main clone.
            //
            // The dedicated-workspace check runs FIRST, before any store lookup
            // is even attempted (#412 review N3): a planner is untouched by any
            // of this — no store scan on its behalf, `cwd` stays exactly what
            // the roster inherited (or `None`) — so "planners are unaffected"
            // stays literally true, not just true in the common case.
            let cwd = if resumed && cwd.is_none() {
                let group = reg.group(&caller.group);
                // A block-less resume that reaches here always carried an
                // explicit `kind` (a bare one inherited a block just above), so
                // `kind` is Some — and when it somehow isn't, the effective
                // block stays None rather than being derived from a guessed
                // class (#544).
                let effective_block = match block.as_deref() {
                    Some(id) => group.as_ref().and_then(|g| g.guardrails.block(id).cloned()),
                    None => kind.and_then(|k| {
                        group.as_ref().and_then(|g| g.guardrails.block_for(k).cloned())
                    }),
                };
                let effective_role = effective_block.as_ref().map(|b| b.kind);
                let dedicated = effective_role.filter(|r| needs_dedicated_workspace(*r));
                match dedicated {
                    // #412 rev-17 NB3: restores the pre-PR `is_dir()` check this
                    // branch briefly lost in the mcp.rs restructure — a planner's
                    // roster cwd is always `group.repo` in practice, so this is
                    // unreachable today, but a vanished recorded cwd must still
                    // fall back to `None` (→ spawn_agent_ex's per-role default)
                    // rather than reach it as a doomed `cwd_override`.
                    None => owner
                        .as_ref()
                        .map(|o| o.cwd.trim())
                        .filter(|c| !c.is_empty() && Path::new(c).is_dir())
                        .map(str::to_string),
                    Some(r) => {
                        let (Some(sid), Some(g), Some(b)) = (resume.as_deref(), &group, &effective_block)
                        else {
                            return Err(format!(
                                "guardrail: this {r} resume has no recorded workspace to inherit, \
                                 and there is no session id, group, or block to check against a \
                                 CLI store (#338/#359) — pass `cwd` explicitly. A {r} resume must \
                                 never fall back to the main clone, which is the human's \
                                 environment.",
                                r = r.as_str(),
                            ));
                        };
                        let cli = super::workflow::cli_of(b, &g.guardrails.agent_cli);
                        let roster_cwd = owner.as_ref().map(|o| o.cwd.as_str());
                        match super::resolve_worker_resume_cwd(cli, sid, roster_cwd, &g.repo) {
                            Ok(c) => Some(c),
                            Err(tagged) => {
                                return Err(format!(
                                    "{tagged} This {r} resume also has no recorded workspace to \
                                     inherit (#338/#359) — pass `cwd` explicitly (the session's \
                                     original workspace), or confirm the session id. A {r} \
                                     resume must never fall back to the main clone, which is the \
                                     human's environment.",
                                    r = r.as_str(),
                                ));
                            }
                        }
                    }
                }
            } else {
                cwd
            };
            // `kind` is None only when `block` is Some — a fresh spawn naming
            // neither was refused above, and a bare resume inherited a block —
            // and a named block's own kind is authoritative inside
            // `spawn_agent_ex`, which discards this argument entirely. So this
            // is never a capability decision. It is `Planner` (the least
            // privileged class, and the one that never gets a worktree) rather
            // than `Worker` so that even a future path reaching it with no
            // block could not acquire write capability by omission — the whole
            // point of #544 — and would fail loudly on a group with no planner
            // block instead.
            let role = kind.unwrap_or(Role::Planner);
            let a = reg.spawn_agent_ex(&caller.group, role, block, name, task, worktree, branch, base, resume, cwd, None)?;
            // Copilot mints its session id a few seconds into boot; loomux
            // binds it to the pane once it appears (visible then in
            // list_agents / the task board).
            let session = a
                .session_id
                .as_deref()
                .map(|s| format!("Session {s}."))
                .unwrap_or_else(|| "Session id will appear in list_agents once Copilot initializes.".into());
            Ok(format!(
                "spawned {} (\"{}\", block {}, {:?}){}. {} It will report when ready.",
                a.id,
                a.name,
                a.block,
                a.role,
                if resumed { " resuming its previous session" } else { "" },
                session,
            ))
        }
        "send_prompt" => {
            require_orchestrator(caller)?;
            let target = arg_str(args, "agent_id").ok_or("agent_id required")?;
            let text = arg_str(args, "text").ok_or("text required")?;
            let a = require_in_group(reg, caller, target)?;
            if a.id == caller.agent_id {
                return Err("cannot send a prompt to yourself".into());
            }
            // The target is being given work/direction — it is no longer
            // idle, so the idle-kill guardrail's clock stops for it. Marked
            // before delivery (which is async in the running app) so the
            // intent to assign counts regardless of delivery timing.
            reg.set_agent_idle(&a.id, false);
            reg.deliver_prompt(&a.id, text, &caller.agent_id, Delivery::MidSession)?;
            Ok(format!("prompt delivered to {}", a.id))
        }
        "get_output" => {
            require_orchestrator(caller)?;
            let target = arg_str(args, "agent_id").ok_or("agent_id required")?;
            let lines = args.get("lines").and_then(Value::as_u64).unwrap_or(60) as usize;
            let a = require_in_group(reg, caller, target)?;
            reg.agent_output_tail(&a.id, lines)
        }
        "kill_agent" => {
            require_orchestrator(caller)?;
            let target = arg_str(args, "agent_id").ok_or("agent_id required")?;
            let a = require_in_group(reg, caller, target)?;
            reg.kill_agent(&a.id)?;
            Ok(format!("kill signal sent to {}", a.id))
        }
        "focus_agent" => {
            require_orchestrator(caller)?;
            let target = arg_str(args, "agent_id").ok_or("agent_id required")?;
            let a = require_in_group(reg, caller, target)?;
            reg.focus_agent(&a.id)?;
            Ok(format!("focused {}", a.id))
        }
        "rename_agent" => {
            require_orchestrator(caller)?;
            let target = arg_str(args, "agent_id").ok_or("agent_id required")?;
            let name = arg_str(args, "name").ok_or("name required")?;
            // Scope to the caller's group; rename_agent enforces alive + the
            // human > orchestrator precedence and returns the applied name.
            let a = require_in_group(reg, caller, target)?;
            let applied = reg.rename_agent(&a.id, name, NameSource::Orchestrator)?;
            Ok(format!("renamed {} to \"{applied}\"", a.id))
        }
        "set_state" => {
            require_orchestrator(caller)?;
            let state = arg_str(args, "state").ok_or("state required")?;
            reg.set_state(&caller.group, state)?;
            // Self-scoped sign-of-life for request_compact's offload-checklist
            // warning (#328) — see AgentEntry.last_state_write_ms.
            reg.note_state_write(&caller.agent_id);
            Ok("state saved".into())
        }

        "request_compact" => reg.request_compact(&caller.agent_id),

        "note_directive" => {
            let text = arg_str(args, "text").ok_or("text required")?;
            let replace = args.get("replace").and_then(Value::as_bool).unwrap_or(false);
            reg.note_directive(&caller.agent_id, text, replace)
        }

        "notify_when" => {
            require_not_planner(caller)?;
            let kind = arg_str(args, "kind").ok_or("kind required")?;
            let condition = match kind {
                "pr_checks" => {
                    let raw = arg_str(args, "pr").ok_or("pr required for pr_checks")?;
                    let pr = super::pr_number(raw)
                        .ok_or_else(|| format!("cannot parse a PR number from {raw:?}"))?;
                    super::notify::Condition::PrChecks { pr }
                }
                "workflow_run" => {
                    let raw = arg_str(args, "run").ok_or("run required for workflow_run")?;
                    // `run_id_from`, not the bare `pr_number` tail-digits parse:
                    // a run URL can carry a trailing `/job/<id>` segment whose
                    // digits are a DIFFERENT number (the job id), which
                    // `pr_number` would silently return instead.
                    let run = super::notify::run_id_from(raw)
                        .ok_or_else(|| format!("cannot parse a run id from {raw:?}"))?;
                    super::notify::Condition::WorkflowRun { run }
                }
                // Unrecognized kind is REJECTED, never defaulted (the
                // spawn_agent kind lesson, #222) — there is no sensible
                // fallback condition to silently watch instead.
                other => {
                    return Err(format!(
                        "unrecognized notification kind: {other:?} (must be pr_checks or workflow_run)"
                    ))
                }
            };
            // Capped (well above `NOTICE_FIELD_CAP`, which trims it again at
            // notice time) so an agent can't stash an unbounded string in a
            // watch that lives up to 4h — a cheap bound, not a security
            // boundary (the note is sanitized separately before it ever
            // enters a notice).
            let note: String = arg_str(args, "note").unwrap_or("").chars().take(500).collect();
            // Present-but-not-a-whole-number (a JSON string, a fraction) is
            // REJECTED, not silently discarded to the default: the caller
            // did supply a value, and clamp_expires_minutes(None) would
            // otherwise turn "30" or 30.5 into a mysterious 60 with no
            // signal anything was wrong. Absent entirely is the one case
            // that legitimately defaults.
            let expires_minutes = match args.get("expires_minutes") {
                None => super::notify::clamp_expires_minutes(None),
                Some(v) => match v.as_u64() {
                    Some(n) => super::notify::clamp_expires_minutes(Some(n as u32)),
                    None => {
                        return Err(format!("expires_minutes must be a whole number of minutes, got: {v}"))
                    }
                },
            };
            let w = reg.register_notification(&caller.group, &caller.agent_id, condition, note, expires_minutes)?;
            Ok(format!(
                "registered {} ({}), polled every 30s, expires in {expires_minutes} min. \
                 You will get a [loomux] notice in this pane when it completes — do other work until then.",
                w.id, w.condition.label(),
            ))
        }
        "list_notifications" => {
            require_not_planner(caller)?;
            Ok(reg.list_notifications(&caller.agent_id).to_string())
        }
        "cancel_notification" => {
            require_not_planner(caller)?;
            let id = arg_str(args, "id").ok_or("id required")?;
            reg.cancel_notification(&caller.agent_id, id)?;
            Ok(format!("cancelled {id}"))
        }

        "channel_send" => {
            require_not_planner(caller)?;
            let text = arg_str(args, "text").ok_or("text required")?;
            reg.channel_send(caller, text)
        }
        "channel_status" => {
            require_not_planner(caller)?;
            Ok(reg.channel_status(caller).to_string())
        }

        "report" => {
            if caller.role == Role::Orchestrator {
                return Err("report is for workers/reviewers; use send_prompt".into());
            }
            // #398: two report shapes share this one tool. `outcome` (new,
            // structured) is a superset of legacy `status` — it's what implies
            // status when the caller omits it, so a fully-structured report
            // never has to pass both. Either the legacy `status` or the new
            // `outcome` is required (never neither); same for the text.
            let outcome = arg_str(args, "outcome");
            if let Some(o) = outcome {
                if !report::OUTCOMES.contains(&o) {
                    return Err(format!("outcome must be one of: {}", report::OUTCOMES.join(", ")));
                }
            }
            let status_arg = arg_str(args, "status");
            if let Some(s) = status_arg {
                if !report::STATUSES.contains(&s) {
                    return Err("status must be progress | done | blocked".into());
                }
            }
            let status = status_arg
                .or_else(|| outcome.map(report::status_for_outcome))
                .ok_or("status or outcome required")?;
            let note = arg_str(args, "note").filter(|s| !s.is_empty());
            let summary = arg_str(args, "summary").filter(|s| !s.is_empty());
            if note.is_none() && summary.is_none() {
                return Err("summary or note required".into());
            }
            // A worker that finished (done) or stalled (blocked) is idle
            // again — restart its idle-kill clock; progress keeps it active.
            reg.set_agent_idle(&caller.agent_id, matches!(status, "done" | "blocked"));
            // Attention routing: a done/blocked report badges the pane (and can
            // toast) so the human sees which one needs them; progress clears it.
            reg.note_report_attention(&caller.agent_id, status);
            let message = match outcome {
                // Structured path: the note is hard-capped here (not in a
                // template guideline) and the notice names the ref/pointer the
                // orchestrator routes on instead of reading a paraphrase.
                Some(o) => {
                    let body = note.map(report::truncate_note).unwrap_or_else(|| summary.unwrap_or("").to_string());
                    report::structured_notice(&caller.agent_id, o, &body, arg_str(args, "ref"), arg_str(args, "detail_url"))
                }
                // Legacy path: byte-for-byte the pre-#398 message, uncapped.
                None => format!("[loomux] {} reports {status}: {}", caller.agent_id, note.or(summary).unwrap()),
            };
            // #576 residual: the relay variant, which opts this notice into the
            // question gate's delivery record — the note is the CALLER's words
            // landing in the ORCHESTRATOR's pane, which is the cross-pane
            // authorship the record requires. See
            // `deliver_relayed_to_orchestrator`.
            reg.deliver_relayed_to_orchestrator(&caller.group, &message, &caller.agent_id)?;
            // #203: a planner's contract is one plan → one report → exit. Close
            // its pane deterministically on the `done` report so it stops holding
            // a delegate slot the instant its work is posted — the role-template
            // exit instruction is only belt-and-braces. The report is handed off
            // first (above); the close enqueues the completion exit notice after
            // it (see `close_completed_planner` for the ordering guarantee and
            // its edges). Progress/blocked reports leave the planner alone.
            if caller.role == Role::Planner && status == "done" {
                reg.close_completed_planner(&caller.agent_id);
            }
            Ok("reported to orchestrator".into())
        }
        "review_verdict" => {
            // Authorization is enforced twice on purpose: here, and again in
            // `record_verdict` next to the write. A verdict is what opens a merge
            // gate, so "only a reviewer may record one" must not depend on a single
            // check in a JSON shim.
            if caller.role != Role::Reviewer {
                return Err("permission denied: review_verdict is for reviewer-kind blocks — \
                            use report(status, summary)".into());
            }
            let pr = arg_str(args, "pr").ok_or("pr required")?;
            let verdict = arg_str(args, "verdict").ok_or("verdict required")?;
            let summary = arg_str(args, "summary").ok_or("summary required")?;
            let rec = reg.record_verdict(&caller.group, &caller.agent_id, pr, verdict, summary)?;
            // A verdict is also news: the orchestrator is the one that decides what
            // happens next (send the findings back to the worker, ask the human,
            // merge), and loomux's design norm is that agent→agent traffic arrives
            // as a VISIBLE prompt in the recipient's pane — never a side channel.
            let gate = reg.gate_status_line(&caller.group, rec.pr);
            let _ = reg.deliver_to_orchestrator(
                &caller.group,
                &format!(
                    "[loomux] {} ({}) recorded verdict {} on PR #{}: {}{}",
                    caller.agent_id,
                    rec.block,
                    rec.verdict.as_str().to_uppercase(),
                    rec.pr,
                    rec.summary,
                    gate.as_deref().map(|g| format!("\n[loomux] {g}")).unwrap_or_default(),
                ),
                &caller.agent_id,
            );
            Ok(format!(
                "recorded: {} on PR #{} attributed to block {}. {}",
                rec.verdict.as_str().to_uppercase(),
                rec.pr,
                rec.block,
                gate.unwrap_or_else(|| "This group declares no merge gate, so the verdict is \
                    recorded for the humans and the orchestrator to read; the human merge gate \
                    is unchanged.".into()),
            ))
        }
        "list_verdicts" => {
            let prs = match arg_str(args, "pr") {
                Some(pr) => vec![super::pr_number(pr)
                    .ok_or_else(|| format!("no PR number found in {pr:?}"))?],
                None => reg.verdict_prs(&caller.group),
            };
            let out: Vec<Value> = prs
                .into_iter()
                .map(|pr| {
                    // #565: a verdict pins the head SHA, so a moved head is visible.
                    // The PR BODY is not part of that SHA and moves silently — and
                    // on a squash-merging repo it is the commit message. `body_changed`
                    // is the same staleness question asked of the other half of the
                    // reviewed artifact, per verdict: true/false when it can be
                    // answered, ABSENT when it cannot (no digest recorded, or the
                    // body unreadable now) — never a `false` that means "we didn't
                    // check". Handling is asymmetric and the gate line says which is
                    // which: on a `pass` it is a hazard, on a `fail` it is the fix
                    // loop and quite possibly a finding that is already fixed.
                    let now = reg.pr_body_digest(&caller.group, pr);
                    let verdicts: Vec<Value> = reg
                        .verdicts(&caller.group, pr)
                        .into_iter()
                        .map(|v| {
                            let changed = v.body_changed(now.as_deref());
                            let mut val = serde_json::to_value(&v).unwrap_or(Value::Null);
                            if let (Some(changed), Some(obj)) = (changed, val.as_object_mut()) {
                                obj.insert("body_changed".into(), json!(changed));
                            }
                            val
                        })
                        .collect();
                    json!({
                        "pr": pr,
                        "verdicts": verdicts,
                        "gate": reg.gate_status_line(&caller.group, pr),
                    })
                })
                .collect();
            Ok(serde_json::to_string(&out).unwrap_or_default())
        }

        // `process`-hinted worker blocks only — see the matching note on this
        // tool's `tool_defs` entry. The listing already hides this from
        // everyone else; this is the real, re-checked gate.
        "session_digest" => {
            if caller.role != Role::Worker || caller.role_hint.as_deref() != Some("process") {
                return Err("permission denied: session_digest is for process-hinted worker blocks".into());
            }
            let task = arg_str_strict(args, "task")?;
            let agent = arg_str_strict(args, "agent")?;
            let pr = arg_str_strict(args, "pr")?;
            let provided = [task.is_some(), agent.is_some(), pr.is_some()].into_iter().filter(|b| *b).count();
            if provided != 1 {
                return Err("exactly one of task, agent, or pr is required".into());
            }
            let lookup = if let Some(t) = task {
                super::DigestLookup::Task(t.to_string())
            } else if let Some(a) = agent {
                // Group-scoped by construction: `merged_records(&caller.group)`
                // only ever contains this group's rows, so an id from another
                // group is simply absent — same "unknown agent" shape
                // `require_in_group` gives a live target, without needing a
                // live `AgentEntry` (this tool must also find DEAD agents —
                // see `OrchRegistry::session_digest`'s doc comment).
                if !reg.merged_records(&caller.group).iter().any(|r| r.id == a) {
                    return Err(format!("unknown agent: {a}"));
                }
                super::DigestLookup::Agent(a.to_string())
            } else {
                super::DigestLookup::Pr(pr.unwrap().to_string())
            };
            let digest = reg.session_digest(&caller.group, lookup)?;
            Ok(serde_json::to_string(&digest).unwrap_or_default())
        }

        "message_orchestrator" => {
            if caller.role == Role::Orchestrator {
                return Err("you are the orchestrator".into());
            }
            let text = arg_str(args, "text").ok_or("text required")?;
            // A message is a sign of life: reset the watchdog's silence clock
            // (report already does this via set_agent_idle).
            reg.note_agent_activity(&caller.agent_id);
            // #576 residual: same relay variant, same reason as `report` above.
            reg.deliver_relayed_to_orchestrator(
                &caller.group,
                &format!("[loomux] message from {}: {text}", caller.agent_id),
                &caller.agent_id,
            )?;
            Ok("message delivered".into())
        }

        _ => Err(format!("unknown tool: {name}")),
    }
}
