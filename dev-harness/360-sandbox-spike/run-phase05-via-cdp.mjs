// Phase-0.5 spike for #360 — validates Option B (child WebviewWindow bound to
// a dedicated, deliberately-empty capability) as the pane-plugin trust core.
// Throwaway dev-harness tooling — does not ship.
//
// Prerequisites (see README.md "Phase-0.5" section for the full setup this
// script assumes is already built and running):
//   - src-tauri/build.rs uses tauri_build::Attributes::new().app_manifest(...)
//     to give the app's own commands a real ACL manifest (has_app_acl_manifest
//     = true) — required for capability-based per-window denial to do
//     anything at all for loomux's app-defined commands.
//   - capabilities/spike-plugin-zero.json binds window label "spike-plugin"
//     to an empty permissions array.
//   - capabilities/default.json explicitly grants "main" the 6 spike-relevant
//     commands (see build.rs's AppManifest.commands list).
//   - `npm run tauri dev` (or the built exe) running with
//     WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222"
//     and a dedicated WEBVIEW2_USER_DATA_FOLDER (a second instance sharing
//     the app's default profile silently ignores the remote-debugging flag).
//
// Usage: node run-phase05-via-cdp.mjs
async function cdpConnect(target) {
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  let id = 0;
  const pending = new Map();
  function send(method, params) {
    const msgId = ++id;
    return new Promise((resolve, reject) => {
      pending.set(msgId, { resolve, reject });
      ws.send(JSON.stringify({ id: msgId, method, params }));
    });
  }
  await new Promise((resolve, reject) => {
    ws.addEventListener("open", () => resolve());
    ws.addEventListener("error", (e) => reject(e));
  });
  ws.addEventListener("message", (event) => {
    const msg = JSON.parse(event.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(JSON.stringify(msg.error)));
      else resolve(msg.result);
    }
  });
  await send("Runtime.enable", {});
  return { ws, send };
}

async function evalExpr(conn, expression, timeout = 10000) {
  const r = await conn.send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
    timeout,
  });
  if (r.exceptionDetails) {
    throw new Error("evaluation threw: " + JSON.stringify(r.exceptionDetails));
  }
  return r.result.value;
}

async function listTargets() {
  const res = await fetch("http://127.0.0.1:9222/json");
  return res.json();
}

async function main() {
  const targets = await listTargets();
  const mainTarget = targets.find((t) => t.url === "http://localhost:1420/");
  if (!mainTarget) throw new Error("main window target not found — is the app running with CDP enabled?");

  console.error("Opening the spike-plugin child window from the trusted main frame...");
  const mainConn = await cdpConnect(mainTarget);
  const openResult = await evalExpr(
    mainConn,
    `window.__TAURI_INTERNALS__.invoke("spike_open_plugin_window", {}).then(() => "OK").catch((e) => "ERR:" + (e.message || e))`
  );
  console.error("open result:", openResult);
  mainConn.ws.close();
  if (openResult !== "OK") {
    throw new Error("failed to open spike-plugin window: " + openResult);
  }

  await new Promise((r) => setTimeout(r, 1500));

  const targets2 = await listTargets();
  const pluginTarget = targets2.find((t) => t.url.includes("plugin-window-adversary"));
  if (!pluginTarget) throw new Error("spike-plugin window target not found after creation");

  console.error("Attaching to spike-plugin window and waiting for its self-test to finish...");
  const pluginConn = await cdpConnect(pluginTarget);
  const results = await evalExpr(
    pluginConn,
    `new Promise((resolve) => { const check = () => { if (window.__spikeResults__) resolve(window.__spikeResults__); else setTimeout(check, 300); }; check(); })`,
    15000
  );
  console.log(JSON.stringify(results, null, 2));
  pluginConn.ws.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
