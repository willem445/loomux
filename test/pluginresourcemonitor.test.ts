// Unit tests for the bundled resource-monitor example plugin's pure
// formatting/sorting core (#360 Slice F). DOM-free by construction (no
// `window`/`document`), so this exercises the exact module the plugin's
// `main.js` imports at `plugin://localhost/resource-monitor/format.js` — the
// plugin's own DOM/broker wiring is hand-validated (it runs inside a
// sandboxed `plugin-*` child webview CI has no way to drive). Run with
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  formatBytes,
  formatPercent,
  sortProcesses,
  normalizeSortPref,
  DEFAULT_SORT,
  SORT_COLUMNS,
} from "../src-tauri/resources/plugins/resource-monitor/format.js";

function proc(pid: number, name: string, cpu: number, rss: number) {
  return { pid, name, cpu_percent: cpu, rss_bytes: rss };
}

test("formatBytes renders 0 and sub-KB values in plain bytes", () => {
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(512), "512 B");
});

test("formatBytes crosses unit boundaries at 1024, with one decimal above B", () => {
  assert.equal(formatBytes(1024), "1.0 KB");
  assert.equal(formatBytes(1536), "1.5 KB");
  assert.equal(formatBytes(1024 * 1024), "1.0 MB");
  assert.equal(formatBytes(1024 * 1024 * 1024 * 2.5), "2.5 GB");
});

test("formatBytes clamps negative/non-finite input to zero rather than throwing", () => {
  assert.equal(formatBytes(-5), "0 B");
  assert.equal(formatBytes(NaN), "0 B");
  assert.equal(formatBytes(Infinity), "0 B");
});

test("formatPercent keeps one decimal and does not cap above 100", () => {
  // Real, expected data: sysinfo's per-process cpu_usage() is normalized
  // against a single core, so a busy multi-threaded process can exceed
  // 100% on a multi-core machine — the display must not lie about that.
  assert.equal(formatPercent(0), "0.0%");
  assert.equal(formatPercent(12.34), "12.3%");
  assert.equal(formatPercent(230.5), "230.5%");
});

test("formatPercent clamps negative/non-finite input to zero", () => {
  assert.equal(formatPercent(-1), "0.0%");
  assert.equal(formatPercent(NaN), "0.0%");
});

test("sortProcesses sorts by cpu descending by default direction", () => {
  const input = [proc(1, "low", 5, 100), proc(2, "high", 90, 100), proc(3, "mid", 40, 100)];
  const sorted = sortProcesses(input, "cpu", "desc");
  assert.deepEqual(
    sorted.map((p) => p.pid),
    [2, 3, 1]
  );
});

test("sortProcesses reverses order when direction is asc", () => {
  const input = [proc(1, "low", 5, 100), proc(2, "high", 90, 100), proc(3, "mid", 40, 100)];
  const sorted = sortProcesses(input, "cpu", "asc");
  assert.deepEqual(
    sorted.map((p) => p.pid),
    [1, 3, 2]
  );
});

test("sortProcesses breaks ties by pid ascending regardless of direction", () => {
  const input = [proc(30, "a", 10, 0), proc(10, "b", 10, 0), proc(20, "c", 10, 0)];
  const desc = sortProcesses(input, "cpu", "desc");
  const asc = sortProcesses(input, "cpu", "asc");
  assert.deepEqual(
    desc.map((p) => p.pid),
    [10, 20, 30]
  );
  assert.deepEqual(
    asc.map((p) => p.pid),
    [10, 20, 30]
  );
});

test("sortProcesses sorts by name and by rss too", () => {
  const input = [proc(1, "beta", 0, 100), proc(2, "alpha", 0, 300), proc(3, "gamma", 0, 200)];
  assert.deepEqual(
    sortProcesses(input, "name", "asc").map((p) => p.name),
    ["alpha", "beta", "gamma"]
  );
  assert.deepEqual(
    sortProcesses(input, "rss", "desc").map((p) => p.pid),
    [2, 3, 1]
  );
});

test("sortProcesses never mutates its input array", () => {
  const input = [proc(1, "b", 1, 0), proc(2, "a", 2, 0)];
  const snapshot = [...input];
  sortProcesses(input, "cpu", "asc");
  assert.deepEqual(input, snapshot);
});

test("normalizeSortPref accepts a valid stored preference", () => {
  assert.deepEqual(normalizeSortPref({ column: "name", direction: "asc" }), {
    column: "name",
    direction: "asc",
  });
});

test("normalizeSortPref falls back to the default on missing/malformed/unknown-column input", () => {
  assert.deepEqual(normalizeSortPref(null), DEFAULT_SORT);
  assert.deepEqual(normalizeSortPref(undefined), DEFAULT_SORT);
  assert.deepEqual(normalizeSortPref("not-an-object"), DEFAULT_SORT);
  assert.deepEqual(normalizeSortPref({ column: "cmdline", direction: "asc" }), {
    column: DEFAULT_SORT.column,
    direction: "asc",
  });
});

test("SORT_COLUMNS matches the curated metrics.system payload shape", () => {
  assert.deepEqual([...SORT_COLUMNS].sort(), ["cpu", "name", "pid", "rss"]);
});
