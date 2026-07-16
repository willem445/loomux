// Drives the sandbox-spike harness over the Chrome DevTools Protocol against a
// running `npm run tauri dev` instance (started with
// WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--remote-debugging-port=9222").
// Throwaway dev-harness tooling — does not ship. Usage: node run-via-cdp.mjs
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const dir = path.dirname(fileURLToPath(import.meta.url));
const harnessSrc = readFileSync(path.join(dir, "harness.js"), "utf8");

async function main() {
  const listRes = await fetch("http://127.0.0.1:9222/json");
  const targets = await listRes.json();
  const page = targets.find((t) => t.type === "page") ?? targets[0];
  if (!page) {
    console.error("No CDP targets found:", targets);
    process.exit(1);
  }
  console.error("Attaching to target:", page.title, page.url);

  const ws = new WebSocket(page.webSocketDebuggerUrl);
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

  // Wrap the harness so Runtime.evaluate can await its results directly instead
  // of us needing a second round-trip to read window.__spikeResults__.
  const wrapped = `
    (async () => {
      ${harnessSrc}
      await new Promise((r) => setTimeout(r, 3500));
      return window.__spikeResults__ || [];
    })()
  `;

  const evalResult = await send("Runtime.evaluate", {
    expression: wrapped,
    awaitPromise: true,
    returnByValue: true,
    timeout: 15000,
  });

  if (evalResult.exceptionDetails) {
    console.error("Evaluation threw:", JSON.stringify(evalResult.exceptionDetails, null, 2));
    process.exit(1);
  }

  const results = evalResult.result.value;
  console.log(JSON.stringify(results, null, 2));
  ws.close();
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
