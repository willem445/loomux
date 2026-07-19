// The "plugin" content-pane surface (#360 Slice D — see doc/design/pane-plugins.md
// and the multiwebview-embedding spike, fix/360-plugin-embed commit e337c95,
// findings comment on #360). Unlike GitView/FileEditView/WorkflowView, this view
// hosts NO DOM content of its own: a plugin's UI lives in its own isolated child
// `Webview`, embedded directly into the MAIN window via `Window::add_child`
// (Slice C's `plugin_open_window`) — a native region of the SAME top-level OS
// window, not a separate window and not a DOM node this view could append.
// "Hosting it as the pane's content" therefore means continuously repositioning
// and resizing that child webview to sit exactly over `this.el` — the pane's
// `.pane-content` box — for as long as the pane exists, and closing it the
// moment the pane does. The pure arithmetic for that lives in pluginwindow.ts
// (DOM-free, unit-tested); this file is the Tauri/DOM wiring, hand-validated per
// CLAUDE.md's convention for DOM wiring.
//
// Replaces an earlier overlay-window design (a separate top-level `WebviewWindow`
// tracked via absolute screen coordinates) that shipped as a floating, decorated
// OS window instead of embedded pane content — see the #360 multiwebview spike
// for why `Window::add_child` was chosen instead and what it fixes structurally.
//
// KNOWN, ACCEPTED GAPS (documented rather than engineered around, matching this
// repo's own precedent for a cosmetic-but-real limitation — see content-panes.md's
// "one known, accepted cosmetic gap"):
//   - A freshly-opened plugin webview that starts hidden (opened into a
//     currently-invisible pane — a hidden tab, a docked pane) may render for one
//     frame at a degenerate 1x1 size before `reposition()`'s first call hides it
//     (`add_child` has no `visible: false` builder option to suppress this the
//     way a `visible()` flag would) — far smaller than the overlay-window
//     design's equivalent gap (that one flashed at a full default size), but not
//     fully eliminated.
//   - Multi-monitor DPI: NOT a gap in this design — unlike the overlay-window
//     design (absolute screen coordinates, needing `main`'s own scale factor to
//     translate), `Window::add_child` positions relative to `main`'s own client
//     area, so there is no cross-monitor scale-factor math to get wrong at all.
//
// Z-order versus `main`'s OWN DOM content (context menus, tooltips, modals
// implemented as HTML/CSS) was a gap here (a child webview is a native
// surface compositing above `main`'s web content within its own bounds, and
// does not respect CSS z-index) — CLOSED on Windows by #391 (folded into this
// slice, `src-tauri/src/pluginregion.rs`): every time this view repositions,
// it also re-clips the plugin's own HWND (`setPluginFrame`, below) to punch a
// hole for whatever DOM overlay rects currently cover this pane
// (`overlaystate.ts`'s live registry + `pluginocclusion.ts`'s pure
// intersect/translate math), so `main`'s overlay renders over the plugin and
// stays interactive there, while the plugin still shows through everywhere
// else — not a global hide (the reverted PR #392's band-aid), a real
// per-region clip. See `pluginregion.rs`'s module doc comment for why
// WebView2 composition hosting was rejected as the mechanism, for the
// residual macOS/Linux gap this fix does NOT close (documented there, not
// silently dropped), and for the #380 amendment folding bounds into the same
// atomic call as the clip (`setPluginFrame` replaced the old separate
// `Webview.setPosition`/`setSize` + `setPluginOcclusion` sequence, which
// raced under the sessions sidebar's open animation).

import { Webview } from "@tauri-apps/api/webview";
import { dataDir, join } from "@tauri-apps/api/path";
import { openPluginWindow, closePluginWindow, setPluginFrame, type PluginCapability } from "./pluginbroker";
import { pluginErrorCode, pluginErrorMessage, type PluginManifest } from "./pluginhost";
import { pluginWebviewRect, pluginWindowShouldShow } from "./pluginwindow";
import { computeExcludeRects } from "./pluginocclusion";
import { overlayState, type OverlayChangeReason } from "./overlaystate";

