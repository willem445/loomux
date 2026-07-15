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

use super::{Caller, Delivery, NameSource, OrchRegistry, Role};
use serde_json::{json, Value};
use std::io::Read as _;
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
        "tools/list" => Ok(json!({ "tools": tool_defs(caller.role) })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            reg.audit(&caller.group, &caller.agent_id, "tool-call",
                json!({ "tool": name, "args": args }));
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
            Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
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

/// The tool surface is role-filtered so workers never even see privileged
/// tools; `call_tool` re-checks anyway (listing is cosmetic, not security).
fn tool_defs(role: Role) -> Vec<Value> {
    let mut tools = vec![
        tool("list_agents", "List the agents in your orchestration group with role, status, and task.",
            json!({}), &[]),
        tool("get_state", "Read the group's durable orchestration state (JSON string). Survives sessions.",
            json!({}), &[]),
        tool("list_tasks",
            "Read the group's task board (JSON array, order = priority) as COMPACT rows: id, title, status, issue, pr, assignee, session, updated_ms, note_count — NO note text. The human sees and edits the full board (with notes) beside your pane. Use note_count to tell whether a task has history worth pulling, then call get_task(id) for that task's full notes.",
            json!({}), &[]),
        tool("get_task",
            "Read ONE task's full record, including its note history (capped: only the newest notes are kept verbatim, older ones collapse into one placeholder — the full text of every note is always in this group's audit log regardless). Use this after list_tasks's compact row shows a note_count worth reading.",
            json!({ "id": { "type": "string", "description": "Task id, e.g. t-3" } }),
            &["id"]),
        tool("list_verdicts",
            "Read the recorded review verdicts for a PR: which reviewer block recorded what (pass | fail | escalate), when, and its summary — plus, when this repo's .loomux/workflow.yml declares a merge gate, whether that gate is satisfied. This is STATE, not a notification: it is what the loomux gh interceptor reads when it decides whether to allow `gh pr merge`. Omit pr to list every PR with a recorded verdict.",
            json!({
                "pr": { "type": "string", "description": "PR number, #n, or URL. Omit to list all PRs with verdicts." },
            }),
            &[]),
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
                "Register a background watch on a CI/run condition and get a [loomux] notice IN THIS PANE the moment it fires — never another agent's. Register and immediately go do other work; do not sleep or re-poll `gh pr checks`/`gh run view` yourself, loomux polls every 30s. kind: \"pr_checks\" (a PR's checks reach SUCCESS/FAILURE — pass pr) or \"workflow_run\" (a specific `gh run` id completes — pass run). expires_minutes defaults to 60, clamped to 5-240. Capped at 4 live per agent / 12 per group; cancel one with cancel_notification or let it fire/expire to free a slot.",
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
        ]);
    }
    if role == Role::Orchestrator {
        tools.extend([
            tool("spawn_agent",
                "Open a new worker, reviewer, or planner agent pane in this group. Guardrails apply: live-agent cap and per-role pinned CLI + model. Set worktree=true for parallel work that must not collide; give branch a meaningful name either way. Empty task spawns an idle agent awaiting prompts. A planner explores the codebase read-only and writes an implementation plan as a GitHub issue comment, then reports and exits. Its read-only contract is enforced structurally where the CLI allows it — it never gets a worktree, and its file-editing tools plus git commit/push are denied at the CLI level — so it cannot edit files or push code; not opening PRs is asked of it in its instructions (gh stays available so it can post the plan comment). For a FOLLOW-UP on a finished task, pass resume_session (from list_agents/the task board) plus cwd (where that work happened) — the pane reopens that conversation with its context instead of cold-starting. A resume with no kind/block INHERITS the resumed session's original block (and therefore its persona, model and capability class) from this group's roster — it never re-derives a default from `kind`, so a reviewer resumed bare comes back a reviewer, not a worker. An unrecognized session id with no block is a hard error, never a silent worker spawn. To deliberately re-role a resumed session into a different capability class, pass `block` explicitly — same as any other spawn, and audited the same way (the agent-spawn record always carries block + session + resume).",
                json!({
                    "name": { "type": "string", "description": "Short display name for the pane" },
                    "kind": { "type": "string", "enum": ["worker", "reviewer", "planner"], "description": "Capability class (default worker). An unrecognized value is rejected, never treated as a worker. On a resume_session, passing this ALSO defeats block inheritance — same as passing block — and re-derives the default block for that kind instead; omit both to inherit the resumed session's own block." },
                    "block": { "type": "string", "description": "Id of a block declared in the repo's .loomux/workflow.yml — e.g. 'rev-security'. The block supplies the persona, CLI, model and capability class (so `kind` is ignored when this is set). Your kickoff lists the blocks this group has; omit it to get the default block for `kind` — UNLESS resume_session is set, in which case omitting it inherits that session's own original block instead (see resume_session). Set it explicitly on a resume only when you mean to re-role that conversation into a different capability class." },
                    "task": { "type": "string", "description": "Full task brief; empty = idle. With resume_session, this is the follow-up prompt." },
                    "worktree": { "type": "boolean", "description": "Create a dedicated git worktree + branch" },
                    "branch": { "type": "string", "description": "Branch name (default agent/<id>)" },
                    "base": { "type": "string", "description": "Start-point for the worktree branch (default: the repo's default branch, fetched fresh from origin). Pass a feature branch (e.g. 'feat/x' or 'origin/feat/x') to deliberately stack this worktree on top of it. Ignored without worktree=true, and ignored when 'branch' already exists (the existing branch is checked out as-is)." },
                    "resume_session": { "type": "string", "description": "Session id to resume instead of starting fresh" },
                    "cwd": { "type": "string", "description": "Existing directory to run in (required with resume_session; use the original workspace)" },
                }),
                &["task"]),
            tool("send_prompt",
                "Type a prompt into an agent's CLI. The human sees it verbatim in that pane.",
                json!({
                    "agent_id": { "type": "string" },
                    "text": { "type": "string" },
                }),
                &["agent_id", "text"]),
            tool("get_output", "Read the last N lines of an agent's terminal (ANSI-stripped).",
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
                "Create (omit id, title required) or update a task on the shared board. status: queued | in-progress | review | pr | prototype | human-testing | done | blocked. Use `prototype` for a demo-gated draft the human will decide whether to promote — the board shows them a Proceed button, and clicking it prompts you to run the full production build. Keep the board current — it is the human's window into your queue. note appends a timestamped note.",
                json!({
                    "id": { "type": "string", "description": "Existing task id; omit to create" },
                    "title": { "type": "string" },
                    "status": { "type": "string", "enum": ["queued", "in-progress", "review", "pr", "prototype", "human-testing", "done", "blocked"] },
                    "issue": { "type": "string", "description": "GitHub issue ref, e.g. #12" },
                    "pr": { "type": "string", "description": "PR ref or URL" },
                    "assignee": { "type": "string", "description": "Agent id working on it" },
                    "session": { "type": "string", "description": "Worker session id for this task (enables follow-up resume)" },
                    "note": { "type": "string", "description": "Note to append" },
                }),
                &[]),
            tool("remove_task", "Delete a task from the shared board.",
                json!({ "id": { "type": "string" } }), &["id"]),
            tool("group_usage",
                "Aggregate the group's token usage and estimated dollar cost into one summary, split live vs lifetime (killed/recycled agents still count). Tokens come from each agent's session transcript and are exact; dollars are estimated from a model price table (subscription/Max accounts show $0 in the CLI, so cite tokens). Fold it into your status updates so the human sees spend at a glance.",
                json!({}), &[]),
        ]);
    } else {
        tools.extend([
            tool("report",
                "Report a status change to the orchestrator: progress | done | blocked. For done, include the PR URL.",
                json!({
                    "status": { "type": "string", "enum": ["progress", "done", "blocked"] },
                    "summary": { "type": "string" },
                }),
                &["status", "summary"]),
            tool("message_orchestrator", "Send a free-form message to the orchestrator.",
                json!({ "text": { "type": "string" } }), &["text"]),
        ]);
    }
    // Reviewers only: the verdict is the gate. Listed for the capability class, and
    // re-checked in `call_tool` — the listing is cosmetic, the dispatch check is the
    // enforcement (a worker that could file its own PASS would make the gate a prop).
    if role == Role::Reviewer {
        tools.push(tool("review_verdict",
            "Record your REVIEW OUTCOME for a pull request. This is durable, attributed state — not a notification — and when this repo's .loomux/workflow.yml declares a merge gate, it is what loomux's gh interceptor reads before allowing `gh pr merge`. Call it once you have finished reviewing, after posting your review on the PR, and then report() to the orchestrator as usual. verdict: `pass` (reviewed, nothing blocking), `fail` (blocking findings — fix and re-review), `escalate` (you will not decide this one: ambiguous requirement, out of your depth, a risk you won't sign off on — a human must look). fail and escalate BOTH refuse the merge, and one blocking verdict beats any number of passes, so never record `pass` to be agreeable or to unblock the queue. Your verdict is bound to the PR's CURRENT HEAD COMMIT: if the author pushes anything afterwards, your pass goes STALE and the gate reopens until you review the new commits and record again — so review the head as it stands, and expect to be asked again after a fix. Re-recording replaces your own earlier verdict (that is how you upgrade a `fail` to a `pass`, and how you refresh a stale one). The summary must stand on its own for a human reading it a week later: what you reviewed, and what decided the verdict. Verdict words are lowercase.",
            json!({
                "pr": { "type": "string", "description": "PR number, #n, or URL — the PR you reviewed." },
                "verdict": { "type": "string", "enum": ["pass", "fail", "escalate"], "description": "pass | fail | escalate, lowercase. Never guessed: an unrecognized value is rejected." },
                "summary": { "type": "string", "description": "Why. One or two lines a human can act on." },
            }),
            &["pr", "verdict", "summary"]));
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

fn call_tool(reg: &OrchRegistry, caller: &Caller, name: &str, args: &Value) -> Result<String, String> {
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
            let task = reg.upsert_task(
                &caller.group,
                &caller.agent_id,
                arg_str(args, "id"),
                super::TaskPatch {
                    title: arg_str(args, "title").map(str::to_string),
                    status: arg_str(args, "status").map(str::to_string),
                    issue: arg_str(args, "issue").map(str::to_string),
                    pr: arg_str(args, "pr").map(str::to_string),
                    assignee: arg_str(args, "assignee").map(str::to_string),
                    session: arg_str(args, "session").map(str::to_string),
                    note: arg_str(args, "note").map(str::to_string),
                },
            )?;
            Ok(format!("{} \"{}\" — {}", task.id, task.title, task.status))
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

        "spawn_agent" => {
            require_orchestrator(caller)?;
            // An unrecognized kind is REJECTED (#222). This used to be
            // `_ => Role::Worker` — so a typo'd or hallucinated kind silently
            // became a *worker*, complete with a worktree and write access. A
            // capability class is the one thing that must never be guessed.
            let kind = match arg_str(args, "kind") {
                None => Role::Worker, // documented default
                Some(k) => super::workflow::kind_from_str(k).ok_or_else(|| {
                    format!(
                        "unknown kind {k:?} — must be one of {}",
                        super::workflow::kind_names()
                    )
                })?,
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
            if kind == Role::Orchestrator {
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
            let worktree = args.get("worktree").and_then(Value::as_bool).unwrap_or(false);
            let branch = arg_str(args, "branch").map(str::to_string);
            let base = arg_str(args, "base").map(str::to_string);
            let resume = arg_str(args, "resume_session").map(str::to_string);
            let cwd = arg_str(args, "cwd").map(str::to_string);
            let resumed = resume.is_some();
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
            let block = if block.is_none() && arg_str(args, "kind").is_none() {
                match resume.as_deref() {
                    Some(session_id) => {
                        // A session can appear more than once (roster + audit
                        // backfill can both carry it, or it was re-spawned into
                        // a different block over its lifetime) — the
                        // last-touched record wins deliberately, since that is
                        // the agent's most recent identity, not its first one.
                        let owner = reg
                            .merged_records(&caller.group)
                            .into_iter()
                            .filter(|r| r.session.as_deref() == Some(session_id))
                            .max_by_key(|r| r.updated_ms)
                            .ok_or_else(|| {
                                format!(
                                    "unknown session {session_id:?} — cannot resume without an \
                                     explicit block or kind (no roster record maps this session \
                                     to one). Pass block (or kind) explicitly if you are sure of \
                                     its capability class."
                                )
                            })?;
                        let owner_block = if owner.block.trim().is_empty() {
                            // Pre-#222 roster row: only a role was ever recorded,
                            // no block identity — inherit that role's default
                            // block instead, since there is no block id to name.
                            let owner_role =
                                super::workflow::kind_from_str(&owner.role).unwrap_or(kind);
                            reg.group(&caller.group)
                                .and_then(|g| g.guardrails.block_for(owner_role).map(|b| b.id.clone()))
                                .ok_or_else(|| {
                                    format!(
                                        "this group's workflow declares no {} block",
                                        owner_role.as_str()
                                    )
                                })?
                        } else {
                            owner.block
                        };
                        Some(owner_block)
                    }
                    None => None,
                }
            } else {
                block
            };
            let a = reg.spawn_agent_ex(&caller.group, kind, block, name, task, worktree, branch, base, resume, cwd, None)?;
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
            Ok("state saved".into())
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

        "report" => {
            if caller.role == Role::Orchestrator {
                return Err("report is for workers/reviewers; use send_prompt".into());
            }
            let status = arg_str(args, "status").unwrap_or("progress");
            if !matches!(status, "progress" | "done" | "blocked") {
                return Err("status must be progress | done | blocked".into());
            }
            let summary = arg_str(args, "summary").ok_or("summary required")?;
            // A worker that finished (done) or stalled (blocked) is idle
            // again — restart its idle-kill clock; progress keeps it active.
            reg.set_agent_idle(&caller.agent_id, matches!(status, "done" | "blocked"));
            // Attention routing: a done/blocked report badges the pane (and can
            // toast) so the human sees which one needs them; progress clears it.
            reg.note_report_attention(&caller.agent_id, status);
            reg.deliver_to_orchestrator(
                &caller.group,
                &format!("[loomux] {} reports {status}: {summary}", caller.agent_id),
                &caller.agent_id,
            )?;
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
                    json!({
                        "pr": pr,
                        "verdicts": reg.verdicts(&caller.group, pr),
                        "gate": reg.gate_status_line(&caller.group, pr),
                    })
                })
                .collect();
            Ok(serde_json::to_string(&out).unwrap_or_default())
        }

        "message_orchestrator" => {
            if caller.role == Role::Orchestrator {
                return Err("you are the orchestrator".into());
            }
            let text = arg_str(args, "text").ok_or("text required")?;
            // A message is a sign of life: reset the watchdog's silence clock
            // (report already does this via set_agent_idle).
            reg.note_agent_activity(&caller.agent_id);
            reg.deliver_to_orchestrator(
                &caller.group,
                &format!("[loomux] message from {}: {text}", caller.agent_id),
                &caller.agent_id,
            )?;
            Ok("message delivered".into())
        }

        _ => Err(format!("unknown tool: {name}")),
    }
}
