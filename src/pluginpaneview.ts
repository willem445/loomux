// The "plugin" content-pane surface (#360 Slice D — see doc/design/pane-plugins.md).
// Unlike GitView/FileEditView/WorkflowView, this view hosts NO DOM content of its
// own: a plugin's UI lives in its own isolated `WebviewWindow` (Slice C's
// `plugin_open_window`), a separate top-level OS window, not a DOM node this view
// could append. "Hosting it as the pane's content" therefore means continuously
// repositioning and resizing that OS window to sit exactly over `this.el` — the
// pane's `.pane-content` box — for as long as the pane exists, and closing it the
// moment the pane does. The pure arithmetic for that lives in pluginwindow.ts
// (DOM-free, unit-tested); this file is the Tauri/DOM wiring, hand-validated per
// CLAUDE.md's convention for DOM wiring.
//
// KNOWN, ACCEPTED GAPS (documented rather than engineered around, matching this
// repo's own precedent for a cosmetic-but-real limitation — see content-panes.md's
// "one known, accepted cosmetic gap"):
//   - A freshly-opened plugin window may flash at Tauri's default placement for one
//     frame before the first reposition lands (hide→reposition→show narrows this to
//     a brief hide, but Slice C's builder — out of this slice's scope to change —
//     doesn't take an initial `visible: false`).
//   - Z-order relative to the main window (context menus, toasts, modals) is
//     whatever the OS window manager gives a plain top-level window; this view does
//     not fight it with a setFocus/always-on-top dance.
//   - Multi-monitor DPI: repositioning uses the MAIN window's own scale factor for
//     both windows. A plugin window dragged by the human onto a differently-scaled
//     monitor would fight the next reposition — an edge case the human can avoid by
//     simply not dragging a hosted plugin window, which has no reason to be dragged.

import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";
import { dataDir, join } from "@tauri-apps/api/path";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { openPluginWindow, type PluginCapability } from "./pluginbroker";
import { pluginErrorCode, pluginErrorMessage, type PluginManifest } from "./pluginhost";
import { pluginOverlayRect, pluginWindowShouldShow } from "./pluginwindow";

/** What a plugin pane needs to open its window — the CURRENT manifest's fields
 *  `plugin_open_window` (Slice C) takes, resolved by the caller (the welcome form
 *  or main.ts's restore path) from `list_plugins` (Slice B) at open time, never
 *  persisted (doc/design/pane-plugins.md: a plugin's install root/capabilities can
 *  change between sessions — a re-install, an upgrade — and the pane must follow
 *  the CURRENT manifest, not a stale snapshot; only `pluginId` itself persists,
 *  in `PersistedPane.pluginId`). */
export interface PluginPaneManifest {
  pluginId: string;
  entry: string;
  /** Straight off `PluginManifest.capabilities` (pluginhost.ts) — Slice B already
   *  validated every entry against the closed enum at manifest-parse time, but the
   *  wire type stays `string[]` there (arbitrary JSON), so this view re-narrows to
   *  `PluginCapability[]` itself (`toPluginCapabilities`, below) rather than
   *  trusting an upstream cast. `plugin_open_window` (Slice C) re-validates anyway
   *  — defense in depth, never a caller's validation as the only check. */
  capabilities: string[];
  apiVersion: number;
  /** Absolute path to the plugin's install folder; null for `rootless: true`. */
  root: string | null;
  /** The manifest's `name` field, VERBATIM — untrusted third-party text (design
   *  note: "Plugin-provided text is untrusted, regardless of transport"). This
   *  view only ever renders it via `textContent`, never markup interpolation;
   *  callers (Pane.setName for the tab label) must hold the same line. */
  displayName: string;
}

/** `<app-data>/loomux/plugins/<id>` — the recommended install location
 *  (`doc/design/pane-plugins.md`, "Install / discovery"), computed HERE
 *  because `list_plugins` (Slice B, `pluginhost.ts`) echoes back a manifest's
 *  `id`/`name`/`entry`/`capabilities`/`apiVersion`/`rootless` but not an
 *  absolute path — Slice B's wire type never needed one until now, and this
 *  slice can't add one without editing Slice B's file (out of scope). Mirrors
 *  `plugins_root_dir()` (`src-tauri/src/plugins.rs`) via Tauri's OWN
 *  base-directory resolver (`dataDir()`, which the Windows baseline this repo
 *  targets resolves to the identical `%APPDATA%` `dirs::data_dir()` returns)
 *  rather than reimplementing OS path logic — the "loomux/plugins" suffix is
 *  the install-location contract the design note PUBLISHES, not a private
 *  implementation detail being duplicated. Null for `rootless: true`,
 *  matching `plugin_open_window`'s own `root: None` contract exactly. If the
 *  install location ever changes (the design note flags it as an OPEN
 *  decision — a repo-scoped `.loomux/plugins/` alternative is still live),
 *  this is the one place Slice D needs to follow it. */
