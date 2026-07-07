// Unit tests for the steering-strip attachment logic (#72). Pure helpers only;
// the DOM wiring in pane.ts is exercised by hand. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  checkAttachment,
  composeSteerText,
  attachmentLine,
  extForMime,
  attachRejectMessage,
  bytesToBase64,
  steerKeyAction,
  steerBoxHeight,
  MAX_ATTACHMENT_BYTES,
  MAX_ATTACHMENTS,
} from "../src/steer.ts";

/** Minimal keydown snapshot for steerKeyAction (defaults = a plain keypress). */
function key(over: Partial<Parameters<typeof steerKeyAction>[0]> = {}) {
  return { key: "a", shiftKey: false, isComposing: false, keyCode: 65, ...over };
}

test("extForMime maps accepted image types and rejects the rest", () => {
  assert.equal(extForMime("image/png"), "png");
  assert.equal(extForMime("image/jpeg"), "jpg");
  assert.equal(extForMime("image/gif"), "gif");
  assert.equal(extForMime("image/webp"), "webp");
  assert.equal(extForMime("image/bmp"), "bmp");
  // Not an image we persist.
  assert.equal(extForMime("image/svg+xml"), null);
  assert.equal(extForMime("text/plain"), null);
  assert.equal(extForMime("application/pdf"), null);
});

test("checkAttachment accepts a normal screenshot", () => {
  assert.deepEqual(checkAttachment("image/png", 1024 * 1024, 0), { ok: true, ext: "png" });
});

test("checkAttachment rejects a non-image type", () => {
  assert.deepEqual(checkAttachment("text/plain", 10, 0), { ok: false, reason: "type" });
});

test("checkAttachment rejects an oversize image before it hits the backend", () => {
  assert.deepEqual(
    checkAttachment("image/png", MAX_ATTACHMENT_BYTES + 1, 0),
    { ok: false, reason: "size" },
  );
  // Exactly at the cap is still allowed.
  assert.equal(checkAttachment("image/png", MAX_ATTACHMENT_BYTES, 0).ok, true);
  // A zero-byte blob is refused as a size problem, not silently queued.
  assert.deepEqual(checkAttachment("image/png", 0, 0), { ok: false, reason: "size" });
});

test("checkAttachment caps the number of images per message", () => {
  assert.deepEqual(
    checkAttachment("image/png", 10, MAX_ATTACHMENTS),
    { ok: false, reason: "count" },
  );
  // One below the cap still fits.
  assert.equal(checkAttachment("image/png", 10, MAX_ATTACHMENTS - 1).ok, true);
});

test("type wins over size/count when several rules would fail", () => {
  assert.deepEqual(
    checkAttachment("text/plain", MAX_ATTACHMENT_BYTES + 1, MAX_ATTACHMENTS),
    { ok: false, reason: "type" },
  );
});

test("attachmentLine uses a plain path for Claude and an @mention for Copilot", () => {
  assert.equal(attachmentLine("/a/1.png", "claude"), "Attached image: /a/1.png");
  assert.equal(attachmentLine("/a/1.png", "copilot"), "Attached image: @/a/1.png");
  // An unknown/empty CLI falls back to the plain-path form.
  assert.equal(attachmentLine("/a/1.png", ""), "Attached image: /a/1.png");
});

test("composeSteerText appends one path line per image (Claude form)", () => {
  assert.equal(
    composeSteerText("look at this", ["C:/g/attachments/1-0.png"], "claude"),
    "look at this\nAttached image: C:/g/attachments/1-0.png",
  );
  assert.equal(
    composeSteerText("two", ["/a/1.png", "/a/2.jpg"], "claude"),
    "two\nAttached image: /a/1.png\nAttached image: /a/2.jpg",
  );
});

test("composeSteerText emits @mentions for a Copilot orchestrator", () => {
  assert.equal(
    composeSteerText("check this", ["/a/1.png", "/a/2.jpg"], "copilot"),
    "check this\nAttached image: @/a/1.png\nAttached image: @/a/2.jpg",
  );
});