/** Terse trigger label for `reposition()`, threaded all the way to
 *  `plugin_set_frame`'s breadcrumb (#380) — diagnostic only, never trusted
 *  for anything. `"resize"` covers BOTH a `ResizeObserver` tick on this
 *  pane's own element (a divider drag, a split, a maximize elsewhere) AND a
 *  tab switch (which surfaces as the same kind of size change — a hidden
 *  tab's pane going from `display:none`/zero-size to its real box — not a
 *  separate code path of its own; there's no dedicated tab-switch hook to
 *  distinguish it from any other resize). */
type RepositionSource = "resize" | "move-notify" | "overlay-open" | "overlay-close" | "overlay-poke" | "init";

/** `overlayState.subscribe`'s edge -> the breadcrumb label for it. `"poke"`
 *  (a covering overlay resizing/moving WHILE already open, without a fresh
 *  open/close edge of its own — `sessions.ts`'s `panelResizeObs` is its first
 *  production caller, #380 follow-up) maps to its OWN `"overlay-poke"`
 *  source rather than folding into `"overlay-open"`: an animated overlay's
 *  poke is a PER-FRAME burst for as long as it's transitioning, the same
 *  frequency class as a `"resize"` storm, not the rare discrete edge
 *  `"overlay-open"`/`"overlay-close"` are — `pluginregion.rs`'s
 *  `should_log_frame` gates `"overlay-poke"` identically to `"resize"`
 *  (logs only on an actual exclude change or a native failure) for exactly
 *  that reason; folding it into `"overlay-open"` would have reintroduced the
 *  per-frame breadcrumb storm that source's own "always logs" gating is
 *  built to avoid, just triggered by the overlay's geometry instead of the
 *  pane's own. */
