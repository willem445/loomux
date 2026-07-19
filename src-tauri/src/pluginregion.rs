//! Per-overlay occlusion for a plugin's embedded child webview (#391, folded
//! into #380 — the corrected root-cause fix superseding the reverted
//! global-hide band-aid, PR #392, reverted at d3333b3; see
//! doc/design/pane-plugins.md's `#391` amendment for the full writeup).
//!
//! **Root cause.** A plugin is a `Window::add_child` webview
//! (`pluginbroker::plugin_open_window`) — on Windows a real, windowed child
//! `HWND` that always paints ABOVE every other pixel `main`'s own window
//! produces and swallows every pointer event over its own rect, unconditionally
//! (basic Win32 z-order: a `WS_CHILD` window always composites above whatever
//! its parent draws in its own `WM_PAINT`; there is no z-index knob for that).
//! `main`'s DOM overlays (the sessions sidebar, modals, context menus, …) are
//! painted BY `main`'s own webview surface, not as separate OS surfaces, so
//! whichever one is "on top" wins for the WHOLE overlapping rect — the DOM
//! cannot interleave with an always-on-top native window at the level of
//! individual elements.
//!
//! **Composition-hosting spike (rejected, no fork needed to reach that
//! verdict).** WebView2 has a windowless "composition" mode
//! (`ICoreWebView2CompositionController`, backed by `Windows.UI.Composition`/
//! DirectComposition) that would let a webview's rendered content be placed as
//! a visual in a host-owned compositor tree instead of a native child `HWND`.
//! `webview2-com`'s own bindings already expose the full COM surface for it
//! (`webview2-com-sys`'s `declared_interfaces.rs` lists
//! `ICoreWebView2CreateCoreWebView2CompositionControllerCompletedHandler`) —
//! but `wry` 0.55.1's Windows backend (`wry::webview2::mod.rs`) never calls
//! into it: `create_controller` hardcodes the windowed
//! `CreateCoreWebView2Controller`/`CreateCoreWebView2ControllerWithOptions`
//! path unconditionally. Composition hosting is therefore unreachable through
//! Tauri/wry's public API and would need a wry fork to add at all.
//!
//! That fork would not even solve THIS problem, though, which is the real
//! reason composition hosting is rejected rather than merely deferred:
//! composition-hosting the PLUGIN alone only changes whether ITS visual sits
//! above or below `main`'s ENTIRE webview surface as one opaque unit — it
//! still can't interleave with content `main` paints INSIDE its own single
//! surface (the sessions sidebar, a modal, …) without ALSO composition-hosting
//! `main` end-to-end, sharing one compositor device, and the host app driving
//! per-region hit-test routing itself (`SendMouseInput`/`SendPointerInput` to
//! whichever controller owns a given point — WebView2 does not do this
//! automatically for windowless content). That is a full rewrite of Tauri's
//! Windows windowing backend affecting every window in the app, not a
//! per-plugin opt-in, and it would still need the exact same
//! "which rects does the overlay cover right now" computation this module
//! does — just wrapped in DirectComposition instead of `SetWindowRgn`.
//!
//! **The fix.** Region-clip the plugin's own child `HWND` — the same
//! technique browsers used for windowed-plugin/DOM coexistence (NPAPI/ActiveX
//! windowed plugins had this identical problem). `Webview::with_webview` (a
//! STABLE Tauri v2 API, not multiwebview/`unstable`) hands back
//! [`tauri::webview::PlatformWebview::controller()`] — the real
//! `ICoreWebView2Controller` — and `ICoreWebView2Controller::ParentWindow`
//! returns exactly the container `HWND` `wry` created for this one embedded
//! webview (`webview2::mod.rs`'s `create_container_hwnd`, passed as the
//! controller's own parent at creation — confirmed by reading that source).
//! [`SetWindowRgn`] on that `HWND`, minus the rects of every DOM overlay
//! currently covering this pane, makes the plugin stop painting AND stop
//! receiving pointer input in exactly those rects — both paint and hit-test
//! fall through to whatever `main` draws underneath, which is the real
//! "compose like a DOM element" behavior the human asked for, not a global
//! hide. No wry fork, no new crate: `windows` is already a dependency (see
//! `Cargo.toml`'s getrandom note — this only adds the `Win32_Graphics_Gdi`/
//! `Win32_UI_HiDpi` feature flags to it, zero new crates in the tree).
//!
//! **Coordinates.** The frontend (`pluginocclusion.ts`, DOM-free/tested)
//! computes, for a given plugin pane, the intersection of every open overlay's
//! rect with the pane's own rect, translated into the PANE's own top-left
//! origin — logical pixels, the same convention
//! `pluginbroker::OpenPluginWindowRequest` already uses. This module converts
//! those to physical pixels via `GetDpiForWindow` on the SAME container `HWND`
//! (mirroring exactly the conversion `wry` itself does when it first sizes
//! that `HWND` — `hwnd_dpi`/`scale_factor` in `webview2::mod.rs`) and builds
//! the base region from the `HWND`'s OWN LIVE `GetClientRect`, not a size the
//! frontend has to pass and keep in sync — the region is always correct for
//! whatever size `reposition()` last set, even if this call races a resize.
//!
//! **ACL isolation is untouched.** This module clips OS-level painting/hit-testing
//! on an `HWND` — it never touches the webview's label, capability
//! resolution, or the broker's per-session state (`pluginbroker.rs`). The two
//! are orthogonal; `tests/acl_manifest.rs`'s guards are unaffected by this file.
//!
//! **Cross-platform.** Windows-only implementation. On macOS/Linux the SAME
//! root cause applies (`add_child`'s child webview is a peer `NSView`/
//! `GtkWidget` of `main`'s own webview there too, and `main`'s overlays are
//! painted inside main's own single surface on those platforms exactly the
//! same way — see the root-cause section above, which is platform-agnostic),
//! so a region-clip equivalent (`CALayer` masking on macOS, GDK shape/input
//! regions on GTK) would need separate platform-specific code this PR does
//! NOT add: unverified native GUI code for a platform this workspace cannot
//! build or interactively test (Windows-only dev environment, per CLAUDE.md),
//! and CI's macOS/Ubuntu builds don't exercise the overlay-over-plugin
//! scenario either, so a from-scratch implementation would ship unverified by
//! construction. [`apply_frame`]'s non-Windows arm is a documented no-op —
//! the SAME pre-existing bleed as before this PR, not a regression, tracked as
//! a follow-up rather than shipped unverified.
//!
//! **#380 amendment: `plugin_set_occlusion` → [`plugin_set_frame`], folding in
//! bounds.** The #391 fix above shipped, then broke live under one specific
//! trigger: opening the sessions sidebar (`#sessions`'s `width: 0 -> 344px`
//! CSS transition, `styles.css`) while a plugin pane was visible let the
//! plugin paint back over the sidebar. Root cause, proved by reading the
//! actual Tauri/wry dispatch code this app is pinned to (not by guessing):
//! `pluginpaneview.ts`'s old `reposition()` called THREE separate IPC
//! commands in sequence — `Webview::setPosition`/`setSize` (Tauri's own
//! built-in webview commands, `core:webview:allow-set-webview-position`/
//! `-size`), then this module's `plugin_set_occlusion`. The built-in position/
//! size commands are declared `async` (`tauri-2.11.5/src/webview/plugin.rs`'s
//! `setter!` macro), so Tauri dispatches them onto the async runtime's
//! threadpool; their body calls `Dispatch::set_bounds`, which
//! (`tauri-runtime-wry-2.11.4/src/lib.rs`'s `send_user_message`) checks
//! `current_thread().id() == main_thread_id` — false, from a threadpool
//! worker — and takes the ELSE branch: `context.proxy.send_event(message)`,
//! a **fire-and-forget** post to the winit/tao event loop's user-event queue.
//! The awaited JS promise resolves the instant that message is *enqueued*,
//! not once the window has actually moved/resized. `plugin_set_occlusion`,
//! by contrast, is a plain (non-`async`) `#[tauri::command]`, which Tauri's
//! macro (`tauri-macros-2.6.3`'s `body_blocking`) runs INLINE, synchronously,
//! on whatever thread dispatched the IPC call — for a call from `main`'s own
//! webview, that's the WebView2 UI/main thread itself, via a completely
//! separate dispatch path from winit's user-event queue (no shared ordering
//! guarantee between the two). Net effect: `plugin_set_occlusion` could run —
//! and read the child `HWND`'s `GetClientRect` — BEFORE the just-awaited
//! `setPosition`/`setSize` calls had actually reached the main thread's event
//! queue, building a clip region against the OLD size/position while the
//! frontend's `computeExcludeRects` had already translated the DOM overlay's
//! rect into the NEW pane origin — a genuine mismatch, not just a rounding
//! quirk. Nothing then re-applied the region once the (also fire-and-forget,
//! also unordered relative to EACH OTHER across rapid `ResizeObserver` ticks)
//! resize eventually landed, so the plugin stayed misaligned until some
//! unrelated later event forced a fresh `reposition()` — matching both live
//! reports ("glitches for a few seconds before correcting").
//!
//! **The fix: fold bounds into the same synchronous command as the clip.**
//! [`plugin_set_frame`] replaces `plugin_set_occlusion` outright (its only
//! caller, `pluginpaneview.ts`, is updated in the same change) and stays a
//! plain, non-`async` command — so it keeps running inline on the calling
//! thread — but now ALSO sets the webview's bounds itself, via
//! `tauri::Webview::set_bounds` (the same `Dispatch::set_bounds` the built-in
//! commands use), called from THIS synchronous context. Because THIS call
//! originates on the main thread already (per the `body_blocking` behavior
//! above), `send_user_message` takes the FAST branch —
//! `handle_user_message(...)` runs immediately, not queued — so the resize
//! is applied before this same function goes on to read `GetClientRect` and
//! build the clip region a few lines later, with no `await`, no thread hop,
//! and no other command able to interleave between the two: one IPC round
//! trip, one synchronous sequence, atomic by construction rather than by
//! convention. This also collapses the earlier three-call race between
//! *concurrent* `reposition()` invocations (a burst of `ResizeObserver` ticks
//! during the sidebar's 240ms transition): since each is now a single
//! synchronous command, WebView2's IPC dispatch processes them strictly in
//! arrival order — there is no longer a window for an older call's
//! now-orphaned position write to land after a newer call's.
//!
//! Every application CAN log a breadcrumb (`crate::obs::breadcrumb`,
//! `"pluginregion"`) recording the trigger source the frontend passed
//! through, the bounds and exclude-rect count applied, and whether the
//! native calls succeeded — but NOT every application DOES: `"resize"` is
//! `pluginpaneview.ts`'s `ResizeObserver`/window-resize source, the actual
//! high-frequency one (a divider drag or the sidebar's own 240ms transition
//! fires a burst of these), and the common case for it is no overlay
//! covering the pane at all — logging every one of those would mean
//! per-frame synchronous file I/O on the main thread, in the exact hot path
//! this fix exists to keep smooth, for a case (`exclude` unchanged, usually
//! empty) that tells a reader nothing new. [`should_log_frame`] (pure,
//! unit-tested below) gates on it: a `"resize"` call only breadcrumbs when
//! the exclude set actually differs from the last one logged FOR THIS LABEL
//! (`LAST_LOGGED_EXCLUDE`, cleared on `plugin_close_window` via
//! [`on_plugin_webview_closed`]) or a native call failed; every other
//! trigger (`overlay-open`/`overlay-close`/`move-notify`/`init`, and any
//! future value) is comparatively rare and always logs — those edges ARE
//! the diagnostic signal a live occurrence needs, not storm noise. The
//! native calls themselves (bounds + region) always run regardless of
//! whether this application gets logged — only the breadcrumb is gated.
//!
//! **`"overlay-poke"` (#380 follow-up): a second high-frequency source, gated
//! the same way as `"resize"`.** `overlaystate.ts`'s `poke()` lets a DOM
//! overlay that keeps moving/resizing WHILE OPEN (the sessions sidebar's own
//! `width` transition — see `sessions.ts`'s `panelResizeObs`) force every
//! subscribed plugin pane to recompute, on every tick of THAT transition —
//! the same per-frame frequency class as a `ResizeObserver` burst on the
//! pane's own element, just driven by a covering overlay's geometry instead
//! of the pane's own. Mapping it to `"overlay-open"` (an always-logs,
//! "comparatively rare" source) would reintroduce the exact per-frame
//! file-I/O storm `"resize"`'s gate exists to prevent, just from the other
//! direction. `should_log_frame` therefore gates `"overlay-poke"` identically
//! to `"resize"`: logs only when `exclude_changed` or a native call failed.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Deserialize;

