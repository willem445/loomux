// Unit tests for the steering-strip attachment logic (#72). Pure helpers only;
// the DOM wiring in pane.ts is exercised by hand. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  checkAttachment,
  composeSteerText,
  extForMime,
  attachRejectMessage,
  bytesToBase64,
  MAX_ATTACHMENT_BYTES,
  MAX_ATTACHMENTS,
} from "../src/steer.ts";

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

test("composeSteerText appends one path line per image", () => {
  assert.equal(
    composeSteerText("look at this", ["C:/g/attachments/1-0.png"]),
    "look at this\nAttached image: C:/g/attachments/1-0.png",
  );
  assert.equal(
    composeSteerText("two", ["/a/1.png", "/a/2.jpg"]),
    "two\nAttached image: /a/1.png\nAttached image: /a/2.jpg",
  );
});

test("composeSteerText allows an images-only message (no typed text)", () => {
  assert.equal(
    composeSteerText("   ", ["/a/1.png"]),
    "Attached image: /a/1.png",
  );
});

test("composeSteerText is empty when there's nothing to send", () => {
  assert.equal(composeSteerText("", []), "");
  assert.equal(composeSteerText("   \n  ", []), "");
});

test("composeSteerText trims the draft but preserves interior text", () => {
  assert.equal(composeSteerText("  hi there  ", []), "hi there");
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