test("composeSteerText allows an images-only message (no typed text)", () => {
  assert.equal(
    composeSteerText("   ", ["/a/1.png"], "claude"),
    "Attached image: /a/1.png",
  );
});

test("composeSteerText is empty when there's nothing to send", () => {
  assert.equal(composeSteerText("", [], "claude"), "");
  assert.equal(composeSteerText("   \n  ", [], "copilot"), "");
});

test("composeSteerText trims the draft but preserves interior text", () => {
  assert.equal(composeSteerText("  hi there  ", [], "claude"), "hi there");
});

test("attachRejectMessage phrases each refusal", () => {
  assert.match(attachRejectMessage("type", "a.svg"), /only PNG/i);
  assert.match(attachRejectMessage("size", "big.png"), /limit/i);
  assert.match(attachRejectMessage("count"), /more than/i);
});

test("bytesToBase64 round-trips through atob", () => {
  const bytes = new Uint8Array([0, 1, 2, 254, 255, 65, 66, 67]);
  const b64 = bytesToBase64(bytes);
  const back = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
  assert.deepEqual(back, bytes);
});

test("bytesToBase64 handles a payload larger than the chunk size", () => {
  // 100 KiB crosses the 32 KiB fromCharCode chunk boundary several times.
  const bytes = new Uint8Array(100 * 1024);
  for (let i = 0; i < bytes.length; i++) bytes[i] = i & 0xff;
  const back = Uint8Array.from(atob(bytesToBase64(bytes)), (c) => c.charCodeAt(0));
  assert.deepEqual(back, bytes);
});

// --- steer-box key handling & auto-grow (#100) ---------------------------

test("steerKeyAction: plain Enter sends", () => {
  assert.equal(steerKeyAction(key({ key: "Enter" })), "submit");
});

test("steerKeyAction: Shift+Enter inserts a newline instead of sending", () => {
  // This is the core of #100 — Enter must not send when Shift is held, so a
  // multi-line draft is possible.
  assert.equal(steerKeyAction(key({ key: "Enter", shiftKey: true })), "newline");
});

test("steerKeyAction: Enter mid-IME-composition is never a send", () => {
  // isComposing OR the legacy keyCode 229 both mean the key belongs to the IME.
  assert.equal(steerKeyAction(key({ key: "Enter", isComposing: true })), "newline");
  assert.equal(steerKeyAction(key({ key: "Enter", keyCode: 229 })), "newline");
});

test("steerKeyAction: Escape returns to the terminal, but not mid-composition", () => {
  assert.equal(steerKeyAction(key({ key: "Escape" })), "blur");
  // Escape during composition cancels the candidate — leave it to the IME.
  assert.equal(steerKeyAction(key({ key: "Escape", isComposing: true })), "pass");
});

test("steerKeyAction: ordinary typing falls through to the textarea", () => {
  assert.equal(steerKeyAction(key({ key: "a" })), "pass");
  assert.equal(steerKeyAction(key({ key: "Tab" })), "pass");
});

test("steerBoxHeight: grows with content while under the cap", () => {
  // 3 lines' worth, cap of 6 lines → box takes the content height, no scrollbar.
  assert.deepEqual(steerBoxHeight(60, 122), { heightPx: 60, scroll: false });
});

test("steerBoxHeight: caps at the max and switches to internal scroll", () => {
  // 8 lines of content against a 6-line cap → clamp and scroll.
  assert.deepEqual(steerBoxHeight(180, 122), { heightPx: 122, scroll: true });
});

test("steerBoxHeight: exactly at the cap does not scroll", () => {
  assert.deepEqual(steerBoxHeight(122, 122), { heightPx: 122, scroll: false });
});

test("steerBoxHeight: a zero/unknown cap never forces a scrollbar", () => {
  // getComputedStyle could hand us 0 before layout; fall back to the content height.
  assert.deepEqual(steerBoxHeight(40, 0), { heightPx: 40, scroll: false });
});
