// DOM + broker wiring for the bundled resource-monitor example plugin
// (#360 Slice F). Hand-validated, not unit-tested — it runs inside a
// sandboxed `plugin-*` WebviewWindow CI has no way to drive (CLAUDE.md's
// convention: DOM wiring is validated by hand, pure logic is in format.js).
//
// Talks to the host ONLY through the two commands this window's ACL grant
// allows (`plugin_broker_request`, `plugin_broker_open_channel`) — the raw
// broker API from pluginprotocol.ts/pluginbroker.rs, not a plugin SDK (#360
// Slice G is building one, in parallel; this plugin deliberately doesn't
// depend on it, per this slice's brief).

import { invoke, Channel } from "./tauri-ipc.js";
import { formatBytes, formatPercent, sortProcesses, normalizeSortPref, DEFAULT_SORT } from "./format.js";

const API_VERSION = 1;
const STORAGE_SORT_KEY = "sortPref";
const INTERVAL_OPTIONS = [1000, 2000, 3000, 5000, 10000];
const DEFAULT_INTERVAL_MS = 2000;
const COLUMN_LABELS = { name: "Process", pid: "PID", cpu: "CPU", rss: "RAM" };

let requestSeq = 0;
function nextRequestId() {
  requestSeq += 1;
  return `req-${requestSeq}`;
}

/** One round trip through the broker (design note's envelope: id/apiVersion/
 *  method/params in, ok/result/error out). Throws on `ok: false` so callers
 *  can `.catch()` rather than re-checking `resp.ok` everywhere. */
async function brokerRequest(method, params) {
  const resp = await invoke("plugin_broker_request", {
    request: { id: nextRequestId(), apiVersion: API_VERSION, method, params: params ?? null },
  });
  if (!resp.ok) {
    const err = new Error((resp.error && resp.error.message) || `${method} failed`);
    err.code = (resp.error && resp.error.code) || "unknown";
    throw err;
  }
  return resp.result;
}

function describeError(err) {
  const code = err && err.code ? ` (${err.code})` : "";
  const message = (err && err.message) || String(err);
  return `${message}${code}`;
}

class ResourceMonitorView {
  constructor(root) {
    this.root = root;
    this.sort = { ...DEFAULT_SORT };
    this.intervalMs = DEFAULT_INTERVAL_MS;
    this.paused = false;
    this.snapshot = null;
    this.subscribed = false;
    this.headerCells = {};
    this.buildDom();
  }

