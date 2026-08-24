// The native-dialog gate (#1564) — `src/nativedialog.ts`.
//
// WHAT THIS CAN AND CANNOT PROVE. The crash it exists to stop is a Windows
// focus race between comdlg32's dialog thread and WebView2's focus machinery,
// exposed on one machine with BeyondTrust's `PGHook.dll` injected. It cannot be
// reproduced in Node, in CI, or by anything headless, and the end-to-end proof
// is the human re-running the Browse gesture on the PC that crashed. What IS
// testable — and what these tests pin — is the app-side rule the fix rests on:
// exactly one native dialog may be outstanding, and while one is, the app does
// not answer a window-focus event by grabbing focus back into the terminal.
//
// The two halves read ONE latch. A test that held them apart (two fakes, two
// flags) could pass while the shipped code let them disagree, which is the
// failure the module's comment argues about — so these drive the real module
// singleton rather than an injected instance.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  nativeDialogOpen,
  reclaimFocusOnWindowFocus,
  withNativeDialog,
} from "../src/nativedialog.ts";

/** A promise plus its resolver, so a test can hold a "dialog" open across
 *  awaits instead of racing a real one. */
function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void; reject: (e: unknown) => void } {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

test("a second native dialog request is refused while the first is outstanding", async () => {
  const first = deferred<string | null>();
  let opened = 0;
  let secondOpened = 0;

  const running = withNativeDialog(() => {
    opened += 1;
    return first.promise;
  });
  // Positive control for the refusal below: the gate ADMITTED the first one —
  // "no opener ran" would satisfy a `secondOpened === 0` assertion just as well.
  assert.equal(opened, 1, "the first request must actually open a dialog");
  assert.equal(nativeDialogOpen(), true);

  const refused = await withNativeDialog(() => {
    secondOpened += 1;
    return Promise.resolve("C:\second");
  });
  assert.equal(secondOpened, 0, "the second request must not open a second dialog");
  assert.equal(
    refused,
    null,
    "a refused request reads as 'the human chose nothing', the disposition every pickDirectory call site already handles"
  );

  // The first one still owns the gate and still returns its own answer: the
  // refusal must not have disturbed the dialog that IS up.
  first.resolve("C:\first");
  assert.equal(await running, "C:\first");
  assert.equal(nativeDialogOpen(), false);
});

test("the gate reopens once the picker settles — and a REJECTED picker reopens it too", async () => {
  // The bounded-suppression rule: a guard held "while X" needs an answer for "X
  // never clears". Here the only holder is a picker still running, and BOTH
  // settle paths release it — a picker that throws must not wedge Browse for
  // the rest of the session.
  const failing = deferred<string | null>();
  const ran = withNativeDialog(() => failing.promise);
  assert.equal(nativeDialogOpen(), true);
  failing.reject(new Error("picker exploded"));
  await assert.rejects(ran, /picker exploded/);
  assert.equal(nativeDialogOpen(), false, "a rejected picker must reopen the gate");

  // And the gate really admits again afterwards — not merely reports itself open.
  let opened = 0;
  const after = await withNativeDialog(() => {
    opened += 1;
    return Promise.resolve("C:\after");
  });
  assert.equal(opened, 1);
  assert.equal(after, "C:\after");
});

test("the window-focus reclaim is suppressed while a native dialog is outstanding", async () => {
  const held = deferred<string | null>();
  let reclaimed = 0;

  // Positive control FIRST, so "the reclaim never runs at all" cannot pass the
  // absence assertion below.
  reclaimFocusOnWindowFocus(() => {
    reclaimed += 1;
  });
  assert.equal(reclaimed, 1, "with no dialog outstanding the reclaim must run");

  const running = withNativeDialog(() => held.promise);
  reclaimFocusOnWindowFocus(() => {
    reclaimed += 1;
  });
  assert.equal(
    reclaimed,
    1,
    "a focus event arriving while the picker is initializing must NOT push focus back into the webview"
  );

  held.resolve(null);
  await running;
  reclaimFocusOnWindowFocus(() => {
    reclaimed += 1;
  });
  assert.equal(reclaimed, 2, "once the dialog is gone the reclaim resumes");
});

test("the gate spans the gap BEFORE the dialog appears, not just the time it is up", async () => {
  // The window #1564 was lost in: the click has been handled, the IPC hop and
  // the thread spawn are still in flight, and the webview is fully interactive.
  // `nativeDialogOpen()` has to be true from the moment the request is made —
  // a gate that only closed once the OS dialog existed would leave exactly the
  // interval a human spends clicking Browse a second time unguarded.
  const held = deferred<string | null>();
  let sawGateDuringOpen: boolean | null = null;
  const running = withNativeDialog(() => {
    // Inside the opener: the request has been admitted, nothing has awaited yet.
    sawGateDuringOpen = nativeDialogOpen();
    return held.promise;
  });
  assert.equal(sawGateDuringOpen, true);
  held.resolve(null);
  await running;
});