use crate::obs::LockExt;

/// A DOM overlay's intersection with a plugin pane, already translated to the
/// pane's own top-left origin — logical pixels, matching
/// `pluginbroker::OpenPluginWindowRequest`'s coordinate convention. The pure
/// intersect/translate arithmetic lives in the frontend's `pluginocclusion.ts`
/// (DOM-free, unit-tested) — this module receives only the final list and
/// does no geometry of its own beyond the logical->physical DPI conversion.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct OcclusionRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Same guard `plugin_close_window` uses (`pluginbroker.rs`): main is fully
/// trusted already, so this isn't a security boundary — just a backstop
/// against a stray bug in main's own JS clipping something that isn't a
/// plugin webview. Split out as a pure `fn` (mirrors
/// `pluginbroker::validate_open_request`'s own reasoning for why validation
/// lives separately from the `#[tauri::command]` body) so it's testable
/// without a real `AppHandle`/webview.
fn validate_label(label: &str) -> Result<(), String> {
    if !label.starts_with("plugin-") {
        return Err(format!("refusing to clip non-plugin webview label: {label}"));
    }
    Ok(())
}

/// The exclude set last BREADCRUMBED (not last applied — every application
/// is still applied natively regardless of logging, see this module's doc
/// comment) for each `plugin-*` label — [`should_log_frame`]'s state. Same
/// poison-tolerant `Mutex<Option<HashMap<...>>>` shape `pluginbroker.rs`'s
/// `PLUGIN_SESSIONS`/`PLUGIN_CHANNELS` use. Cleared on close
/// ([`on_plugin_webview_closed`]) so a label reused after a plugin
/// closes/reopens starts fresh rather than comparing against a stale
/// session's geometry.
static LAST_LOGGED_EXCLUDE: Mutex<Option<HashMap<String, Vec<OcclusionRect>>>> = Mutex::new(None);