async function resolvePluginRoot(manifest: Pick<PluginManifest, "id" | "rootless">): Promise<string | null> {
  if (manifest.rootless) return null;
  return join(await dataDir(), "loomux", "plugins", manifest.id);
}

/** Build the manifest this view (and `plugin_open_window`) needs from what
 *  `list_plugins` returned, resolving the one field it doesn't carry. The
 *  single conversion point launcher.ts and main.ts both call so the two
 *  places a plugin pane can be opened (fresh, and on restore) build the same
 *  shape the same way. */
export async function resolvePluginPaneManifest(manifest: PluginManifest): Promise<PluginPaneManifest> {
  return {
    pluginId: manifest.id,
    entry: manifest.entry,
    capabilities: manifest.capabilities,
    apiVersion: manifest.api_version,
    root: await resolvePluginRoot(manifest),
    displayName: manifest.name,
  };
}

export interface PluginPaneHost {
  /** The manifest to open. Resolved once, at construction — a plugin pane's
   *  identity (`pluginId`) is immutable for its lifetime, the same "the id
   *  doesn't change under you" rule the workflow pane's block ids and the git
   *  pane's `root` (post-creation) both hold. */
  manifest: PluginPaneManifest;
}

const DEFAULT_WIDTH = 640;
const DEFAULT_HEIGHT = 480;

/** The v1 capability enum, mirrored the same way pluginprotocol.ts already
 *  mirrors it from `pluginbroker.rs` — see that file's own doc comment on why
 *  this stays a closed, reviewed list rather than trusting whatever strings a
 *  manifest happened to carry. Narrows `PluginManifest.capabilities: string[]`
 *  (pluginhost.ts — Slice B's wire type, already backend-validated but not
 *  narrowed) into the `PluginCapability[]` `plugin_open_window` expects. Any
 *  entry that ISN'T one of these four is dropped, never passed through — the
 *  backend re-validates anyway, but this view has no business handing a bad
 *  string to a security-relevant command on the strength of "it was probably
 *  fine". */
const KNOWN_CAPABILITIES: readonly PluginCapability[] = ["panel", "storage", "fs.read", "metrics.system"];
function toPluginCapabilities(caps: readonly string[]): PluginCapability[] {
  return caps.filter((c): c is PluginCapability =>
    (KNOWN_CAPABILITIES as readonly string[]).includes(c)
  );
}

/** Matches `buildContentView`'s `{ el, show() }` contract in pane.ts, plus
 *  `dispose()` — the same shape GitView/FileEditView/WorkflowView expose. */
export class PluginPaneView {
  readonly el: HTMLElement;
  private statusEl: HTMLElement;

  private manifest: PluginPaneManifest;
  private windowLabel: string | null = null;
  private pluginWindow: Window | null = null;
  private resizeObs: ResizeObserver;
  private unlistenMoved: UnlistenFn | null = null;
  private unlistenResized: UnlistenFn | null = null;
  /** Whether the plugin window is CURRENTLY shown — tracked so a resize-to-zero
   *  followed by a resize-back-to-nonzero only calls `.show()`/`.hide()` on an
   *  actual transition, not on every observer tick (each is a real IPC round
   *  trip to the backend). */
  private shown = false;
  private disposed = false;
  /** True once `openPluginWindow` has resolved successfully — guards the
   *  ResizeObserver from trying to position a window that doesn't exist yet
   *  (still opening) or any more (failed, or disposed mid-open). */
  private ready = false;
  /** Monotonic token so a `reposition()` call that's still awaiting the
   *  backend (scaleFactor/innerPosition/setPosition/setSize are each their
   *  own IPC round trip) can't clobber a NEWER call's result if a rapid burst
   *  of resize/move events fires before the first one finishes — a divider
   *  dragged fast enough would otherwise flicker the window back to a stale
   *  position after it had already caught up. */
  private repositionSeq = 0;

  constructor(host: PluginPaneHost) {
    this.manifest = host.manifest;
    this.el = document.createElement("div");
    this.el.className = "pane-plugin";
    // Untrusted text (manifest `name`) — textContent only, never innerHTML. This
    // label is the ONLY thing painted in `el` on success: the plugin's real
    // content is the separate WebviewWindow this view positions over `el`, so
    // once that window is up this text sits harmlessly underneath it.
    this.statusEl = document.createElement("div");
    this.statusEl.className = "pane-plugin-status";
    this.statusEl.textContent = `Opening ${this.manifest.displayName}…`;
    this.el.appendChild(this.statusEl);
    this.resizeObs = new ResizeObserver(() => this.reposition());
  }

