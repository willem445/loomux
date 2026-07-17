// #360 multiwebview-embedding spike — throwaway, does not ship. Mirrors
// spike/360-sandbox-proof's run-phase05-via-cdp.mjs wiring.
//
// Prerequisites:
//   - src-tauri/Cargo.toml has `tauri = { version = "2", features = ["unstable"] }`
//     (spike-only; see that file's comment).
//   - `npm run tauri dev` (or the built exe) running with
//     WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222"
//     and a dedicated WEBVIEW2_USER_DATA_FOLDER.
//
// Usage: node run-multiwebview-via-cdp.mjs
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
  const res = await fetch("http://127.0.0.1:9333/json");
  return res.json();
}

async function main() {
  const targets = await listTargets();
  const mainTarget = targets.find((t) => t.url === "http://localhost:1420/");
  if (!mainTarget) throw new Error("main window target not found — is the app running with CDP enabled?");

  console.error("Opening the spike-child webview from the trusted main frame...");
  const mainConn = await cdpConnect(mainTarget);
  const openResult = await evalExpr(
    mainConn,
    `window.__TAURI_INTERNALS__.invoke("spike_open_child_webview", {}).then(() => "OK").catch((e) => "ERR:" + (e.message || e))`
  );
  console.error("open result:", openResult);
  mainConn.ws.close();
  if (openResult !== "OK") {
    throw new Error("failed to open spike-child webview: " + openResult);
  }

  await new Promise((r) => setTimeout(r, 1500));

  const targets2 = await listTargets();
  const childTarget = targets2.find((t) => t.url.includes("child-webview-adversary"));
  if (!childTarget) throw new Error("spike-child webview target not found after creation — did it get its own CDP target?");

  console.error("Found child webview CDP target:", childTarget.url, "(confirms it is a separate WebView2 controller, not a subframe of main's document)");
  console.error("Attaching and waiting for its self-test to finish...");
  const childConn = await cdpConnect(childTarget);
  const results = await evalExpr(
    childConn,
    `new Promise((resolve) => { const check = () => { if (window.__spikeResults__) resolve(window.__spikeResults__); else setTimeout(check, 300); }; check(); })`,
    15000
  );
  console.log(JSON.stringify(results, null, 2));
  childConn.ws.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
