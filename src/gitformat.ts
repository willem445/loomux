// Pure, DOM-free formatting helpers for the git view's commit rows. Kept
// separate from gitview.ts so they can be unit-tested in isolation (see
// test/gitformat.test.ts) — nothing here touches the DOM or git.

/** Abbreviated commit id (git's default 7 chars). Shorter hashes (or an empty
 *  string) pass through untouched, so a malformed/absent hash never throws. */
export function shortRev(hash: string): string {
  return hash.slice(0, 7);
}

/** Date + 24h time for a commit row, e.g. "11/14/2023 22:13" — the locale's
 *  own date order plus HH:mm, so the row shows the time the reviewer asked for
 *  alongside the date. `locale`/`timeZone` are injectable so the output is
 *  deterministic under test; in the app both are omitted and the user's locale
 *  and zone apply. */
export function fmtWhen(unixSec: number, locale?: string, timeZone?: string): string {
  const d = new Date(unixSec * 1000);
  const date = d.toLocaleDateString(locale, timeZone ? { timeZone } : undefined);
  const time = d.toLocaleTimeString(locale, {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
    ...(timeZone ? { timeZone } : {}),
  });
  return `${date} ${time}`;
}

/** Full, unabbreviated timestamp for a tooltip — the locale's date + time. */
export function fmtWhenFull(unixSec: number, locale?: string, timeZone?: string): string {
  const opts = timeZone ? { timeZone } : undefined;
  return new Date(unixSec * 1000).toLocaleString(locale, opts);
}

/** One-line "who + when" for a commit tooltip. When the committer differs from
 *  the author (rebases, cherry-picks, applied patches) both are shown so the
 *  row's committer column isn't mistaken for the author. */
export function authorLine(author: string, committer: string, unixSec: number): string {
  const who = committer && committer !== author ? `${author} (committed by ${committer})` : author;
  return `${who} · ${fmtWhenFull(unixSec)}`;
}
