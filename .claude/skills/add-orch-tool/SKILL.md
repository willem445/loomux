---
name: add-orch-tool
description: Add or change an MCP orchestration tool (the tools loomux exposes to orchestrator/worker/reviewer/planner agents) — every layer that must move together.
---

# Adding an MCP orchestration tool

Loomux's orchestration agents talk to a local MCP server
(`src-tauri/src/orchestration/mcp.rs`) backed by `OrchRegistry`
(`src-tauri/src/orchestration/mod.rs`). A new tool touches up to six places;
missing one produces a tool that lists but doesn't dispatch, or works but is
invisible to agents and auditors.

## Checklist

1. **Definition — `tool_defs(role)` in `mcp.rs`.** Use the `tool(name, desc,
   props, required)` helper. The list is **role-filtered**: orchestrator-only
   tools go in the `if role == Role::Orchestrator` block, worker/reviewer
   tools in the `else`. Write the description for the *agent* reading it —
   existing descriptions state semantics, defaults, and when to call it; match
   that depth.

2. **Dispatch — `call_tool()` in `mcp.rs`.** Add the match arm. Re-enforce
   authorization here with `require_orchestrator(caller)` /
   `require_in_group(...)` — the role-filtered listing is *cosmetic, not
   security*; the dispatch check is the real gate. Never leak other groups'
   agent ids in errors (mimic the "unknown agent" wording).

3. **Registry logic — a method on `OrchRegistry` in `mod.rs`.** Keep mcp.rs a
   thin JSON shim; state changes live in the registry. If the action matters
   to a human reconstructing a run, write an audit line via
   `self.audit(group, actor, action, detail_json)` — prompts, spawns, task
   edits, and state writes are all audited today.

4. **Role instructions — `src-tauri/src/orchestration/templates/*.md`.**
   Agents only call tools their instructions teach. Update the templates for
   every role that can see the tool (orchestrator.md, worker.md, reviewer.md,
   planner.md — they're `include_str!`'d, so a rebuild picks them up).

5. **Tests — `src-tauri/tests/orchestration.rs`** (must stay an integration
   test — see CLAUDE.md constraint 4). Drive the real `dispatch()` with a
   `Caller` of each relevant role and assert: the happy path, the
   wrong-role rejection, and the cross-group rejection. Never spawn a real
   agent CLI.

6. **Docs — README "Agent orchestration" section** if the tool changes what a
   human sees (new pane behavior, board fields, audit rows). Deeper design
   rationale goes in `doc/design/orchestration.md`.

## Design norms to preserve

- **Visible prompts:** anything that sends text to an agent goes through the
  serialized PTY delivery path so the human sees it verbatim in the pane —
  never invent a side channel.
- **Guardrails in the platform:** caps, pinned models, and isolation are
  enforced in Rust; agent judgment stays in the instruction templates. Don't
  move an enforcement into a prompt.
- **Frontend round-trips:** if the tool needs a pane action, follow the
  spawn pattern — emit an event, wait on a bound response with a deadline,
  and handle the expiry/cancel/late-bind orderings (#106 is the case study).
