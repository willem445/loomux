// The launch → starter-workers wire contract (#1020 item 5). Run with `npm test`.
//
// WHY THIS IS A SOURCE SCAN, AND WHY IT IS HERE AT ALL.
//
// Item 5's promise is that a launch opens NO worker panes. `starter_workers` pins the
// decision (src-tauri/tests/orchestration.rs) and that is the half a unit test can reach.
// The half it cannot is the one that makes the decision *apply*: the launcher expresses
// "no starters" by OMITTING the `initialWorkers` key, which is only 0 because the backend
// argument is `Option<u32>` and tauri maps a missing key to `None`. That contract spans the
// IPC boundary — no test in this repo crosses it (rev-740 verified the tauri-side behaviour
// by reading `tauri/src/ipc/command.rs`), so nothing else would notice it breaking.
//
// And it breaks SILENTLY in the direction that matters. Change the argument back to a plain
// `u32` and every suite stays green — `cargo test` never invokes the command, and the
// frontend never type-checks against Rust — while the first real launch fails at the IPC
// layer with an `InvalidArgs` error for a missing field. That is a defect no reviewer would
// see in a diff that "just tightened a type".
//
// **The behavioural test this replaces could not exist, and finding that out is the point.**
// The spawn loop that opens starters runs in `register_orchestrator_pane`'s post-bind
// thread, and that thread is never started headlessly: `if reg.app.lock_safe().is_none() {
// … return Ok(request) }` returns before it (mod.rs), because a bind needs a frontend pane.
// A test asserting "no workers appeared" after a headless launch is therefore VACUOUS — it
// passes whatever the default is, including a re-introduced 2. Driving it needs a real
// `AppHandle` (`set_app`), which an integration test cannot construct. So this pins what is
// actually pinnable: the shape that carries the decision across the boundary.
//
// It is a TEXTUAL scan and enumerates its own limits, the way the groupid scans do: it
// matches the argument declaration and the invoke payload as written today. A macro-built
// argument list, an aliased type, or a payload assembled from a spread would slip past it.
// None exists today — don't be the first.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (rel: string): string => readFileSync(new URL(rel, import.meta.url), "utf8");

/** Rust line comments, so a `//` mention of a symbol never satisfies a scan for it. */
function stripRustComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

/** TS line + block comments, same reason. */
function stripTsComments(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

test("create_orchestration's starter count stays OPTIONAL, so an omitted key is legal", () => {
  // The load-bearing half. `Option<u32>` is what makes the launcher's silence mean "none";
  // a plain `u32` turns that same silence into a runtime InvalidArgs on the first launch.
  const rs = stripRustComments(read("../src-tauri/src/orchestration/mod.rs"));
  const command = rs.match(/pub async fn create_orchestration\(([\s\S]*?)\)\s*->/);
  assert.ok(command, "create_orchestration is no longer an async command with an argument list");
  const arg = command[1].match(/\binitial_workers\s*:\s*([^,]+),/);
  assert.ok(arg, "create_orchestration no longer takes an initial_workers argument at all");
  assert.equal(
    arg[1].trim(),
    "Option<u32>",
    "initial_workers must stay Option<u32>: the launcher sends no key, and only an optional " +
      "argument reads a missing key as None (→ 0 starters). A plain u32 fails at the IPC layer."
  );
});

test("the launcher sends no starter count — the omission IS the request for zero", () => {
  const ts = stripTsComments(read("../src/orchestration.ts"));
  const call = ts.match(/invoke<OrchSpawnRequest>\(\s*"create_orchestration",\s*\{([\s\S]*?)\n\s*\}\);/);
  assert.ok(call, "launchOrchestrator no longer invokes create_orchestration with an object literal");
  assert.equal(
    /\binitialWorkers\b/.test(call[1]),
    false,
    "the create_orchestration payload carries an initialWorkers key again — a launch must send " +
      "none, so the backend's Option resolves to 0 rather than to whatever number the form guessed"
  );
  // The config type must not carry it either: a field nothing reads is the seam a future
  // caller re-populates without noticing there is nowhere for it to go.
  assert.equal(
    /\binitialWorkers\b/.test(stripTsComments(read("../src/orchestration.ts"))),
    false,
    "OrchestratorConfig still declares initialWorkers"
  );
});

test("the setup form offers no starter-worker control", () => {
  // The other direction of the same rule: the field is gone, not merely unsent. A form that
  // still collected a number and dropped it on the floor would be a control that silently
  // does nothing — worse than either shipping it or removing it.
  const launcher = stripTsComments(read("../src/launcher.ts"));
  assert.equal(/workersInput/.test(launcher), false, "the launcher still holds a starter-worker input");
  assert.equal(
    /Initial workers/i.test(launcher),
    false,
    "the launcher still renders an 'Initial workers' field label"
  );
});