fn with_last_logged_exclude<R>(f: impl FnOnce(&mut HashMap<String, Vec<OcclusionRect>>) -> R) -> R {
    let mut guard = LAST_LOGGED_EXCLUDE.lock_safe();
    f(guard.get_or_insert_with(HashMap::new))
}

/// Releases `label`'s entry in [`LAST_LOGGED_EXCLUDE`] — called from
/// `pluginbroker::plugin_close_window`, the same explicit-close hook that
/// already releases the broker's own `PluginSession`/channel state and
/// `procmetrics`'s poll thread (a child webview never fires
/// `WindowEvent::Destroyed` for anything to clean up on its own — see that
/// command's doc comment).
pub fn on_plugin_webview_closed(label: &str) {
    with_last_logged_exclude(|m| {
        m.remove(label);
    });
}

/// Whether THIS application's exclude set differs from the last one
/// breadcrumbed for its label — the ONLY thing a `"resize"` source gates on
/// (see [`should_log_frame`]): comparing bounds too would defeat the point,
/// since a pane's own bounds change on nearly every tick of a genuine
/// resize/drag regardless of whether any overlay covers it. Pure so it's
/// unit-tested without a real label/webview.
fn exclude_changed(last_logged: Option<&[OcclusionRect]>, current: &[OcclusionRect]) -> bool {
    last_logged != Some(current)
}

