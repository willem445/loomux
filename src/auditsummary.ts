// Pure audit-entry summarization: one-line, human-readable sentences per audit
// action. Split out of auditview.ts (issue #248) so it's unit-testable under
// `node --test` without dragging in the AuditView class — that class uses TS
// constructor parameter properties (`constructor(private groupId: string, ...)`),
// which Node's type-stripping test runner cannot parse, so summarize() (and the
// small JSON helpers it needs) live in their own DOM-free module instead, mirroring
// layout.ts / steer.ts / spawnexpiry.ts. auditview.ts imports from here; nothing
// about how the audit viewer renders changes.

export interface AuditEntry {
  ts_ms: number;
  actor: string;
  action: string;
  // detail is per-action JSON; the viewer renders it generically.
  detail: unknown;
}

/** A detail object as a plain record, or null when it isn't one. */
export function asObject(v: unknown): Record<string, unknown> | null {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

export function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

/** Short one-line summary per action. Falls back to compact detail JSON so an
 *  unknown/new action is never opaque. */
export function summarize(e: AuditEntry): string {
  const d = asObject(e.detail) ?? {};
  const firstLine = (s: string): string => {
    const line = s.split("\n", 1)[0];
    return line.length > 160 ? line.slice(0, 160) + "…" : line;
  };
  switch (e.action) {
    case "prompt":
      return `→ ${str(d.to) ?? "?"}: ${firstLine(str(d.text) ?? "")}`;
    case "prompt-typed":
      return `→ ${str(d.to) ?? "?"} delivered (waited ${str(d.waited_ms) ?? d.waited_ms ?? "?"}ms)`;
    case "prompt-failed":
      return `→ ${str(d.to) ?? "?"} failed: ${str(d.reason) ?? ""}`;
    case "submit-retries-skipped":
      return `→ ${str(d.to) ?? "?"}: ${str(d.reason) ?? "retries skipped"}`;
    case "agent-spawn":
      return `${str(d.agent) ?? "?"} (${str(d.role) ?? "?"})${d.task ? ` — ${firstLine(str(d.task) ?? "")}` : ""}`;
    case "agent-bind":
      return `${str(d.agent) ?? "?"} bound to pty ${d.pty ?? "?"}`;
    case "task-upsert": {
      const id = str(d.id) ?? "";
      const title = str(d.title) ?? "";
      const status = str(d.status) ?? "";
      return `${id} "${title}"${status ? ` → ${status}` : ""}`;
    }
    case "task-delete":
      return `deleted ${str(d.id) ?? ""}`;
    case "task-reorder":
      return "reordered task board";
    case "group-create":
    case "group-resume":
      return `${str(d.repo) ?? ""} (max ${d.max_agents ?? "?"})`;
    case "state-write":
      return `state.json (${d.bytes ?? "?"} bytes)`;
    // CI watches (#243/#248): notify_when/list_notifications/cancel_notification's
    // six lifecycle events. register/cancel are agent-initiated (actor = the
    // agent); fired/expired/failed are loomux-delivered notices whose full text
    // rides in `detail.text`, so — like "prompt" above — only its first line
    // goes in the summary; the rest is one click away in the expandable body.
    case "watch-register":
      return `${str(d.target) ?? "?"} — expires in ${d.expires_minutes ?? "?"}m (watch ${str(d.id) ?? "?"})`;
    case "watch-cancel":
      return `cancelled watch ${str(d.id) ?? "?"}`;
    case "watch-cleanup": {
      const ids = Array.isArray(d.ids) ? d.ids.map(String) : [];
      const count = `${ids.length} watch${ids.length === 1 ? "" : "es"}`;
      return `${str(d.agent) ?? "?"} — ${count} dropped${ids.length ? ` (${ids.join(", ")})` : ""}`;
    }
    case "watch-fired":
    case "watch-expired":
    case "watch-failed":
      return `→ ${str(d.agent) ?? "?"}: ${firstLine(str(d.text) ?? "")}`;
    // Cross-workspace channels (#271): connect/disconnect are human-initiated (actor
    // "human", mirroring the watch-register/-cancel pattern above); channel-message is
    // agent-initiated. Written to BOTH endpoints' group logs, so each side's timeline
    // reads the same sentence for the same event.
    case "channel-connect": {
      const members = Array.isArray(d.members) ? (d.members as Record<string, unknown>[]) : [];
      const names = members.map((m) => `${str(m.name) ?? "?"} (${str(m.role) ?? "?"})`);
      return `connected ${names.join(" ↔ ") || "?"} — channel ${str(d.channel_id) ?? "?"}`;
    }
    case "channel-message":
      return `${str(d.from) ?? "?"} → ${str(d.to) ?? "?"} (channel ${str(d.channel_id) ?? "?"}): ${firstLine(str(d.text) ?? "")}`;
    case "channel-disconnect": {
      // `remaining` is a bare count, not a `closed` flag (mod.rs's disconnect_agent
      // never writes one) — the backend tears the whole channel down once membership
      // drops below 2, so remaining < 2 in THIS record is what "closed" means here.
      const remainingNum = typeof d.remaining === "number" ? d.remaining : Number(d.remaining ?? NaN);
      const note =
        remainingNum < 2
          ? "channel closed"
          : `${remainingNum} member${remainingNum === 1 ? "" : "s"} remaining`;
      return `${str(d.agent) ?? "?"} disconnected from channel ${str(d.channel_id) ?? "?"} — ${note}`;
    }
    default: {
      const compact = JSON.stringify(e.detail ?? {});
      return compact === "{}" || compact === "null"
        ? ""
        : compact.length > 200
          ? compact.slice(0, 200) + "…"
          : compact;
    }
  }
}
