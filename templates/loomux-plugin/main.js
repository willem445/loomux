// The template's "hello world" — one broker round trip, using the `storage`
// capability declared in plugin.json (no root/fs.read needed, so this works
// for a `rootless: true` plugin too — see docs/features/pane-plugins.md for
// what each capability needs).
import { createPluginClient } from "./sdk/plugin-sdk.js";

// apiVersion must match plugin.json's own `apiVersion` field.
const client = createPluginClient({ apiVersion: 1 });

const statusEl = document.getElementById("status");
const bumpEl = document.getElementById("bump");

async function bump() {
  const previous = await client.request("storage.get", { key: "visits" });
  const visits = (typeof previous === "number" ? previous : 0) + 1;
  await client.request("storage.set", { key: "visits", value: visits });
  statusEl.textContent = `Storage round-trip OK — this plugin has been opened ${visits} time(s).`;
  bumpEl.disabled = false;
}

bumpEl.addEventListener("click", () => {
  bumpEl.disabled = true;
  void bump().catch(showError);
});

function showError(err) {
  statusEl.textContent = `Broker request failed: ${err && err.code ? err.code : "error"} — ${err && err.message ? err.message : String(err)}`;
}

void bump().catch(showError);