/// Whether this `plugin_set_frame` application is worth a breadcrumb line
/// (see this module's doc comment's "Telemetry" section for the full
/// rationale). `native_ok` is false for ANY failure (bounds not applied,
/// HWND/region lookup failed, `SetWindowRgn` itself failed) — always logged,
/// regardless of source, since a failure is exactly what the next live
/// occurrence needs evidence of. Otherwise: `"resize"` and `"overlay-poke"`
/// (the two actual storm sources — a divider drag or the sidebar's own CSS
/// transition fires a burst of the former from the pane's own
/// `ResizeObserver`, the latter from `overlaystate.ts`'s `poke()` on that
/// SAME transition, see this module's doc comment's `"overlay-poke"`
/// section) only log when `exclude_changed`; every other source
/// (`"overlay-open"`/`"overlay-close"`/`"move-notify"`/`"init"`, and
/// anything not yet named) is a comparatively rare, discrete event and
/// always logs. Pure so it's unit-tested without a real command/webview.
fn should_log_frame(source: &str, exclude_changed: bool, native_ok: bool) -> bool {
    if !native_ok {
        return true;
    }
    if source != "resize" && source != "overlay-poke" {
        return true;
    }
    exclude_changed
}

/// Main-only (mirrors `plugin_open_window`/`plugin_close_window` — see
/// `permissions/sets/misc.toml`; never granted to a plugin's own webview).
/// Sets the plugin child webview's bounds AND its native occlusion clip in
/// ONE synchronous command — see this module's doc comment's "#380
/// amendment" for why folding these together (rather than calling `Webview::
/// setPosition`/`setSize` separately, as `pluginpaneview.ts` used to) is the
/// actual fix, not a stylistic simplification. `source` is an opaque label
/// the frontend passes through for the breadcrumb below (`resize` |
/// `move-notify` | `overlay-open` | `overlay-close` | `overlay-poke`,
/// `pluginpaneview.ts`'s `RepositionSource`) — never validated against a closed enum, since it
/// only ever ends up in a log line and main is fully trusted already (see
/// `validate_label`). `pluginpaneview.ts`'s `reposition()` calls this every
/// time this pane's box or the set of covering overlays could have changed.
/// Best-effort if the webview is already gone — a call racing pane teardown
/// must not surface as an error the frontend has to handle specially.
#[tauri::command]
pub fn plugin_set_frame(
    app: tauri::AppHandle,
    label: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    exclude: Vec<OcclusionRect>,
    source: String,
) -> Result<(), String> {
    validate_label(&label)?;
    let Some(webview) = tauri::Manager::get_webview(&app, &label) else {
        crate::obs::breadcrumb(
            "pluginregion",
            &format!("plugin_set_frame: label={label} src={source} skipped (webview gone)"),
        );
        return Ok(());
    };

    // Bounds BEFORE the clip, in the SAME synchronous call: `set_bounds`
    // (`Dispatch::set_bounds`) called from this non-async command runs
    // inline on the calling (main) thread, so this completes before
    // `apply_frame` below reads the HWND's client rect a few lines later —
    // see this module's doc comment for why that ordering is the entire fix.
    let moved = webview.set_bounds(tauri::Rect {
        position: tauri::Position::Logical(tauri::LogicalPosition::new(x, y)),
        size: tauri::Size::Logical(tauri::LogicalSize::new(width, height)),
    });
    let moved_ok = moved.is_ok();

    webview
        .with_webview(move |platform| {
            apply_frame(platform, &label, (x, y, width, height), &exclude, &source, moved_ok)
        })
        .map_err(|e| e.to_string())
}

