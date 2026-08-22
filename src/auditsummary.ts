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

/** A `*_ms` duration as whole minutes, for the lock lifecycle lines (#858).
 *  Rounds UP so a 40-second hold reads "1 min" rather than "0 min", which
 *  would read as a bug in the mechanism rather than a short hold. Non-numeric
 *  (a truncated or hand-edited record) reads "?" instead of "NaN min". */
export function minutes(v: unknown): string {
  return typeof v === "number" && Number.isFinite(v)
    ? `${Math.max(0, Math.ceil(v / 60_000))} min`
    : "? min";
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
    // #569: a paused group used to DISCARD a delivery and tell its caller Ok —
    // the one remaining path where a payload the sender was told succeeded
    // ceased to exist. Option 2 replaced that with a queue admission, so no
    // build writes this line any more; a timeline still carrying one was
    // written by an older loomux, and the viewer says so rather than leaving a
    // reader to assume the current build behaves this way.
    case "prompt-suppressed-paused":
      return `✕ ${str(d.to) ?? "?"} — discarded, older orrerix (group paused): ${firstLine(str(d.text) ?? "")}`;
    // #569: the resume-time tally for those legacy discards. `delivered` is
    // whether the orchestrator actually took the notice; when it did not, the
    // reason is named, because "the orchestrator was told" is a claim and this
    // is the line that has to be honest about it.
    case "pause-suppression-notice": {
      const n = typeof d.count === "number" ? d.count : Number(d.count ?? NaN);
      const what = `${n} deliver${n === 1 ? "y" : "ies"} discarded by an earlier orrerix while paused`;
      return d.delivered
        ? `${what} — orchestrator notified on resume`
        : `${what} — could NOT notify the orchestrator (${str(d.error) ?? "?"}); panes badged instead`;
    }
    // #569 review B1: the admission saw the group had been resumed underneath
    // it, so it took responsibility for starting the drain that `resume_group`
    // had already decided it did not need to. Rare and invisible otherwise —
    // this line is the only trace the race ever happened, which is what makes
    // it worth its own sentence rather than a JSON blob.
    case "pause-race-nudge":
      return `raced a resume — nudged the drainer for ${str(d.to) ?? "?"} (pty ${d.pty ?? "?"}, delivery ${d.id ?? "?"})`;
    // #569 option 2: the resume set these panes draining what the pause held.
    // `started` is false when there was no app handle to spawn a drainer with,
    // and the line has to say so — "the flush began" is a claim like any other.
    case "pause-flush": {
      const panes = Array.isArray(d.panes) ? d.panes : [];
      const what = `resume: flushing ${panes.length} held pane${panes.length === 1 ? "" : "s"} (${panes.join(", ")})`;
      return d.started === false ? `${what} — NOT started (no app handle)` : what;
    }
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
    // Lock resources (#858): acquire/queue are agent-initiated (actor = the
    // agent, so the sentence names the resource rather than repeating them);
    // grant/expire/reclaim/timeout are loomux's own and DO name the agent,
    // because the actor column reads "loomux" and the whole point of those
    // lines is which agent lost or gained a slot.
    case "lock-acquire":
      return `took '${str(d.resource) ?? "?"}'${d.note ? ` — ${firstLine(str(d.note) ?? "")}` : ""}`;
    case "lock-acquire-repeat":
      return `already held '${str(d.resource) ?? "?"}' (no change)`;
    case "lock-queued":
      return `queued for '${str(d.resource) ?? "?"}' at position ${d.position ?? "?"}`;
    case "lock-queued-repeat":
      return `already queued for '${str(d.resource) ?? "?"}' at position ${d.position ?? "?"}`;
    case "lock-release":
      return `released '${str(d.resource) ?? "?"}'`;
    case "lock-queue-cancel":
      return `withdrew from the '${str(d.resource) ?? "?"}' queue (was position ${d.position ?? "?"})`;
    case "lock-grant":
      return `'${str(d.resource) ?? "?"}' → ${str(d.agent) ?? "?"} (waited ${minutes(d.waited_ms)})`;
    case "lock-expired":
      return `'${str(d.resource) ?? "?"}' reclaimed from ${str(d.agent) ?? "?"} — held ${minutes(d.held_ms)}, past its max hold`;
    case "lock-reclaim":
      return `'${str(d.resource) ?? "?"}' reclaimed from ${str(d.agent) ?? "?"} — its pane is gone (held ${minutes(d.held_ms)})`;
    case "lock-wait-timeout":
      return `${str(d.agent) ?? "?"} gave up waiting for '${str(d.resource) ?? "?"}' after ${minutes(d.waited_ms)}`;
    case "lock-wait-cleanup":
      return `${str(d.agent) ?? "?"} left the '${str(d.resource) ?? "?"}' queue — its pane is gone`;
    case "lock-undeclared":
      return `'${str(d.resource) ?? "?"}' is no longer declared in the workflow file — its holders and queue were dropped`;
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
    // Human-only sender swap (#271 W3 addendum, part B5) — same actor convention as
    // channel-connect/-disconnect above ("human").
    case "channel-direction":
      return `channel ${str(d.channel_id) ?? "?"}: sender changed from ${str(d.from_sender) ?? "?"} to ${str(d.to_sender) ?? "?"}`;
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
