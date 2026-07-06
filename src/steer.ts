// Steering-strip image attachments (#72). Pasting or attaching a screenshot
// into the orchestrator's compose box can't hand binary to a CLI prompt, but
// Claude Code and Copilot both read image FILES from paths — so each image is
// written to a per-group scratch dir (backend `orch_save_attachment`) and the
// steer text gains an "Attached image: <path>" line pointing at it.
//
// These helpers are pure and DOM-free so the accept/reject rules and the
// text-composition are unit-testable in Node (test/steer.test.ts).

/** Cap on a single pasted/attached image, in bytes. Larger images are refused
 *  with a toast rather than silently written — this bounds both the IPC payload
 *  and the per-group `attachments/` scratch dir. Mirrored by the backend
 *  `MAX_ATTACHMENT_BYTES`, which is the real backstop. */
export const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024; // 10 MiB

/** Most images queued in one steering message. Keeps a fat-fingered multi-paste
 *  from flooding the strip and the prompt with references. */
export const MAX_ATTACHMENTS = 8;

/** Image MIME type → the extension we persist under. Restricting to a known
 *  raster/image set keeps a hostile clipboard from steering the saved filename's
 *  extension and matches what the agent CLIs will actually open. */
const MIME_EXT: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/bmp": "bmp",
};

/** The image extension for a MIME type, or null if it isn't an image type we
 *  accept. */
export function extForMime(mime: string): string | null {
  return MIME_EXT[mime] ?? null;
}

/** Why an attachment was refused, so the UI can phrase a specific toast. */
export type AttachReject = "type" | "size" | "count";

/** Result of vetting a candidate attachment before we hit the backend. */
export type AttachCheck =
  | { ok: true; ext: string }
  | { ok: false; reason: AttachReject };

/** Decide whether a pasted/attached blob may join the queue. `bytes` is its
 *  size, `current` the number already queued. Order matters: an unsupported
 *  type is reported as such even when it's also oversize/over-count. */
export function checkAttachment(mime: string, bytes: number, current: number): AttachCheck {
  const ext = extForMime(mime);
  if (!ext) return { ok: false, reason: "type" };
  if (current >= MAX_ATTACHMENTS) return { ok: false, reason: "count" };
  if (bytes <= 0 || bytes > MAX_ATTACHMENT_BYTES) return { ok: false, reason: "size" };
  return { ok: true, ext };
}

/** Human-readable toast for a rejected attachment. */
export function attachRejectMessage(reason: AttachReject, name?: string): string {
  const who = name ? `"${name}"` : "image";
  switch (reason) {
    case "type":
      return `Can't attach ${who}: only PNG, JPEG, GIF, WebP, and BMP images are supported.`;
    case "size":
      return `Can't attach ${who}: over the ${Math.round(MAX_ATTACHMENT_BYTES / (1024 * 1024))} MB limit.`;
    case "count":
      return `Can't attach more than ${MAX_ATTACHMENTS} images per message.`;
  }
}

/** Build the steering text delivered to the orchestrator: the human's typed
 *  draft, then one "Attached image: <path>" line per queued image. The line
 *  form is what the agent reads to open the file. Returns "" when there's
 *  nothing to send (no text and no attachments), which the caller treats as a
 *  no-op — so a stray Enter on an empty strip never fires a send. */
export function composeSteerText(text: string, paths: string[]): string {
  const lines: string[] = [];
  const t = text.trim();
  if (t) lines.push(t);
  for (const p of paths) lines.push(`Attached image: ${p}`);
  return lines.join("\n");
}

/** base64-encode raw bytes for the `orch_save_attachment` IPC payload. Chunked
 *  so a multi-MB screenshot doesn't blow the argument limit of
 *  `String.fromCharCode(...spread)`. Pure (no DOM) beyond the ambient `btoa`. */
export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000; // 32 KiB per fromCharCode call
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}