/// Converts one logical-pixel exclude rect into the physical-pixel `(left,
/// top, right, bottom)` bounds `CreateRectRgn` expects, at the given
/// DPI-derived scale factor. Pure arithmetic, split out from
/// `apply_frame` so the rounding behavior at fractional DPI scales
/// (125%/150%, not just 100%/200%) is unit-tested without a real `HWND`.
/// `cfg(windows)`-only (like `apply_frame` itself): the physical-pixel
/// conversion only means anything against a real Win32 `HWND`/GDI region.
#[cfg(windows)]
fn physical_bounds(r: OcclusionRect, scale: f64) -> (i32, i32, i32, i32) {
    (
        (r.x * scale).round() as i32,
        (r.y * scale).round() as i32,
        ((r.x + r.width) * scale).round() as i32,
        ((r.y + r.height) * scale).round() as i32,
    )
}

#[cfg(windows)]
fn apply_frame(
    platform: tauri::webview::PlatformWebview,
    label: &str,
    (x, y, width, height): (f64, f64, f64, f64),
    exclude: &[OcclusionRect],
    source: &str,
    moved: bool,
) {
    // `windows061`, not the crate's own `windows` dep: see Cargo.toml's
    // comment on the `windows061` alias for why this file specifically needs
    // the SAME `windows` version Tauri's own WebView2 interop
    // (`PlatformWebview::controller()`) is built against.
    use windows061::Win32::Foundation::{HWND, RECT};
    use windows061::Win32::Graphics::Gdi::{
        CombineRgn, CreateRectRgn, DeleteObject, SetWindowRgn, RGN_DIFF,
    };
    use windows061::Win32::UI::HiDpi::GetDpiForWindow;
    use windows061::Win32::UI::WindowsAndMessaging::GetClientRect;

    let bounds_label = format!("({x:.0},{y:.0},{width:.0}x{height:.0})");

    let controller = platform.controller();
    // `ParentWindow` returns the container HWND `wry` creates per embedded
    // webview (`webview2::mod.rs`'s `create_container_hwnd`, passed as the
    // controller's own parent at creation — see this module's doc comment) —
    // sized/positioned to exactly this plugin's pane box, so clipping IT
    // clips everything WebView2 renders beneath it.
    let mut hwnd = HWND::default();
    if unsafe { controller.ParentWindow(&mut hwnd as *mut HWND) }.is_err() || hwnd.is_invalid() {
        crate::obs::breadcrumb(
            "pluginregion",
            &format!(
                "plugin_set_frame: label={label} src={source} bounds={bounds_label} moved={moved} skipped (no hwnd)"
            ),
        );
        return;
    }

    // No overlay currently covers this pane — remove any prior clip so the
    // webview paints (and hit-tests) its full box again. `None` means "no
    // region at all" for SetWindowRgn's `hrgn` param, i.e. the whole window.
    let region_applied = if exclude.is_empty() {
        unsafe { SetWindowRgn(hwnd, None, true) }
    } else {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale = dpi as f64 / 96.0;

        let mut client = RECT::default();
        if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
            crate::obs::breadcrumb(
                "pluginregion",
                &format!(
                    "plugin_set_frame: label={label} src={source} bounds={bounds_label} moved={moved} exclude={} skipped (no client rect)",
                    exclude.len()
                ),
            );
            return;
        }
        let base = unsafe { CreateRectRgn(0, 0, client.right, client.bottom) };
        if base.is_invalid() {
            crate::obs::breadcrumb(
                "pluginregion",
                &format!(
                    "plugin_set_frame: label={label} src={source} bounds={bounds_label} moved={moved} exclude={} skipped (region create failed)",
                    exclude.len()
                ),
            );
            return;
        }
        for r in exclude {
            let (x0, y0, x1, y1) = physical_bounds(*r, scale);
            let hole = unsafe { CreateRectRgn(x0, y0, x1, y1) };
            if hole.is_invalid() {
                continue;
            }
            unsafe {
                CombineRgn(Some(base), Some(base), Some(hole), RGN_DIFF);
                let _ = DeleteObject(hole.into());
            }
        }

        // SetWindowRgn takes ownership of `base` ON SUCCESS (MSDN: "the
        // system owns the region... do not make any further calls with this
        // handle, in particular do not delete it" — it also auto-deletes
        // whatever region was previously set, so there is nothing to release
        // across repeated calls). Only on FAILURE do we still own `base` and
        // must clean it up ourselves, or every failed call leaks one GDI
        // region handle — a real concern here since this runs on every
        // overlay open/close/resize, not a one-shot.
        let applied = unsafe { SetWindowRgn(hwnd, Some(base), true) };
        if applied == 0 {
            unsafe {
                let _ = DeleteObject(base.into());
            }
        }
        applied
    };

    let native_ok = moved && region_applied != 0;
    let changed = with_last_logged_exclude(|m| {
        let changed = exclude_changed(m.get(label).map(Vec::as_slice), exclude);
        m.insert(label.to_string(), exclude.to_vec());
        changed
    });
    if should_log_frame(source, changed, native_ok) {
        crate::obs::breadcrumb(
            "pluginregion",
            &format!(
                "plugin_set_frame: label={label} src={source} bounds={bounds_label} moved={moved} exclude={} region_applied={}",
                exclude.len(),
                region_applied != 0
            ),
        );
    }
}

