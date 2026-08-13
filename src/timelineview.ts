// Progress-timeline overlay for orchestration panes (#608, Slice C): a
// group's audit log and the repo's GitHub issue/PR lifecycle, plotted on one
// time axis. Read-only — it mutates nothing and starts no backend work beyond
// the two reads it already shares with the audit view.
//
// Every answer that can be WRONG lives in the three DOM-free modules this file
// consumes — timelinemodel.ts (what happened), timelinelayout.ts (where it
// goes), timelinechrome.ts (how it is labelled and capped). What is left here
// is SVG construction, polling and click wiring, which the repo's convention
// hand-validates rather than unit-tests.
//
// Hard constraint 1 (never resize the PTY for a UI feature) lands on this
// view: it is an embeddable view (#361), so it FLOATS as an overlay by
// default, and when the human docks it, it goes through the same grid path
// every other embedded panel uses. Nothing here measures or resizes a
// terminal — the chart's width comes from its own container via a
// ResizeObserver, and the pure layout takes that width as a parameter.

import { invoke } from "./transport.ts";
// The orch_* reads are called here directly, exactly as AuditView and
// TasksView call `orch_audit` / `orch_tasks`: an orchestration view is its own
// bridge for the group-scoped read it renders. The gh half goes through
// issues.ts's typed wrapper, which is where every gh command already lives.
import { ghActivity, type GhActivity } from "./issues";
import { RefreshGate } from "./refreshgate";
import {
  CATEGORY_ORDER,
  DEFAULT_CATEGORIES,
  DEFAULT_WINDOW_ID,
  WINDOW_PRESETS,
  categoryOf,
  coverageNotes,
  extractTimeline,
  filterTimeline,
  resolveWindow,
  type TimelineCategory,
  type TimelineEvent,
  type TimelineExtraction,
  type TimelineRange,
} from "./timelinemodel";
import {
  DEFAULT_LANE_HEIGHT_PX,
  DEFAULT_PAD_LEFT_PX,
  DEFAULT_PAD_RIGHT_PX,
  layoutTimeline,
  xForTs,
} from "./timelinelayout";
import {
  detailSlice,
  ghCoverageFloorMs,
  ghFloorNote,
  ghUnavailableNote,
  laneLabel,
  shouldRefreshGh,
  tickScale,
  toggleCategory,
  type TimelineNote,
} from "./timelinechrome";
import { PollGate } from "./pollgate";

/** Audit re-poll cadence while following — the same 1.5s the audit view uses,
 *  since it is the same underlying read. The gh half is refreshed far more
 *  slowly (timelinechrome's `GH_REFRESH_MS`): it shells out to `gh`. */
const FOLLOW_MS = 1500;

const SVG_NS = "http://www.w3.org/2000/svg";

/** Vertical chrome around the lanes: breathing room above the first lane, and
 *  the strip along the bottom that holds the tick labels. */
const TOP_PAD_PX = 10;
const AXIS_PX = 22;

const DOT_R = 4;
const CLUSTER_R = 7;