function overlayReasonToSource(reason: OverlayChangeReason): RepositionSource {
  if (reason === "close") return "overlay-close";
  if (reason === "poke") return "overlay-poke";
  return "overlay-open";
}

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
  private pluginWebview: Webview | null = null;
  private resizeObs: ResizeObserver;
  /** Whether the plugin webview is CURRENTLY shown — tracked so a resize-to-zero
   *  followed by a resize-back-to-nonzero only calls `.show()`/`.hide()` on an
   *  actual transition, not on every observer tick (each is a real IPC round
   *  trip to the backend). */
  private shown = false;
  private disposed = false;
  /** True once `openPluginWindow` has resolved successfully — guards the
   *  ResizeObserver from trying to position a webview that doesn't exist yet
   *  (still opening) or any more (failed, or disposed mid-open). */
  private ready = false;
  /** Monotonic token so a `reposition()` call that's still awaiting the
   *  backend (`plugin_set_frame` is one IPC round trip, #380) can't clobber a
   *  NEWER call's result if a rapid burst of resize events fires before the
   *  first one finishes — a divider dragged fast enough would otherwise
   *  flicker the webview back to a stale position after it had already
   *  caught up. This only guards which call's `show()`/`hide()` follow-up
   *  runs; it does NOT need to guard the frame IPC call itself; see
   *  `reposition()`'s own doc comment for why that's no longer racy. */
  private repositionSeq = 0;
  /** Unsubscribe from the shared overlay registry (overlaystate.ts, #391) —
   *  set in `show()`, released in `dispose()`. Null before `show()` runs and
   *  after `dispose()` has (idempotency guard: `dispose()` can run more than
   *  once via the `disposed` flag below, but must only unsubscribe once). */
  private overlayUnsub: (() => void) | null = null;
  /** Bound so `removeEventListener` in `dispose()` matches the exact function
   *  reference `addEventListener` in `show()` registered. */
  private readonly onWindowResize = () => void this.reposition("resize");

  constructor(host: PluginPaneHost) {
    this.manifest = host.manifest;
    this.el = document.createElement("div");
    this.el.className = "pane-plugin";
    // Untrusted text (manifest `name`) — textContent only, never innerHTML. This
    // label is the ONLY thing painted in `el` on success: the plugin's real
    // content is the separate child webview this view positions over `el`, so
    // once that webview is up this text sits harmlessly underneath it.
    this.statusEl = document.createElement("div");
    this.statusEl.className = "pane-plugin-status";
    this.statusEl.textContent = `Opening ${this.manifest.displayName}…`;
    this.el.appendChild(this.statusEl);
    this.resizeObs = new ResizeObserver(() => this.reposition("resize"));
  }

  /** Open the plugin's child webview and start tracking this pane's box. Safe to
   *  call only once `el` is attached to the document (startContent's "ATTACH,
   *  THEN show" contract) — the first `reposition()` reads a real layout.
   *  `Window::add_child` positions the webview relative to the main window's
   *  OWN client area, so a window move changes nothing for the webview's own
   *  position/size, and a window resize only matters to THAT insofar as it
   *  resizes `el` itself — which the ResizeObserver below already watches
   *  directly, same as before #391. The `window.resize` listener added here
   *  is for a DIFFERENT reason (#391, folded into this slice): an open DOM
   *  overlay that's docked to the window edge (the sessions sidebar) can move
   *  when the window resizes WITHOUT `el` itself changing size — the overlay
   *  registry has no way to know that on its own, so this view re-runs
   *  `reposition()` (which recomputes occlusion every time, below) on every
   *  window resize to catch it. */
  show(): void {
    this.resizeObs.observe(this.el);
    // #391 (folded into #380): a loomux DOM overlay opening/closing over this
    // pane's screen region doesn't change `el`'s own rect at all — nothing
    // else here would notice it — so `reposition()` (which recomputes and
    // re-sends occlusion every call, below) is re-run on every open/close
    // edge of the shared registry, immediately, and again on every window
    // resize (see this method's own doc comment) to catch a docked overlay
    // moving without `el` itself resizing.
    this.overlayUnsub = overlayState.subscribe((reason) => void this.reposition(overlayReasonToSource(reason)));
    window.addEventListener("resize", this.onWindowResize);
    void this.open();
  }

  private async open(): Promise<void> {
    const m = this.manifest;
    // Best-effort initial box from the pane's own current rect — floored at 1px
    // by pluginWebviewRect, so even a pane that happens to be zero-sized right
    // now (opened into a hidden tab) gets a valid (if degenerate) request rather
    // than a special-cased fallback size. Unlike the overlay-window design this
    // replaces, this IS the webview's real initial position (add_child places it
    // there directly), not just a size passed to an OS-placed window — so a pane
    // that's already visible when opened gets its plugin embedded in the right
    // spot on the very first frame, no flash-then-correct step needed.
    const rect = this.el.getBoundingClientRect();
    const webviewRect = pluginWebviewRect(rect);
    try {
      const label = await openPluginWindow({
        pluginId: m.pluginId,
        entry: m.entry,
        root: m.root ?? undefined,
        capabilities: toPluginCapabilities(m.capabilities),
        apiVersion: m.apiVersion,
        x: webviewRect.x,
        y: webviewRect.y,
        width: webviewRect.width,
        height: webviewRect.height,
      });
      if (this.disposed) {
        // The pane closed while the webview was still opening — don't leak it.
        void closeWebviewByLabel(label);
        return;
      }
      this.windowLabel = label;
      this.pluginWebview = await Webview.getByLabel(label);
      if (this.disposed) {
        void closeWebviewByLabel(label);
        return;
      }
      this.ready = true;
      this.statusEl.hidden = true;
      await this.reposition("init");
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
   *  child webview to match — and, since #391 (folded into #380), re-clip its
   *  native occlusion (below) so it stays correct across every one of the
   *  same triggers. Called on every layout change that could move `el` — a
   *  divider drag, a split, a tab switch, a maximize elsewhere — via the
   *  ResizeObserver registered in `show()`, plus every open/close edge of the
   *  shared overlay registry and every window resize (also wired in `show()`).
   *  A no-op until `open()` has a webview to move (`ready`).
   *
   *  Bounds and occlusion are ONE atomic backend call (`setPluginFrame` ->
   *  `plugin_set_frame`, #380) built from a SINGLE fresh `rect` read — not
   *  the old three-call sequence (`Webview.setPosition`, `setSize`, then a
   *  separate `plugin_set_occlusion`), which raced: those two built-in
   *  webview commands are `async`-dispatched by Tauri and their handler is
   *  fire-and-forget onto the main event loop (the awaited JS promise
   *  resolves once the native call is *queued*, not once it's applied), so
   *  the old `plugin_set_occlusion` — a plain, inline-executing command —
   *  could run and read the webview's client rect BEFORE a "completed"
   *  resize had actually landed, clipping against stale geometry (see
   *  `pluginregion.rs`'s module doc comment for the full mechanism this was
   *  proved against). Folding both into one command removes that gap AND the
   *  matching one between concurrent `reposition()` calls: WebView2's IPC
   *  dispatch processes single synchronous commands strictly in arrival
   *  order, so a burst of `ResizeObserver` ticks (the sessions sidebar's
   *  240ms open/close transition, `styles.css`'s `#sessions` `width`
   *  transition, is the trigger that surfaced this live) can no longer let
   *  an older call's now-orphaned write land after a newer one's. `seq`
   *  below still guards `show()`/`hide()` follow-up against a stale call
   *  finishing after a newer one already ran — that part remains genuinely
   *  async (its own IPC round trip) and unordered relative to a newer
   *  `reposition()`'s OWN frame call. */
  private async reposition(source: RepositionSource): Promise<void> {
    if (!this.ready || !this.pluginWebview || this.disposed) return;
    const seq = ++this.repositionSeq;
    const rect = this.el.getBoundingClientRect();
    if (!pluginWindowShouldShow(rect)) {
      if (this.shown) {
        this.shown = false;
        await this.pluginWebview.hide().catch(() => {});
      }
      return;
    }
    try {
      if (this.windowLabel) {
        const webviewRect = pluginWebviewRect(rect);
        const exclude = computeExcludeRects(rect, overlayState.currentRects());
        await setPluginFrame(
          this.windowLabel,
          webviewRect.x,
          webviewRect.y,
          webviewRect.width,
          webviewRect.height,
          exclude,
          source
        ).catch(() => {});
      }
      if (seq !== this.repositionSeq || this.disposed || !this.pluginWebview) return;
      if (!this.shown) {
        this.shown = true;
        await this.pluginWebview.show();
      }
    } catch {
      // Best-effort: a reposition racing pane teardown (webview already closing)
      // must not throw into the ResizeObserver callback.
    }
  }

  /** `Pane.notifyMoved()`'s forward target (#380): re-run the position/size/
   *  occlusion sync after this pane's element relocated to a new slot WITHOUT
   *  a size change — a drag-reorder swap, or any other position-only move
   *  `Grid`'s `syncMovedPanes` backstop catches. `reposition()`'s own three
   *  triggers (the `ResizeObserver` on `el`, the overlay subscription, the
   *  window `resize` listener) all miss this case: none of them fire on a
   *  same-size DOM move, which is exactly why the child webview was left
   *  painted at its pre-swap screen position, over whatever pane is there
   *  now, while this pane's own box went blank. */
  notifyMoved(): void {
    void this.reposition("move-notify");
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.resizeObs.disconnect();
    window.removeEventListener("resize", this.onWindowResize);
    this.overlayUnsub?.();
    this.overlayUnsub = null;
    if (this.windowLabel) void closeWebviewByLabel(this.windowLabel);
  }
}

/** Close a plugin's child webview by label, best-effort — mirrors the
 *  fire-and-forget `killPty(...).catch(() => {})` posture the rest of this
 *  codebase uses for teardown that can't meaningfully be retried from here.
 *  Goes through `plugin_close_window` (not a raw `Webview.close()` call) so
 *  the broker's session/channel/procmetrics-poll-thread state is released
 *  too — see that command's doc comment for why an explicit close is needed
 *  at all (a child webview never fires `WindowEvent::Destroyed`). */
async function closeWebviewByLabel(label: string): Promise<void> {
  try {
    await closePluginWindow(label);
  } catch {
    // Nothing more to do — the webview is either already gone or unreachable.
  }
}