#[cfg(not(windows))]
fn apply_frame(
    _platform: tauri::webview::PlatformWebview,
    label: &str,
    (x, y, width, height): (f64, f64, f64, f64),
    exclude: &[OcclusionRect],
    source: &str,
    moved: bool,
) {
    // Not implemented on macOS/Linux — see this module's doc comment's
    // "Cross-platform" section for why: the same root cause applies there,
    // but fixing it needs separate, unverifiable-from-here native code
    // (CALayer masking / GDK shape regions), tracked as a follow-up rather
    // than shipped unverified. This is a no-op, not a regression: nothing
    // clipped the plugin webview on these platforms before this PR either.
    // Still breadcrumbed (gated the same way the Windows arm is — see
    // `should_log_frame`), so a diagnostic trail exists on every platform
    // without spamming a resize storm there either.
    let changed = with_last_logged_exclude(|m| {
        let changed = exclude_changed(m.get(label).map(Vec::as_slice), exclude);
        m.insert(label.to_string(), exclude.to_vec());
        changed
    });
    if should_log_frame(source, changed, moved) {
        crate::obs::breadcrumb(
            "pluginregion",
            &format!(
                "plugin_set_frame: label={label} src={source} bounds=({x:.0},{y:.0},{width:.0}x{height:.0}) moved={moved} exclude={} skipped (non-windows, no-op)",
                exclude.len()
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_label_accepts_plugin_prefixed_labels() {
        assert!(validate_label("plugin-demo-0").is_ok());
    }

    #[test]
    fn validate_label_rejects_non_plugin_labels() {
        let err = validate_label("main").unwrap_err();
        assert!(err.contains("main"));
        assert!(validate_label("").is_err());
        assert!(validate_label("untrusted-probe-0").is_err());
    }

    const RECT_A: OcclusionRect = OcclusionRect { x: 0.0, y: 0.0, width: 10.0, height: 10.0 };
    const RECT_B: OcclusionRect = OcclusionRect { x: 5.0, y: 5.0, width: 20.0, height: 20.0 };

    #[test]
    fn exclude_changed_is_false_for_the_common_no_overlay_resize_storm() {
        // The case NB-1 exists for: a divider drag with nothing covering the
        // pane keeps `exclude` empty on every tick — must NOT read as changed.
        assert!(!exclude_changed(Some(&[]), &[]));
        assert!(!exclude_changed(Some(&[RECT_A]), &[RECT_A]));
    }

    #[test]
    fn exclude_changed_is_true_the_first_time_a_label_is_seen() {
        assert!(exclude_changed(None, &[]));
        assert!(exclude_changed(None, &[RECT_A]));
    }

    #[test]
    fn exclude_changed_is_true_when_the_set_or_a_rect_differs() {
        assert!(exclude_changed(Some(&[RECT_A]), &[]));
        assert!(exclude_changed(Some(&[]), &[RECT_A]));
        assert!(exclude_changed(Some(&[RECT_A]), &[RECT_B]));
        assert!(exclude_changed(Some(&[RECT_A]), &[RECT_A, RECT_B]));
    }

    #[test]
    fn should_log_frame_always_logs_a_native_failure_regardless_of_source_or_change() {
        assert!(should_log_frame("resize", false, false));
        assert!(should_log_frame("overlay-open", false, false));
        assert!(should_log_frame("overlay-poke", false, false));
    }

    #[test]
    fn should_log_frame_gates_resize_on_exclude_change_only() {
        // The actual storm source: unchanged exclude (the common no-overlay
        // case) is exactly what must NOT log, or NB-1 recurs.
        assert!(!should_log_frame("resize", false, true));
        assert!(should_log_frame("resize", true, true));
    }

    #[test]
    fn should_log_frame_gates_overlay_poke_the_same_as_resize() {
        // #380 follow-up: `overlay-poke` (the sessions sidebar's own
        // transition re-triggering every plugin pane, `sessions.ts`'s
        // `panelResizeObs`) is the SECOND high-frequency source — it must be
        // gated identically to `resize`, or wiring `poke()` into production
        // reintroduces the exact per-frame log storm `resize`'s gate exists
        // to prevent.
        assert!(!should_log_frame("overlay-poke", false, true));
        assert!(should_log_frame("overlay-poke", true, true));
    }

    #[test]
    fn should_log_frame_always_logs_discrete_sources_even_with_no_change() {
        for source in ["overlay-open", "overlay-close", "move-notify", "init"] {
            assert!(
                should_log_frame(source, false, true),
                "{source} should always log even with an unchanged exclude set"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn physical_bounds_at_100_percent_scale_is_a_plain_round_trip() {
        let r = OcclusionRect { x: 10.0, y: 20.0, width: 30.0, height: 40.0 };
        assert_eq!(physical_bounds(r, 1.0), (10, 20, 40, 60));
    }

    #[test]
    #[cfg(windows)]
    fn physical_bounds_scales_and_rounds_at_fractional_dpi() {
        // 125% scaling (120 DPI / 96) — the common "Recommended" Windows
        // scale on many laptop panels, not just the round 100%/150%/200%
        // steps a naive implementation might only be tested against.
        let r = OcclusionRect { x: 10.0, y: 10.0, width: 33.0, height: 7.0 };
        let scale = 120.0 / 96.0; // 1.25
        // x: 12.5 -> 13 (round-half-away-from-zero), width edge: 43*1.25=53.75->54
        assert_eq!(physical_bounds(r, scale), (13, 13, 54, 21));
    }

    #[test]
    #[cfg(windows)]
    fn physical_bounds_handles_zero_size_rect() {
        let r = OcclusionRect { x: 5.0, y: 5.0, width: 0.0, height: 0.0 };
        assert_eq!(physical_bounds(r, 2.0), (10, 10, 10, 10));
    }
}
