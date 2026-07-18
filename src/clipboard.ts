// OSC 52 clipboard bridge.
//
// A CLI copies to the system clipboard by emitting
//   ESC ] 52 ; <Pc> ; <Pd> BEL
// where Pc selects the clipboard ("c" = clipboard, "p" = primary, "s" =
// selection, "0".."7" = numbered cut-buffers, or several concatenated) and Pd
// is the base64 of the UTF-8 text. @xterm/xterm does NOT implement OSC 52 — it
// only exposes `parser.registerOscHandler(52, …)` — so without wiring the
// sequence is silently dropped: the CLI prints "copied!" but the system
// clipboard never changes (issue #65). We register a handler that parses the
// payload and writes it to the clipboard.

/** Max base64 length we'll accept in an OSC 52 payload before decoding — 1 MiB
 *  of base64 (~768 KiB of text). xterm's own OSC length limit only partially
 *  backstops this; the explicit cap keeps a hostile or buggy CLI from making us
 *  `atob` + `TextDecoder` an unbounded string on the main thread. */
export const OSC52_MAX_B64_LEN = 1024 * 1024;

/** Outcome of parsing an OSC 52 payload. `ignore` means "not a write we act on"
 *  (read request / empty / malformed base64) and is silent; `oversize` means a
 *  well-formed but too-large payload we refuse and want to surface. */
export type Osc52Parse =
  | { ok: true; text: string }
  | { ok: false; reason: "ignore" | "oversize" };

/** Decode an OSC 52 payload (`<Pc>;<Pd>`) into the text to copy.
 *
 *  Rejected as `ignore` (silent):
 *   - a read request (`<Pc>;?`) — servicing it would leak the clipboard to any
 *     process that asks and require writing a reply back into the PTY;
 *   - an empty or malformed payload / undecodable base64.
 *  Rejected as `oversize`: a valid payload beyond {@link OSC52_MAX_B64_LEN}.
 *
 *  Exported (and DOM-free) so the parsing is unit-testable in Node. */
export function parseOsc52(payload: string): Osc52Parse {
  const sep = payload.indexOf(";");
  if (sep < 0) return { ok: false, reason: "ignore" }; // no Pc;Pd split
  const data = payload.slice(sep + 1);
  if (data === "" || data === "?") return { ok: false, reason: "ignore" }; // empty / read
  // Cap BEFORE atob so an oversized payload can't balloon memory on decode.
  if (data.length > OSC52_MAX_B64_LEN) return { ok: false, reason: "oversize" };
  let bin: string;
  try {
    bin = atob(data);
  } catch {
    return { ok: false, reason: "ignore" }; // not valid base64
  }
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  // OSC 52 payloads are UTF-8; decode as such so non-ASCII copies survive.
  return { ok: true, text: new TextDecoder().decode(bytes) };
}

/** Write `text` to the system clipboard, with a legacy fallback for webviews
 *  that reject the async Clipboard API. Mirrors gitview's copyText so behavior
 *  is consistent across the app. Never throws; returns whether the write
 *  actually succeeded so the caller can signal a total failure (otherwise a
 *  locked-down webview silently reintroduces the "said copied, clipboard empty"
 *  symptom — #65). */
export async function writeClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    /* fall through to the execCommand path */
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false; // nothing more we can do
  } finally {
    ta.remove();
  }
  return ok;
}

/** Read text off the system clipboard, with the same legacy fallback
 *  `writeClipboard` uses for a webview that rejects the async Clipboard API.
 *  Never throws. `ok: false` means the read genuinely failed (both the async
 *  API and the execCommand fallback came up empty) — the caller's job is to
 *  SURFACE that, not swallow it (#370: the terminal paste handler used to do
 *  exactly `.catch(() => {})`, so a blocked/denied read looked identical to a
 *  keypress that did nothing). An empty-but-successful read (`ok: true, text:
 *  ""`) is not a failure — the clipboard legitimately has nothing in it, and a
 *  paste of nothing is a normal no-op, not an error to report. */
export async function readClipboard(): Promise<{ ok: true; text: string } | { ok: false }> {
  try {
    return { ok: true, text: await navigator.clipboard.readText() };
  } catch {
    /* fall through to the execCommand path */
  }
  const ta = document.createElement("textarea");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.focus();
  let ok = false;
  try {
    ok = document.execCommand("paste");
  } catch {
    ok = false; // nothing more we can do
  }
  const text = ta.value;
  ta.remove();
  return ok ? { ok: true, text } : { ok: false };
}
