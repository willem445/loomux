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
            "Read the group's task board (JSON array, order = priority). The human sees and edits this same board.",
            json!({}), &[]),
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
                "List your OWN live notifications (id, kind, target, note, registered/expiry times). Notifications do not survive a loomux restart, and this is your only memory of what you registered — call it on session start and after a /compact, and re-register anything you were still waiting on.",
                json!({}), &[]),
            tool("cancel_notification",
                "Cancel one of your own live notifications by id (e.g. because the PR it watched got closed).",
                json!({ "id": { "type": "string" } }), &["id"]),
        ]);
    }
    if role == Role::Orchestrator {
        tools.extend([
            tool("spawn_agent",
                "Open a new worker, reviewer, or planner agent pane in this group. Guardrails apply: live-agent cap and per-role pinned CLI + model. Set worktree=true for parallel work that must not collide; give branch a meaningful name either way. Empty task spawns an idle agent awaiting prompts. A planner explores the codebase read-only and writes an implementation plan as a GitHub issue comment, then reports and exits. Its read-only contract is enforced structurally where the CLI allows it — it never gets a worktree, and its file-editing tools plus git commit/push are denied at the CLI level — so it cannot edit files or push code; not opening PRs is asked of it in its instructions (gh stays available so it can post the plan comment). For a FOLLOW-UP on a finished task, pass resume_session (from list_agents/the task board) plus cwd (where that work happened) — the pane reopens that conversation with its context instead of cold-starting.",
                json!({
                    "name": { "type": "string", "description": "Short display name for the pane" },
                    "kind": { "type": "string", "enum": ["worker", "reviewer", "planner"], "description": "Agent role (default worker)" },
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
        "list_tasks" => Ok(serde_json::to_string(&reg.tasks(&caller.group)).unwrap_or_default()),

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
            let kind = match arg_str(args, "kind").unwrap_or("worker") {
                "reviewer" => Role::Reviewer,
                "planner" => Role::Planner,
                _ => Role::Worker,
            };
            let task = arg_str(args, "task").unwrap_or("");
            let name = arg_str(args, "name").unwrap_or("");
            let worktree = args.get("worktree").and_then(Value::as_bool).unwrap_or(false);
            let branch = arg_str(args, "branch").map(str::to_string);
            let base = arg_str(args, "base").map(str::to_string);
            let resume = arg_str(args, "resume_session").map(str::to_string);
            let cwd = arg_str(args, "cwd").map(str::to_string);
            let resumed = resume.is_some();
            let a = reg.spawn_agent_ex(&caller.group, kind, name, task, worktree, branch, base, resume, cwd, None)?;
            // Copilot mints its session id a few seconds into boot; loomux
            // binds it to the pane once it appears (visible then in
            // list_agents / the task board).
            let session = a
                .session_id
                .as_deref()
                .map(|s| format!("Session {s}."))
                .unwrap_or_else(|| "Session id will appear in list_agents once Copilot initializes.".into());
            Ok(format!(
                "spawned {} (\"{}\", {:?}){}. {} It will report when ready.",
                a.id,
                a.name,
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
                    let run = super::pr_number(raw)
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
            let requested = args.get("expires_minutes").and_then(Value::as_u64).map(|n| n as u32);
            let expires_minutes = super::notify::clamp_expires_minutes(requested);
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
