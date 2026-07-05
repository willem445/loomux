// Ordered, back-pressured writer for a pane's PTY input.
//
// xterm emits input (keystrokes AND pastes) through `onData` in strict order,
// but every write crosses the async Tauri IPC boundary. Firing them
// concurrently — `writePty(id, data).catch()` — lets the calls reach the
// backend out of order: each `invoke` runs as its own task and they acquire
// the per-pty writer lock in nondeterministic order. A keystroke can then
// overtake a paste, or (worse) a bracketed-paste terminator `ESC[201~` can
// land before its body, wedging the target app in paste mode so everything
// typed next is swallowed. That is the "paste lags / doesn't land" report in
// issue #65.
//
// This serializes writes: exactly one `invoke` is in flight at a time, so the
// IPC layer receives them in the order xterm produced them and cannot reorder.
// It also
//   (a) buffers input produced before the PTY exists, flushing in order once
//       it is ready (subsumes the old ad-hoc inputQueue), and
//   (b) splits very large pastes into bounded chunks so a single multi-megabyte
//       write can't stall ConPTY's small input pipe.

/** Max bytes-ish per PTY write. ConPTY's input pipe is small; a huge single
 *  write blocks the backend command thread until the child drains it. 16 KiB
 *  keeps ordinary pastes to one chunk while capping worst-case stall. */
export const PTY_WRITE_CHUNK = 16 * 1024;

/** Split `data` into pieces of at most `max` UTF-16 code units, never slicing
 *  a surrogate pair (which would corrupt an emoji / astral char mid-paste).
 *  Concatenating the result reproduces `data` exactly. */
export function chunkForPty(data: string, max: number = PTY_WRITE_CHUNK): string[] {
  if (data.length <= max) return [data];
  const parts: string[] = [];
  let i = 0;
  while (i < data.length) {
    let end = Math.min(i + max, data.length);
    if (end < data.length) {
      // If we'd cut right after a high surrogate, defer it to the next chunk.
      const code = data.charCodeAt(end - 1);
      if (code >= 0xd800 && code <= 0xdbff) end -= 1;
    }
    parts.push(data.slice(i, end));
    i = end;
  }
  return parts;
}

export interface OrderedWriter {
  /** Queue input for delivery. Before `ready`, it is buffered; after, it is
   *  chunked and sent strictly in order (one invoke in flight at a time). */
  write(data: string): void;
  /** Bind the actual sender (available once the PTY id is known) and flush any
   *  buffered input in arrival order. */
  ready(send: (data: string) => Promise<void>): void;
  /** Count of items buffered while not yet ready (for tests/introspection). */
  readonly pendingCount: number;
}

/** Create an ordered writer. `chunk` is exposed for tests. */
export function createOrderedWriter(chunk: number = PTY_WRITE_CHUNK): OrderedWriter {
  let send: ((data: string) => Promise<void>) | null = null;
  // The tail of the delivery chain: each write appends `.then(send)` so the
  // next send only starts after the previous resolves — FIFO, single in-flight.
  let chain: Promise<void> = Promise.resolve();
  const pending: string[] = [];

  const pump = (data: string): void => {
    const s = send!;
    for (const part of chunkForPty(data, chunk)) {
      chain = chain.then(() => s(part)).catch(() => {});
    }
  };

  return {
    write(data: string): void {
      if (send) pump(data);
      else pending.push(data);
    },
    ready(sender: (data: string) => Promise<void>): void {
      send = sender;
      for (const data of pending.splice(0)) pump(data);
    },
    get pendingCount(): number {
      return pending.length;
    },
  };
}
