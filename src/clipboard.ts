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

/** Decode an OSC 52 payload (`<Pc>;<Pd>`) into the text to copy, or null when
 *  it isn't a clipboard *write* we should act on.
 *
 *  Returns null for:
 *   - a read request (`<Pc>;?`) — servicing it would leak the clipboard to any
 *     process that asks and require writing a reply back into the PTY;
 *   - an empty or malformed payload / undecodable base64.
 *
 *  Exported (and DOM-free) so the parsing is unit-testable in Node. */
export function parseOsc52(payload: string): string | null {
  const sep = payload.indexOf(";");
  if (sep < 0) return null; // no Pc;Pd split
  const data = payload.slice(sep + 1);
  if (data === "" || data === "?") return null; // empty, or a read request
  let bin: string;
  try {
    bin = atob(data);
  } catch {
    return null; // not valid base64
  }
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  // OSC 52 payloads are UTF-8; decode as such so non-ASCII copies survive.
  return new TextDecoder().decode(bytes);
}

/** Write `text` to the system clipboard, with a legacy fallback for webviews
 *  that reject the async Clipboard API. Mirrors gitview's copyText so behavior
 *  is consistent across the app. Best-effort: never throws. */
export async function writeClipboard(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    /* fall through to the execCommand path */
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try {
    document.execCommand("copy");
  } catch {
    /* nothing more we can do */
  } finally {
    ta.remove();
  }
}
