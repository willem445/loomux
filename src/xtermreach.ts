// Bounded reaches into xterm's private internals — extracted from pane.ts so
// each one is a DOM-free pure function `test/*.test.ts` can pin directly,
// per this repo's testing convention (CLAUDE.md: "Frontend logic that needs
// tests is extracted into DOM-free pure modules"). `Terminal` is imported
// type-only: this file never loads `@xterm/xterm` at runtime, so importing it
// from a `node:test` file costs nothing and pulls in no DOM/addon baggage.
import type { Terminal } from "@xterm/xterm";

/** xterm's own "a human just typed" parse-latency hint, reached behind two
 *  private fields — the only path that makes `WriteBuffer.write` parse a chunk
 *  SYNCHRONOUSLY instead of scheduling it behind a `setTimeout`
 *  (src/common/input/WriteBuffer.ts, the `_didUserInput` fast path). Not an
 *  invented hook: it is the exact call xterm wires to `coreService.onUserInput`
 *  for every keystroke (src/common/CoreTerminal.ts, `this.coreService
 *  .onUserInput(() => this._writeBuffer.handleUserInput())`). The public
 *  `Terminal` type omits both `_core` and the WriteBuffer, so the reach is a
 *  bounded cast against the pinned xterm — and it is `handleUserInput`, NOT
 *  `writeSync`, because `WriteBuffer.writeSync` is marked `@deprecated
 *  Unreliable, to be removed soon` and can drop a chunk mid-sync-loop. Both
 *  fields are reached with optional chaining: the dependency is a caret range
 *  (the lockfile pins today), and if either field ever changes shape this must
 *  degrade to a silent no-op, not throw — a throw here lands inside
 *  `flushOutput` after `pendingOut` has already been drained into `chunks`,
 *  which would lose those bytes and throw on every later flush.
 *
 *  `test/xterm-syncparse.test.ts` pins both halves of that contract against
 *  `@xterm/headless` (the pinned dependency, not a mock): that the reach still
 *  resolves and arms the fast path today, and that a shape it does NOT
 *  recognise degrades to a no-op instead of throwing — so a future
 *  `@xterm/xterm` bump that renames `_core`/`_writeBuffer` fails the FIRST
 *  test loudly, in CI, rather than reinstating the #813 lock stall silently
 *  in prod. #518/#179: this arms `_didUserInput` while hidden, same as a
 *  keystroke would; that's safe here because `wakeOutput()` always flushes
 *  BEFORE `humanOrigin.mark()` runs, and a hidden document is one nobody is
 *  typing into, so the sync parse this triggers can never land inside a
 *  marked turn. */
export function hintXtermSyncParse(term: Terminal): void {
  const wb = (term as unknown as { _core?: { _writeBuffer?: { handleUserInput?(): void } } })
    ._core?._writeBuffer;
  wb?.handleUserInput?.();
}