  buildDom() {
    this.root.innerHTML = "";

    const header = document.createElement("div");
    header.className = "rm-header";

    const title = document.createElement("div");
    title.className = "rm-title";
    title.textContent = "Resource Monitor";
    header.appendChild(title);

    this.summaryEl = document.createElement("div");
    this.summaryEl.className = "rm-summary";
    this.summaryEl.textContent = "Connecting…";
    header.appendChild(this.summaryEl);

    const controls = document.createElement("div");
    controls.className = "rm-controls";

    this.pauseBtn = document.createElement("button");
    this.pauseBtn.type = "button";
    this.pauseBtn.className = "rm-btn";
    this.pauseBtn.textContent = "Pause";
    this.pauseBtn.addEventListener("click", () => this.togglePause());
    controls.appendChild(this.pauseBtn);

    const intervalLabel = document.createElement("label");
    intervalLabel.className = "rm-interval-label";
    intervalLabel.textContent = "Every";
    this.intervalSel = document.createElement("select");
    this.intervalSel.className = "rm-select";
    for (const ms of INTERVAL_OPTIONS) {
      const opt = document.createElement("option");
      opt.value = String(ms);
      opt.textContent = ms < 1000 ? `${ms}ms` : `${ms / 1000}s`;
      this.intervalSel.appendChild(opt);
    }
    this.intervalSel.value = String(this.intervalMs);
    this.intervalSel.addEventListener("change", () => {
      void this.setInterval(Number(this.intervalSel.value));
    });
    intervalLabel.appendChild(this.intervalSel);
    controls.appendChild(intervalLabel);

    header.appendChild(controls);
    this.root.appendChild(header);

    this.statusEl = document.createElement("div");
    this.statusEl.className = "rm-status";
    this.statusEl.hidden = true;
    this.root.appendChild(this.statusEl);

    const tableWrap = document.createElement("div");
    tableWrap.className = "rm-table-wrap";
    const table = document.createElement("table");
    table.className = "rm-table";
    const thead = document.createElement("thead");
    const headRow = document.createElement("tr");
    for (const col of ["name", "pid", "cpu", "rss"]) {
      const th = document.createElement("th");
      th.className = "rm-sortable";
      th.tabIndex = 0;
      const label = document.createElement("span");
      label.textContent = COLUMN_LABELS[col];
      const arrow = document.createElement("span");
      arrow.className = "rm-sort-arrow";
      th.appendChild(label);
      th.appendChild(arrow);
      th.addEventListener("click", () => this.setSort(col));
      th.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          this.setSort(col);
        }
      });
      this.headerCells[col] = { th, arrow };
      headRow.appendChild(th);
    }
    thead.appendChild(headRow);
    table.appendChild(thead);
    this.tbody = document.createElement("tbody");
    table.appendChild(this.tbody);
    tableWrap.appendChild(table);
    this.root.appendChild(tableWrap);

    this.footerEl = document.createElement("div");
    this.footerEl.className = "rm-footer";
    this.footerEl.textContent =
      "Curated snapshot: name, PID, CPU%, RAM — top 32 processes by CPU, no command lines or paths.";
    this.root.appendChild(this.footerEl);

    this.syncSortHeaders();
  }

  setStatus(message) {
    if (!message) {
      this.statusEl.hidden = true;
      this.statusEl.textContent = "";
      this.statusEl.classList.remove("rm-status-error");
      return;
    }
    this.statusEl.hidden = false;
    this.statusEl.textContent = message;
  }

  setError(message) {
    this.statusEl.hidden = false;
    this.statusEl.textContent = message;
    this.statusEl.classList.add("rm-status-error");
  }

  syncSortHeaders() {
    for (const [col, { th, arrow }] of Object.entries(this.headerCells)) {
      const active = col === this.sort.column;
      th.classList.toggle("rm-sort-active", active);
      arrow.textContent = active ? (this.sort.direction === "asc" ? "▲" : "▼") : "";
    }
  }

  async start() {
    this.setStatus("Connecting…");
    try {
      const storedPref = await brokerRequest("storage.get", { key: STORAGE_SORT_KEY });
      this.sort = normalizeSortPref(storedPref);
    } catch {
      // `storage` is a nice-to-have (persists the sort column across
      // reopens) — a denial or read failure just keeps the built-in
      // default sort, it must not block the metrics view from starting.
    }
    this.syncSortHeaders();

    const channel = new Channel((evt) => this.onEvent(evt));
    await invoke("plugin_broker_open_channel", { channel });
    await this.subscribe();
  }

  async subscribe() {
    await brokerRequest("metrics.subscribe", { intervalMs: this.intervalMs });
    this.subscribed = true;
    this.setStatus(this.snapshot ? null : "Waiting for first sample…");
  }

  onEvent(evt) {
    if (!evt || evt.event !== "metrics.tick") return;
    this.snapshot = evt.payload;
    this.setStatus(null);
    this.render();
  }

  async setInterval(ms) {
    this.intervalMs = ms;
    if (this.paused) return;
    try {
      await this.subscribe();
    } catch (err) {
      this.setError(`Couldn't change interval: ${describeError(err)}`);
    }
  }

  async togglePause() {
    this.paused = !this.paused;
    this.pauseBtn.textContent = this.paused ? "Resume" : "Pause";
    this.intervalSel.disabled = this.paused;
    try {
      if (this.paused) {
        this.subscribed = false;
        await brokerRequest("metrics.unsubscribe");
        this.setStatus("Paused");
      } else {
        await this.subscribe();
      }
    } catch (err) {
      this.setError(`${this.paused ? "Pause" : "Resume"} failed: ${describeError(err)}`);
    }
  }

  setSort(column) {
    if (this.sort.column === column) {
      this.sort = { column, direction: this.sort.direction === "asc" ? "desc" : "asc" };
    } else {
      this.sort = { column, direction: column === "name" ? "asc" : "desc" };
    }
    this.syncSortHeaders();
    this.render();
    void brokerRequest("storage.set", { key: STORAGE_SORT_KEY, value: this.sort }).catch(() => {
      // Best-effort persistence only — a plugin whose manifest never
      // granted `storage`, or a write that races a pane teardown, must not
      // interrupt the (already-rendered) sort the human just clicked.
    });
  }

  render() {
    if (!this.snapshot) return;
    this.summaryEl.textContent =
      `CPU ${formatPercent(this.snapshot.cpu_percent)} · ` +
      `RAM ${formatBytes(this.snapshot.mem_used_bytes)} / ${formatBytes(this.snapshot.mem_total_bytes)}`;

    const rows = sortProcesses(this.snapshot.processes || [], this.sort.column, this.sort.direction);
    this.tbody.innerHTML = "";
    if (rows.length === 0) {
      const tr = document.createElement("tr");
      const td = document.createElement("td");
      td.colSpan = 4;
      td.className = "rm-empty";
      td.textContent = "No process data reported this tick.";
      tr.appendChild(td);
      this.tbody.appendChild(tr);
      return;
    }
    for (const proc of rows) {
      const tr = document.createElement("tr");

      const nameTd = document.createElement("td");
      nameTd.className = "rm-name";
      nameTd.textContent = proc.name;
      nameTd.title = proc.name;
      tr.appendChild(nameTd);

      const pidTd = document.createElement("td");
      pidTd.className = "rm-num";
      pidTd.textContent = String(proc.pid);
      tr.appendChild(pidTd);

      const cpuTd = document.createElement("td");
      cpuTd.className = "rm-num rm-cpu";
      const cpuBar = document.createElement("div");
      cpuBar.className = "rm-bar";
      const cpuFill = document.createElement("div");
      cpuFill.className = "rm-bar-fill";
      cpuFill.style.width = `${Math.max(0, Math.min(100, proc.cpu_percent))}%`;
      if (proc.cpu_percent >= 75) cpuFill.classList.add("rm-bar-hot");
      else if (proc.cpu_percent >= 35) cpuFill.classList.add("rm-bar-warm");
      cpuBar.appendChild(cpuFill);
      const cpuLabel = document.createElement("span");
      cpuLabel.className = "rm-cpu-label";
      cpuLabel.textContent = formatPercent(proc.cpu_percent);
      cpuTd.appendChild(cpuBar);
      cpuTd.appendChild(cpuLabel);
      tr.appendChild(cpuTd);

      const rssTd = document.createElement("td");
      rssTd.className = "rm-num";
      rssTd.textContent = formatBytes(proc.rss_bytes);
      tr.appendChild(rssTd);

      this.tbody.appendChild(tr);
    }
  }
}

async function main() {
  const root = document.getElementById("app");
  const view = new ResourceMonitorView(root);
  try {
    await view.start();
  } catch (err) {
    view.setError(`Couldn't start the resource monitor: ${describeError(err)}`);
  }

  // Best-effort: stop the host's poll thread promptly on close rather than
  // waiting on the window-destroyed cleanup hook alone (procmetrics.rs stops
  // it either way — this just makes the common case tidy sooner).
  window.addEventListener("pagehide", () => {
    if (view.subscribed) void brokerRequest("metrics.unsubscribe").catch(() => {});
  });
}

void main();
