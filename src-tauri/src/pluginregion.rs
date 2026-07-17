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
//! construction. [`apply_occlusion`]'s non-Windows arm is a documented no-op —
//! the SAME pre-existing bleed as before this PR, not a regression, tracked as
//! a follow-up rather than shipped unverified.

use serde::Deserialize;

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

/// Main-only (mirrors `plugin_open_window`/`plugin_close_window` — see
/// `permissions/sets/misc.toml`; never granted to a plugin's own webview).
/// Clips the plugin child webview's own container `HWND` so it stops
/// painting/swallowing pointer input under every DOM overlay currently
/// covering this pane, letting `main`'s overlay (and whatever's beneath the
/// plugin everywhere else) show through and stay interactive — see this
/// module's doc comment for the full mechanism. `pluginpaneview.ts` calls
/// this every time the set of covering overlays could have changed (open/
/// close, resize, this pane's own reposition). Best-effort if the webview is
/// already gone — a call racing pane teardown must not surface as an error
/// the frontend has to handle specially.
#[tauri::command]
pub fn plugin_set_occlusion(
    app: tauri::AppHandle,
    label: String,
    exclude: Vec<OcclusionRect>,
) -> Result<(), String> {
    validate_label(&label)?;
    let Some(webview) = tauri::Manager::get_webview(&app, &label) else {
        return Ok(());
    };
    webview
        .with_webview(move |platform| apply_occlusion(platform, &exclude))
        .map_err(|e| e.to_string())
}

/// Converts one logical-pixel exclude rect into the physical-pixel `(left,
/// top, right, bottom)` bounds `CreateRectRgn` expects, at the given
/// DPI-derived scale factor. Pure arithmetic, split out from
/// `apply_occlusion` so the rounding behavior at fractional DPI scales
/// (125%/150%, not just 100%/200%) is unit-tested without a real `HWND`.
/// `cfg(windows)`-only (like `apply_occlusion` itself): the physical-pixel
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
fn apply_occlusion(platform: tauri::webview::PlatformWebview, exclude: &[OcclusionRect]) {
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

    let controller = platform.controller();
    // `ParentWindow` returns the container HWND `wry` creates per embedded
    // webview (`webview2::mod.rs`'s `create_container_hwnd`, passed as the
    // controller's own parent at creation — see this module's doc comment) —
    // sized/positioned to exactly this plugin's pane box, so clipping IT
    // clips everything WebView2 renders beneath it.
    let mut hwnd = HWND::default();
    if unsafe { controller.ParentWindow(&mut hwnd as *mut HWND) }.is_err() || hwnd.is_invalid() {
        return;
    }

    if exclude.is_empty() {
        // No overlay currently covers this pane — remove any prior clip so
        // the webview paints (and hit-tests) its full box again. `None` means
        // "no region at all" for SetWindowRgn's `hrgn` param, i.e. the whole
        // window.
        unsafe {
            let _ = SetWindowRgn(hwnd, None, true);
        }
        return;
    }

    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let scale = dpi as f64 / 96.0;

    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
        return;
    }
    let base = unsafe { CreateRectRgn(0, 0, client.right, client.bottom) };
    if base.is_invalid() {
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

    // SetWindowRgn takes ownership of `base` ON SUCCESS (MSDN: "the system
    // owns the region... do not make any further calls with this handle, in
    // particular do not delete it" — it also auto-deletes whatever region was
    // previously set, so there is nothing to release across repeated calls).
    // Only on FAILURE do we still own `base` and must clean it up ourselves,
    // or every failed call leaks one GDI region handle — a real concern here
    // since this runs on every overlay open/close/resize, not a one-shot.
    let applied = unsafe { SetWindowRgn(hwnd, Some(base), true) };
    if applied == 0 {
        unsafe {
            let _ = DeleteObject(base.into());
        }
    }
}

#[cfg(not(windows))]
fn apply_occlusion(_platform: tauri::webview::PlatformWebview, _exclude: &[OcclusionRect]) {
    // Not implemented on macOS/Linux — see this module's doc comment's
    // "Cross-platform" section for why: the same root cause applies there,
    // but fixing it needs separate, unverifiable-from-here native code
    // (CALayer masking / GDK shape regions), tracked as a follow-up rather
    // than shipped unverified. This is a no-op, not a regression: nothing
    // clipped the plugin webview on these platforms before this PR either.
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