  /** Open the plugin's WebviewWindow and start tracking this pane's box. Safe to
   *  call only once `el` is attached to the document (startContent's "ATTACH,
   *  THEN show" contract) — the first `reposition()` reads a real layout. */
  show(): void {
    this.resizeObs.observe(this.el);
    void getCurrentWindow()
      .onMoved(() => this.reposition())
      .then((un) => {
        if (this.disposed) {
          un();
          return;
        }
        this.unlistenMoved = un;
      });
    void getCurrentWindow()
      .onResized(() => this.reposition())
      .then((un) => {
        if (this.disposed) {
          un();
          return;
        }
        this.unlistenResized = un;
      });
    void this.open();
  }

  private async open(): Promise<void> {
    const m = this.manifest;
    // Best-effort initial size from the pane's own current box — `openPluginWindow`
    // requires SOME width/height to build the window with, before this view's own
    // reposition() can measure and correct it a moment later. A pane that happens
    // to be zero-sized right now (opened into a hidden tab) still gets a sane
    // window rather than a degenerate 0x0 one; the first `reposition()` (or the
    // resize/move listeners above) fixes the real size once the pane IS visible.
    const rect = this.el.getBoundingClientRect();
    const width = rect.width > 0 ? rect.width : DEFAULT_WIDTH;
    const height = rect.height > 0 ? rect.height : DEFAULT_HEIGHT;
    try {
      const label = await openPluginWindow({
        pluginId: m.pluginId,
        entry: m.entry,
        root: m.root ?? undefined,
        capabilities: toPluginCapabilities(m.capabilities),
        apiVersion: m.apiVersion,
        title: m.displayName,
        width,
        height,
      });
      if (this.disposed) {
        // The pane closed while the window was still opening — don't leak it.
        void closeWindowByLabel(label);
        return;
      }
      this.windowLabel = label;
      this.pluginWindow = await Window.getByLabel(label);
      if (this.disposed) {
        void closeWindowByLabel(label);
        return;
      }
      this.ready = true;
      this.statusEl.hidden = true;
      await this.reposition();
    } catch (err) {
      if (this.disposed) return;
      this.showError(err);
    }
  }

  /** Render a fail-soft inline error — the design note's "empty/error state, not
   *  a crash" — for whatever `plugin_open_window` refused (an apiVersion this
   *  build doesn't speak, an unknown capability if the manifest changed on disk
   *  since this pane's caller resolved it, …). Text only, never markup: `String(err)`
   *  can embed anything a rejected command's message carries. */
  private showError(err: unknown): void {
    this.statusEl.hidden = false;
    this.statusEl.classList.add("pane-plugin-error");
    this.statusEl.textContent = `Couldn't open "${this.manifest.displayName}": ${pluginErrorMessage(err) || String(err)} (${pluginErrorCode(err)})`;
  }

  /** Recompute this pane's on-screen box and move/resize/show/hide the plugin's
   *  window to match. Called on every layout change that could move `el` — a
   *  divider drag, a split, a tab switch, a maximize elsewhere, the MAIN window
   *  itself moving or resizing — via the ResizeObserver + the two window
   *  listeners registered in `show()`. A no-op until `open()` has a window to
   *  move (`ready`). */
  private async reposition(): Promise<void> {
    if (!this.ready || !this.pluginWindow || this.disposed) return;
    const seq = ++this.repositionSeq;
    const rect = this.el.getBoundingClientRect();
    if (!pluginWindowShouldShow(rect)) {
      if (this.shown) {
        this.shown = false;
        await this.pluginWindow.hide().catch(() => {});
      }
      return;
    }
    try {
      const main = getCurrentWindow();
      const scale = await main.scaleFactor();
      const innerPhysical = await main.innerPosition();
      if (seq !== this.repositionSeq || this.disposed || !this.pluginWindow) return;
      const origin = innerPhysical.toLogical(scale);
      const screen = pluginOverlayRect(origin, rect);
      await this.pluginWindow.setPosition(new LogicalPosition(screen.x, screen.y));
      await this.pluginWindow.setSize(new LogicalSize(screen.width, screen.height));
      if (seq !== this.repositionSeq || this.disposed || !this.pluginWindow) return;
      if (!this.shown) {
        this.shown = true;
        await this.pluginWindow.show();
      }
    } catch {
      // Best-effort: a reposition racing pane teardown (window already closing)
      // must not throw into the ResizeObserver callback.
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.resizeObs.disconnect();
    this.unlistenMoved?.();
    this.unlistenResized?.();
    if (this.windowLabel) void closeWindowByLabel(this.windowLabel);
  }
}

/** Close a plugin window by label, best-effort — mirrors the fire-and-forget
 *  `killPty(...).catch(() => {})` posture the rest of this codebase uses for
 *  teardown that can't meaningfully be retried from here. */
async function closeWindowByLabel(label: string): Promise<void> {
  try {
    const win = await Window.getByLabel(label);
    await win?.close();
  } catch {
    // Nothing more to do — the window is either already gone or unreachable.
  }
}
