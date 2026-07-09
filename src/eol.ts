// Line-ending helpers for the editor (issue #174). DOM-free, node:test-covered.
//
// Why this exists: files on disk (especially on Windows / via git autocrlf) are
// often CRLF, but the code editor works in LF internally — CodeMirror splits the
// document on CR/LF and hands back LF from `getValue()`. Comparing that LF text
// against the raw CRLF bytes we read from disk makes a freshly-opened file look
// "modified" the instant it loads (the demo's false "discard unsaved changes"
// warning). The fix: compare dirtiness in an EOL-normalized space, and re-apply
// the file's original line ending when writing so saving never silently
// rewrites CRLF↔LF.

export type Eol = "\r\n" | "\n";

/** The file's dominant line ending: CRLF if any `\r\n` is present, else LF.
 *  (A file with no newline — or only LF — is treated as LF.) */
export function detectEol(text: string): Eol {
  return text.includes("\r\n") ? "\r\n" : "\n";
}

/** Normalize every CRLF to a lone LF. Only collapses `\r\n`; a stray `\r` not
 *  followed by `\n` is left as content (it isn't an editor line break here). */
export function stripCr(text: string): string {
  return text.replace(/\r\n/g, "\n");
}

/** Re-apply `eol` to LF-normalized `text`. `applyEol(stripCr(x), detectEol(x))`
 *  round-trips a document back to its on-disk line ending. */
export function applyEol(text: string, eol: Eol): string {
  const lf = stripCr(text);
  return eol === "\r\n" ? lf.replace(/\n/g, "\r\n") : lf;
}

/** True when two documents differ ignoring line-ending style — the correct
 *  dirty test: opening a CRLF file and touching nothing is NOT a modification,
 *  but changing any actual character is. */
export function textDiffers(a: string, b: string): boolean {
  return stripCr(a) !== stripCr(b);
}