function el(tag: string, cls: string, text?: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function svgEl(tag: string, cls?: string): SVGElement {
  const e = document.createElementNS(SVG_NS, tag);
  if (cls) e.setAttribute("class", cls);
  return e;
}

/** The expandable body for one event: its raw source record. Mirrors the audit
 *  view's own detail body — the raw record is what has repeatedly been
 *  decisive in debugging rounds, so it is shown verbatim rather than
 *  summarized twice. */
function detailText(detail: unknown): string {
  try {
    return JSON.stringify(detail, null, 2);
  } catch {
    return String(detail);
  }
}

export class TimelineView {
  readonly el: HTMLElement;
  private countEl: HTMLElement;
  private followBtn: HTMLButtonElement;
  private embedBtn: HTMLButtonElement;
  private closeBtn: HTMLButtonElement;
  private windowBarEl: HTMLElement;
  private chipBarEl: HTMLElement;
  private bodyEl: HTMLElement;
  private chartEl: HTMLElement;
  private notesEl: HTMLElement;
  private detailEl: HTMLElement;

  /** Raw audit rows, as received. Deliberately `unknown[]`: `extractTimeline`
   *  counts a row it cannot read rather than throwing on it. */
  private auditRows: unknown[] = [];
  private gh: GhActivity | null = null;
  private ghError: unknown = null;
  /** When gh was last ATTEMPTED (not last succeeded) — a failing gh must not
   *  be retried on every 1.5s audit tick. */
  private ghAttemptedMs: number | null = null;

  private windowId = DEFAULT_WINDOW_ID;
  /** Always in `CATEGORY_ORDER` order — `toggleCategory` guarantees it, so the
   *  chips, the lanes and this list can never disagree about position. */
  private categories: TimelineCategory[] = [...DEFAULT_CATEGORIES];
  /** The clicked dot, held as WHAT it was (lane + instant span) rather than as
   *  an index: indices are invalidated by every follow poll, but a lane and a
   *  time span still identify the same cluster in the next payload. */
  private selected: { lane: string; tsMinMs: number; tsMaxMs: number } | null = null;
  private expanded = new Set<string>();

  private follow = false;
  private followTimer: number | undefined;
  /** Window-visibility gate around the follow timer (#743 S6, pollgate.ts) —
   *  the audit view's shape, for the same reason: an armed follow behind a
   *  minimized window refetched the whole audit log every 1.5 s to redraw
   *  lanes nobody could see. */
  private followGate: PollGate = new PollGate({
    arm: () => {
      this.followTimer = window.setInterval(() => void this.load(false), FOLLOW_MS);
    },
    disarm: () => {
      if (this.followTimer !== undefined) {
        clearInterval(this.followTimer);
        this.followTimer = undefined;
      }
    },
    refresh: () => void this.load(false),
  });
  private disposed = false;
  private gate = new RefreshGate();
  private resizeObs: ResizeObserver;
  private lastWidthPx = 0;
  private rafPending = false;
  /** Signature of the last render, so a follow poll that changed nothing does
   *  not rebuild the SVG under the human's cursor. */
  private lastSig = "";

  /** The most recent extraction + its window, kept so a resize or a chip click
   *  re-renders without re-fetching. */
  private extraction: TimelineExtraction | null = null;
  private range: TimelineRange | null = null;
  private filtered: TimelineEvent[] = [];

  constructor(
    private groupId: string,
    private opts: {
      onClose: () => void;
      onEmbedMenu?: (anchor: HTMLElement) => void;
      /** The pane's live repo folder — read on every load rather than
       *  snapshotted, the same way the group panel reads it (#316). Null when
       *  the pane has no folder, which is a real state and renders as a note
       *  instead of a silent GitHub-free chart. */
      getRepo: () => string | null;
    }
  ) {
    this.el = el("div", "timeline-view");

    const head = el("div", "timeline-head");
    head.append(el("span", "timeline-title", "progress timeline"));
    head.append(el("span", "timeline-group", groupId));
    this.countEl = el("span", "timeline-count");
    head.append(this.countEl);

    this.followBtn = el("button", "timeline-follow", "▶ follow") as HTMLButtonElement;
    this.followBtn.title = "Live-follow: re-poll for new activity";
    this.followBtn.addEventListener("click", () => this.toggleFollow());
    head.append(this.followBtn);

    const refresh = el("button", "pane-btn", "⟳") as HTMLButtonElement;
    refresh.title = "Refresh (re-reads GitHub too)";
    refresh.addEventListener("click", () => void this.load(true));
    head.append(refresh);

    // Embed side-picker (#361): float, or dock to any of the pane's three
    // slots. Same contract as every other embeddable view.
    this.embedBtn = el("button", "pane-btn embed", "⬒") as HTMLButtonElement;
    this.embedBtn.addEventListener("click", () => this.opts.onEmbedMenu?.(this.embedBtn));
    head.append(this.embedBtn);

    this.closeBtn = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    this.closeBtn.title = "Close (Alt+W)";
    this.closeBtn.addEventListener("click", () => this.opts.onClose());
    head.append(this.closeBtn);
    this.setPanelActive(false);

    // Window presets + category chips.
    const controls = el("div", "timeline-controls");
    this.windowBarEl = el("div", "timeline-windows");
    for (const preset of WINDOW_PRESETS) {
      const b = el("button", "timeline-chip window", preset.label) as HTMLButtonElement;
      b.dataset.window = preset.id;
      b.title = `Show the last ${preset.label}`;
      b.addEventListener("click", () => {
        this.windowId = preset.id;
        // A window change invalidates the selected cluster's span.
        this.selected = null;
        this.syncChips(); // the active-preset highlight is chrome, not render output
        this.lastSig = "";
        this.render();
      });
      this.windowBarEl.append(b);
    }
    this.chipBarEl = el("div", "timeline-chips");
    for (const cat of CATEGORY_ORDER) {
      const b = el("button", "timeline-chip cat", laneLabel(cat)) as HTMLButtonElement;
      b.dataset.category = cat;
      b.title = `Show/hide the ${laneLabel(cat)} lane`;
      b.addEventListener("click", () => this.toggleCategoryChip(cat));
      this.chipBarEl.append(b);
    }
    controls.append(this.windowBarEl, this.chipBarEl);

    this.bodyEl = el("div", "timeline-body");
    this.chartEl = el("div", "timeline-chart");
    this.notesEl = el("div", "timeline-notes");
    this.detailEl = el("div", "timeline-detail");
    this.bodyEl.append(this.chartEl, this.notesEl, this.detailEl);

    this.el.append(head, controls, this.bodyEl);

    // The chart is laid out against its own container's width — never against
    // the terminal's, and never by resizing anything. One relayout per frame
    // at most; a divider drag emits a resize per mousemove.
    this.resizeObs = new ResizeObserver(() => this.onResize());
    this.resizeObs.observe(this.chartEl);

    this.syncChips();
  }

  /** Called by the pane whenever the view becomes visible, in either mode. */
  show(): void {
    void this.load(false);
  }

  /** Called by the pane whenever the view is hidden — stop the poll. Without
   *  this, every close or slot-eviction leaks a live interval (#361 rev-38's
   *  finding on the group panel). */
  hide(): void {
    this.stopFollow();
    if (this.follow) {
      this.follow = false;
      this.followBtn.classList.remove("on");
      this.followBtn.textContent = "▶ follow";
    }
  }

  /** Reflect whether the pane currently has this view docked (#361). */
  setPanelActive(active: boolean): void {
    this.embedBtn.classList.toggle("active", active);
    this.embedBtn.textContent = active ? "⬓" : "⬒";
    this.embedBtn.title = active
      ? "Un-embed — back to a floating overlay"
      : "Embed beside the terminal (resizes this pane)";
    this.closeBtn.disabled = active;
    this.closeBtn.title = active ? "Docked — un-embed it (side menu) to close" : "Close (Alt+W)";
  }

  dispose(): void {
    this.disposed = true;
    this.stopFollow();
    this.resizeObs.disconnect();
    this.el.remove();
  }

  private toggleFollow(): void {
    this.follow = !this.follow;
    this.followBtn.classList.toggle("on", this.follow);
    this.followBtn.textContent = this.follow ? "⏸ following" : "▶ follow";
    if (this.follow) {
      this.followGate.enable();
      void this.load(false);
    } else {
      this.stopFollow();
    }
  }

  private stopFollow(): void {
    this.followGate.disable();
  }

  private toggleCategoryChip(cat: TimelineCategory): void {
    this.categories = toggleCategory(this.categories, cat, CATEGORY_ORDER) as TimelineCategory[];
    // The hidden lane may have been the selected dot's own.
    if (this.selected && !this.categories.includes(this.selected.lane as TimelineCategory)) {
      this.selected = null;
    }
    this.syncChips();
    this.lastSig = "";
    this.render();
  }

  private syncChips(): void {
    for (const b of Array.from(this.chipBarEl.children) as HTMLButtonElement[]) {
      const cat = b.dataset.category ?? "";
      b.classList.toggle("on", (this.categories as readonly string[]).includes(cat));
      b.classList.add(`cat-${cat}`);
    }
    for (const b of Array.from(this.windowBarEl.children) as HTMLButtonElement[]) {
      b.classList.toggle("on", b.dataset.window === this.windowId);
    }
  }

  private onResize(): void {
    const w = Math.round(this.chartEl.clientWidth);
    if (w === this.lastWidthPx) return;
    if (this.rafPending) return;
    this.rafPending = true;
    // Coalesce a drag's per-frame resizes into one relayout.
    requestAnimationFrame(() => {
      this.rafPending = false;
      if (this.disposed) return;
      this.lastSig = ""; // the width is part of the geometry, not of the data
      this.render();
    });
  }

  /** Re-read both sources. `forceGh` is the ⟳ button: an explicit refresh
   *  always re-reads GitHub, a follow tick only when the slow cadence is due. */
  private async load(forceGh: boolean): Promise<void> {
    if (this.disposed) return;
    // Single-flight with a trailing re-run, so a click during an in-flight
    // fetch is neither dropped nor run concurrently (refreshgate.ts).
    if (!this.gate.begin()) return;
    try {
      try {
        this.auditRows = await invoke<unknown[]>("orch_audit", { groupId: this.groupId });
      } catch {
        // A missing/unreadable log renders as an empty chart, not a broken one.
        this.auditRows = [];
      }
      const repo = this.opts.getRepo();
      if (!repo) {
        this.gh = null;
        this.ghError = "this pane has no repository folder";
      } else if (forceGh || shouldRefreshGh(this.ghAttemptedMs, Date.now())) {
        this.ghAttemptedMs = Date.now();
        try {
          this.gh = await ghActivity(repo);
          this.ghError = null;
        } catch (err) {
          // The gh layer is additive: keep the audit half and say what is
          // missing (timelinechrome's note), rather than blanking the view.
          this.gh = null;
          this.ghError = err;
        }
      }
    } finally {
      if (!this.disposed) this.render();
    }
    if (this.gate.end() && !this.disposed) void this.load(forceGh);
  }

  private render(): void {
    if (this.disposed) return;

    const extraction = extractTimeline(this.auditRows, this.gh);
    const range = resolveWindow(this.windowId, Date.now(), extraction.events);
    const filtered = filterTimeline(extraction.events, range, this.categories);
    const widthPx = Math.round(this.chartEl.clientWidth);

    // Skip a no-op follow re-render: rebuilding the SVG under the human's
    // pointer (and collapsing an open detail row) for identical data is the
    // same fight with the human the audit view's own signature avoids. The
    // window is part of the signature because "now" slides even when the data
    // does not — but only at tick resolution, so a still session does not
    // rebuild 40 times a minute.
    const sig = [
      extraction.events.length,
      extraction.events.at(-1)?.ts_ms ?? 0,
      extraction.undatable,
      extraction.malformed,
      this.windowId,
      this.categories.join(","),
      widthPx,
      this.selected ? `${this.selected.lane}:${this.selected.tsMinMs}:${this.selected.tsMaxMs}` : "",
      this.expanded.size,
      this.ghError === null ? "" : String(this.ghError),
      Math.floor(range.endMs / 60_000),
    ].join("|");
    if (sig === this.lastSig) return;
    this.lastSig = sig;
    this.lastWidthPx = widthPx;

    this.extraction = extraction;
    this.range = range;
    this.filtered = filtered;

    this.countEl.textContent =
      filtered.length === extraction.events.length
        ? `${extraction.events.length} events`
        : `${filtered.length} / ${extraction.events.length} events`;

    this.renderChart(widthPx);
    this.renderNotes();
    this.renderDetail();
  }

  private renderChart(widthPx: number): void {
    const range = this.range!;
    this.chartEl.replaceChildren();

    if (this.categories.length === 0) {
      this.chartEl.append(
        el("div", "timeline-empty", "Every category is switched off — nothing to plot. Turn one back on above.")
      );
      return;
    }
    if (this.extraction!.events.length === 0) {
      this.chartEl.append(
        el("div", "timeline-empty", "No activity recorded for this group yet.")
      );
      return;
    }

    // Lanes for every ENABLED category, whether or not it has events in this
    // window: an empty lane says "nothing happened here", and lanes that come
    // and go as the window slides make the chart jump. `this.categories` is
    // already in lane order (toggleCategory keeps it there).
    const laneKeys = this.categories as readonly string[];
    const layout = layoutTimeline(
      this.filtered.map((e) => ({ ts_ms: e.ts_ms, lane: categoryOf(e.kind) as string })),
      range,
      widthPx,
      {
        laneKeys,
        laneOrder: CATEGORY_ORDER as readonly string[],
        laneHeightPx: DEFAULT_LANE_HEIGHT_PX,
        padLeftPx: DEFAULT_PAD_LEFT_PX,
        padRightPx: DEFAULT_PAD_RIGHT_PX,
      }
    );

    const height = TOP_PAD_PX + layout.heightPx + AXIS_PX;
    const svg = svgEl("svg", "timeline-svg") as SVGSVGElement;
    svg.setAttribute("width", String(Math.max(0, widthPx)));
    svg.setAttribute("height", String(height));

    const fmt = this.tickFormatter(layout.ticks.stepMs);
    // Ticks first, so dots and lane rules paint over them.
    for (const t of layout.ticks.ticks) {
      const x = xForTs(layout.scale, t);
      const line = svgEl("line", "timeline-tick");
      line.setAttribute("x1", String(x));
      line.setAttribute("x2", String(x));
      line.setAttribute("y1", String(TOP_PAD_PX));
      line.setAttribute("y2", String(TOP_PAD_PX + layout.heightPx));
      svg.append(line);
      const label = svgEl("text", "timeline-tick-label");
      label.setAttribute("x", String(x));
      label.setAttribute("y", String(height - 7));
      label.setAttribute("text-anchor", "middle");
      label.textContent = fmt.format(new Date(t));
      svg.append(label);
    }

    for (const lane of layout.lanes) {
      const y = TOP_PAD_PX + lane.y;
      const rule = svgEl("line", "timeline-lane-rule");
      rule.setAttribute("x1", String(layout.scale.x0));
      rule.setAttribute("x2", String(layout.scale.x1));
      rule.setAttribute("y1", String(y));
      rule.setAttribute("y2", String(y));
      svg.append(rule);
      const label = svgEl("text", `timeline-lane-label cat-${lane.id}`);
      label.setAttribute("x", "10");
      label.setAttribute("y", String(y + 4));
      label.textContent = laneLabel(lane.id);
      svg.append(label);
    }

    for (const dot of layout.dots) {
      const g = svgEl("g", "timeline-dot-g");
      const isSelected =
        this.selected !== null &&
        this.selected.lane === dot.lane &&
        this.selected.tsMinMs === dot.tsMinMs &&
        this.selected.tsMaxMs === dot.tsMaxMs;
      const circle = svgEl("circle", `timeline-dot cat-${dot.lane}${isSelected ? " selected" : ""}`);
      circle.setAttribute("cx", String(dot.x));
      circle.setAttribute("cy", String(TOP_PAD_PX + dot.y));
      circle.setAttribute("r", String(dot.count > 1 ? CLUSTER_R : DOT_R));
      g.append(circle);
      if (dot.count > 1) {
        const n = svgEl("text", "timeline-dot-count");
        n.setAttribute("x", String(dot.x));
        n.setAttribute("y", String(TOP_PAD_PX + dot.y + 3));
        n.setAttribute("text-anchor", "middle");
        n.textContent = dot.count > 99 ? "99+" : String(dot.count);
        g.append(n);
      }
      // Native tooltip: no custom positioning to get wrong, and it works
      // identically docked or floating.
      const title = svgEl("title");
      title.textContent = this.dotTooltip(dot.indices, dot.count);
      g.append(title);
      g.addEventListener("click", () => {
        this.selected =
          isSelected ? null : { lane: dot.lane, tsMinMs: dot.tsMinMs, tsMaxMs: dot.tsMaxMs };
        this.expanded.clear();
        this.lastSig = "";
        this.render();
      });
      svg.append(g);
    }

    this.chartEl.append(svg);
    if (layout.dots.length === 0) {
      this.chartEl.append(
        el(
          "div",
          "timeline-empty",
          "No events in this window for the categories you have on. Try a wider window, or turn a category back on."
        )
      );
    }
  }

  private tickFormatter(stepMs: number): Intl.DateTimeFormat {
    // Locale lives here, never in the pure layer: the scale decision is
    // tested, the rendering of it is the browser's.
    switch (tickScale(stepMs)) {
      case "seconds":
        return new Intl.DateTimeFormat(undefined, {
          hour: "2-digit",
          minute: "2-digit",
          second: "2-digit",
        });
      case "days":
        return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
      default:
        return new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });
    }
  }

  private dotTooltip(indices: readonly number[], count: number): string {
    const first = this.filtered[indices[0]];
    if (count === 1) return `${fmtTime(first.ts_ms)} · ${first.label}`;
    const lines = indices.slice(0, 6).map((i) => `${fmtTime(this.filtered[i].ts_ms)} · ${this.filtered[i].label}`);
    if (indices.length > lines.length) lines.push(`…and ${indices.length - lines.length} more — click to expand`);
    return `${count} events\n${lines.join("\n")}`;
  }

  /** Everything this chart is NOT showing. The pure layer owns the sentences
   *  (so they are unit-pinned rather than view prose nobody tests); this only
   *  decides which ones apply and paints them. */
  private renderNotes(): void {
    const x = this.extraction!;
    const notes: TimelineNote[] = [...coverageNotes(x, this.range!)];
    if (this.ghError !== null) notes.push(ghUnavailableNote(this.ghError));
    if (this.gh) {
      // The precise floor, where a capped list can give one.
      if (this.gh.issues_truncated) {
        const floor = ghCoverageFloorMs(this.gh.issues);
        if (floor !== null) notes.push(ghFloorNote("issues", floor));
      }
      if (this.gh.prs_truncated) {
        const floor = ghCoverageFloorMs(this.gh.prs);
        if (floor !== null) notes.push(ghFloorNote("PRs", floor));
      }
    }
    const hidden = CATEGORY_ORDER.filter((c) => !this.categories.includes(c));
    if (hidden.length > 0) {
      notes.push({
        id: "categories-off",
        text: `Lanes switched off: ${hidden.map(laneLabel).join(", ")} — those events are loaded, just not plotted.`,
      });
    }

    this.notesEl.replaceChildren();
    for (const n of notes) {
      const row = el("div", `timeline-note note-${n.id}`, n.text);
      this.notesEl.append(row);
    }
  }

  private renderDetail(): void {
    this.detailEl.replaceChildren();
    const sel = this.selected;
    if (!sel) return;
    const members: number[] = [];
    this.filtered.forEach((e, i) => {
      if (categoryOf(e.kind) === sel.lane && e.ts_ms >= sel.tsMinMs && e.ts_ms <= sel.tsMaxMs) {
        members.push(i);
      }
    });
    if (members.length === 0) {
      // The cluster's events fell out of the current window/filters.
      this.selected = null;
      return;
    }

    const head = el("div", "timeline-detail-head");
    head.append(
      el(
        "span",
        "timeline-detail-title",
        `${members.length} ${members.length === 1 ? "event" : "events"} · ${laneLabel(sel.lane)} · ${fmtTime(sel.tsMinMs)}${
          sel.tsMaxMs !== sel.tsMinMs ? ` – ${fmtTime(sel.tsMaxMs)}` : ""
        }`
      )
    );
    const close = el("button", "pane-btn close", "✕") as HTMLButtonElement;
    close.title = "Clear selection";
    close.addEventListener("click", () => {
      this.selected = null;
      this.lastSig = "";
      this.render();
    });
    head.append(close);
    this.detailEl.append(head);

    const { shown, hidden } = detailSlice(members);
    for (const i of shown) this.detailEl.append(this.renderDetailRow(this.filtered[i]));
    if (hidden > 0) {
      this.detailEl.append(
        el(
          "div",
          "timeline-detail-more",
          `…and ${hidden} more event${hidden === 1 ? "" : "s"} in this cluster, not listed. Narrow the window to see them.`
        )
      );
    }
  }

  private renderDetailRow(ev: TimelineEvent): HTMLElement {
    // Keyed by the EVENT, never by its index: a follow poll shifts every index
    // in `filtered`, which would silently collapse (or worse, move) an open row.
    const key = `${ev.ts_ms}|${ev.kind}|${ev.label}`;
    const row = el("div", "timeline-detail-row");
    const top = el("div", "timeline-detail-top expandable");
    top.append(el("span", "timeline-detail-caret", this.expanded.has(key) ? "▾" : "▸"));
    top.append(el("span", "timeline-detail-time", fmtTime(ev.ts_ms)));
    top.append(el("span", `timeline-detail-kind kind-${ev.kind}`, ev.kind));
    top.append(el("span", "timeline-detail-label", ev.label));
    if (ev.source !== "audit") top.append(el("span", "timeline-detail-source", ev.source));
    top.addEventListener("click", () => {
      if (this.expanded.has(key)) this.expanded.delete(key);
      else this.expanded.add(key);
      this.lastSig = "";
      this.render();
    });
    row.append(top);
    if (this.expanded.has(key)) {
      const pre = el("pre", "timeline-detail-raw");
      pre.textContent = detailText(ev.detail);
      row.append(pre);
    }
    return row;
  }
}

const fmtTime = (ms: number): string =>
  new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
