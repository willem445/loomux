// Pure, DOM-free formatting/sorting for the bundled resource-monitor example
// plugin (#360 Slice F). No `window`/`document` reference anywhere in this
// file so it can be unit-tested directly with node:test
// (test/pluginresourcemonitor.test.ts) the same way the host app's own
// DOM-free modules are (CLAUDE.md's "extract testable logic" convention) —
// main.js is the DOM/broker wiring that imports this and is hand-validated.

/** The columns the table can sort by — matches the curated per-process
 *  payload `metrics.system` delivers (name/pid/cpu%/rss, see
 *  `procmetrics::ProcessSample`), nothing more. */
export const SORT_COLUMNS = Object.freeze(["name", "pid", "cpu", "rss"]);

export const DEFAULT_SORT = Object.freeze({ column: "cpu", direction: "desc" });

/** Binary-unit byte formatting (1024-based, labelled KB/MB/GB/TB — the same
 *  convention Windows' own Task Manager uses). Negative/non-finite input
 *  (should never happen from a real `rss_bytes: u64`, but a corrupted
 *  `storage` blob or a future payload change is exactly the kind of thing
 *  this module should degrade on rather than throw) clamps to zero. */
export function formatBytes(bytes) {
  const n = Number.isFinite(bytes) && bytes > 0 ? bytes : 0;
  if (n === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = n;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  const decimals = unitIndex === 0 ? 0 : 1;
  return `${value.toFixed(decimals)} ${units[unitIndex]}`;
}

/** CPU% formatting. Deliberately NOT capped at 100: `sysinfo`'s per-process
 *  `cpu_usage()` is normalized against a single core, so a busy multi-threaded
 *  process can legitimately read e.g. "230.5%" on an 8-core box — that is
 *  real data, not a bug, and clamping it away would misrepresent what the
 *  bounded `metrics.system` stream actually reports. Only clamps the
 *  impossible (negative/non-finite) case. */
export function formatPercent(cpuPercent) {
  const n = Number.isFinite(cpuPercent) ? Math.max(0, cpuPercent) : 0;
  return `${n.toFixed(1)}%`;
}

function compareBy(column, a, b) {
  switch (column) {
    case "name":
      return a.name.localeCompare(b.name);
    case "pid":
      return a.pid - b.pid;
    case "cpu":
      return a.cpu_percent - b.cpu_percent;
    case "rss":
      return a.rss_bytes - b.rss_bytes;
    default:
      return 0;
  }
}

/** Sort a (already host-bounded, <=32-entry) process list by one column.
 *  Never mutates its input. Ties always break by pid ascending, regardless
 *  of `direction` — so re-sorting the same tick's data is deterministic and
 *  rows don't visibly shuffle between two processes reading the same CPU%. */
export function sortProcesses(processes, column, direction) {
  const dir = direction === "asc" ? 1 : -1;
  return [...processes].sort((a, b) => {
    const cmp = compareBy(column, a, b);
    if (cmp !== 0) return cmp * dir;
    return a.pid - b.pid;
  });
}

/** Validate a `storage.get("sortPref")` result before trusting it: the
 *  plugin's own prior write is the only thing that should ever be there, but
 *  storage is a plain JSON blob on disk (`uistate::load_or_quarantine`) — a
 *  hand-edited or stale-schema file must fall back to the default sort
 *  rather than crash the render. */
export function normalizeSortPref(pref) {
  if (!pref || typeof pref !== "object") return { ...DEFAULT_SORT };
  const column = SORT_COLUMNS.includes(pref.column) ? pref.column : DEFAULT_SORT.column;
  const direction = pref.direction === "asc" ? "asc" : "desc";
  return { column, direction };
}
