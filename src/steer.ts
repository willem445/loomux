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

/** The in-prompt reference line for one attached image, formatted for how the
 *  orchestrator's CLI consumes an image path:
 *   - Claude Code reads a plain absolute path with its file tools;
 *   - GitHub Copilot CLI documents an `@<path>` mention (["Using GitHub Copilot
 *     CLI"](https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/overview)),
 *     which attaches the image into its context.
 *  The human-readable "Attached image: " label is harmless prose to either
 *  agent; the path (bare, or `@`-prefixed) is what does the work. Unknown CLIs
 *  fall back to the plain-path form. */
export function attachmentLine(path: string, cli: string): string {
  return cli === "copilot"
    ? `Attached image: @${path}`
    : `Attached image: ${path}`;
}

/** Build the steering text delivered to the orchestrator: the human's typed
 *  draft, then one attachment reference line per queued image, formatted for
 *  the group's orchestrator `cli`. Returns "" when there's nothing to send (no
 *  text and no attachments), which the caller treats as a no-op — so a stray
 *  Enter on an empty strip never fires a send. */
export function composeSteerText(text: string, paths: string[], cli: string): string {
  const lines: string[] = [];
  const t = text.trim();
  if (t) lines.push(t);
  for (const p of paths) lines.push(attachmentLine(p, cli));
  return lines.join("\n");
}

// ---------------------------------------------------------------------------
// Steering-box key handling & auto-grow (#100). The compose box used to be a
// single-line <input> that scrolled one endless horizontal line; it's now a
// wrapping <textarea> that grows to a few lines. These helpers keep the two
// decisions that carry intent — what a keystroke means, and how tall the box
// may get — DOM-free so they're unit-testable (test/steer.test.ts).

/** The bits of a keydown the steer box cares about. DOM-free so the
 *  submit-vs-newline decision is testable without synthesizing a KeyboardEvent. */
export interface SteerKey {
  key: string;
  shiftKey: boolean;
  /** True while an IME composition is in flight (`KeyboardEvent.isComposing`). */
  isComposing: boolean;
  /** Legacy IME sentinel: some IMEs report 229 without setting `isComposing`. */
  keyCode: number;
}

/** What a keydown on the steer box should do:
 *   - `submit`  send the draft (and swallow the key),
 *   - `newline` insert a line break — the box wraps and grows,
 *   - `blur`    hand focus back to the terminal (and swallow the key),
 *   - `pass`    let the textarea handle it normally (ordinary typing). */
export type SteerKeyAction = "submit" | "newline" | "blur" | "pass";

/** Map a keydown to its steer-box action. Enter sends; **Shift+Enter** inserts a
 *  newline so multi-line drafts are possible; Escape returns to the terminal.
 *  Enter during an IME composition is the candidate-commit keystroke, not a
 *  send, so it's treated as `newline` (i.e. left to the browser) — we must never
 *  submit a half-composed word. */
export function steerKeyAction(e: SteerKey): SteerKeyAction {
  const composing = e.isComposing || e.keyCode === 229;
  if (e.key === "Enter") {
    return e.shiftKey || composing ? "newline" : "submit";
  }
  if (e.key === "Escape" && !composing) return "blur";
  return "pass";
}

/** The height (px) to apply to the auto-growing steer box, plus whether it now
 *  needs an internal scrollbar. `natural` is the content's measured scrollHeight;
 *  `maxPx` the cap (a few lines). Past the cap the box scrolls internally instead
 *  of getting taller — its footprint must stay put so the terminal below it is
 *  never resized (a PTY resize repaints ConPTY; forbidden for UI chrome). */
export function steerBoxHeight(natural: number, maxPx: number): { heightPx: number; scroll: boolean } {
  const capped = maxPx > 0 && natural > maxPx;
  return { heightPx: capped ? maxPx : natural, scroll: capped };
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
