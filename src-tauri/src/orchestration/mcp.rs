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

use super::{OrchRegistry, Caller, Role};
use serde_json::{json, Value};
use std::io::Read as _;
use std::sync::Arc;
use std::time::Duration;

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
    ];
    if role == Role::Orchestrator {
        tools.extend([
            tool("spawn_agent",
                "Open a new worker or reviewer agent pane in this group. Guardrails apply: live-agent cap and per-role pinned model. Set worktree=true for parallel work that must not collide; give branch a meaningful name either way. Empty task spawns an idle agent awaiting prompts.",
                json!({
                    "name": { "type": "string", "description": "Short display name for the pane" },
                    "kind": { "type": "string", "enum": ["worker", "reviewer"], "description": "Agent role (default worker)" },
                    "task": { "type": "string", "description": "Full task brief; empty = idle" },
                    "worktree": { "type": "boolean", "description": "Create a dedicated git worktree + branch" },
                    "branch": { "type": "string", "description": "Branch name (default agent/<id>)" },
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
            tool("set_state",
                "Persist the group's orchestration state (must be a valid JSON string). Call after every queue/plan change; this is your memory across sessions.",
                json!({ "state": { "type": "string" } }), &["state"]),
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

        "spawn_agent" => {
            require_orchestrator(caller)?;
            let kind = match arg_str(args, "kind").unwrap_or("worker") {
                "reviewer" => Role::Reviewer,
                _ => Role::Worker,
            };
            let task = arg_str(args, "task").unwrap_or("");
            let name = arg_str(args, "name").unwrap_or("");
            let worktree = args.get("worktree").and_then(Value::as_bool).unwrap_or(false);
            let branch = arg_str(args, "branch").map(str::to_string);
            let a = reg.spawn_agent(&caller.group, kind, name, task, worktree, branch)?;
            Ok(format!(
                "spawned {} (\"{}\", {:?}). Kickoff prompt delivered to its pane; it will report when ready.",
                a.id, a.name, a.role
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
            reg.deliver_prompt(&a.id, text, &caller.agent_id, Duration::ZERO)?;
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
        "set_state" => {
            require_orchestrator(caller)?;
            let state = arg_str(args, "state").ok_or("state required")?;
            reg.set_state(&caller.group, state)?;
            Ok("state saved".into())
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
            reg.deliver_to_orchestrator(
                &caller.group,
                &format!("[loomux] {} reports {status}: {summary}", caller.agent_id),
                &caller.agent_id,
            )?;
            Ok("reported to orchestrator".into())
        }
        "message_orchestrator" => {
            if caller.role == Role::Orchestrator {
                return Err("you are the orchestrator".into());
            }
            let text = arg_str(args, "text").ok_or("text required")?;
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
