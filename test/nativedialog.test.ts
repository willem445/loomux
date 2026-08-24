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
//
// AND THE WIRING IS PINNED TOO, not just the decision. `reclaimFocusOnWindowFocus`
// being correct says nothing about `main.ts` still ROUTING the window-`focus`
// listener through it: the pre-fix line (`activeGrid().activePane?.focus()`) can be
// restored, import and all, with `tsc --noEmit` clean and the whole suite green —
// `noUnusedLocals` catches deleting the call alone, never an honest revert that drops
// the import with it. The last test in this file is the source scan that closes that,
// mirroring the picker half's bypass scan in `test/transport.test.ts`: a suppression
// one line can walk around is no more a suppression than a gate one call site can walk
// around is a gate. DOM wiring stays hand-validated — the scan reads the source, it
// does not simulate an event.
//
// WHY EVERY TEST THAT HOLDS THE GATE RELEASES IT FROM `t.after`. That singleton
// is shared across this file, so a test that fails MID-HOLD would leave the gate
// shut and every later test would fail for that reason instead of its own — one
// genuine failure reported as three, and a mutation round's reds no longer
// attributable to what they were cut for (the repo's `lock_safe` convention, in
// TypeScript). `after` runs whether the body passed or threw.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  nativeDialogOpen,
  reclaimFocusOnWindowFocus,
  withNativeDialog,
} from "../src/nativedialog.ts";

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (v: T) => void;
  reject: (e: unknown) => void;
}

/** A promise plus its settlers, so a test can hold a "dialog" open across awaits
 *  instead of racing a real one. */
function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

test("a second native dialog request is refused while the first is outstanding", async (t) => {
  const first = deferred<string | null>();
  let opened = 0;
  let secondOpened = 0;

  const running = withNativeDialog(() => {
    opened += 1;
    return first.promise;
  });
  t.after(async () => {
    first.resolve(null);
    await running.catch(() => {});
  });

  // Positive control for the refusal below: the gate ADMITTED the first one —
  // "no opener ran at all" would satisfy a `secondOpened === 0` assertion just
  // as well as the refusal does.
  assert.equal(opened, 1, "the first request must actually open a dialog");
  assert.equal(nativeDialogOpen(), true);

  const refused = await withNativeDialog(() => {
    secondOpened += 1;
    return Promise.resolve("D:/second");
  });
  assert.equal(secondOpened, 0, "the second request must not open a second dialog");
  assert.equal(
    refused,
    null,
    "a refused request reads as a cancel — the disposition every pickDirectory call site already handles"
  );

  // The first one still owns the gate and still returns its own answer: the
  // refusal must not have disturbed the dialog that IS up.
  first.resolve("D:/first");
  assert.equal(await running, "D:/first");
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
    return Promise.resolve("D:/after");
  });
  assert.equal(opened, 1);
  assert.equal(after, "D:/after");
});

test("the window-focus reclaim is suppressed while a native dialog is outstanding", async (t) => {
  const held = deferred<string | null>();
  let reclaimed = 0;
  const onWindowFocus = (): void => {
    reclaimFocusOnWindowFocus(() => {
      reclaimed += 1;
    });
  };

  // Positive control FIRST, so "the reclaim never runs at all" cannot pass the
  // absence assertion below.
  onWindowFocus();
  assert.equal(reclaimed, 1, "with no dialog outstanding the reclaim must run");

  const running = withNativeDialog(() => held.promise);
  t.after(async () => {
    held.resolve(null);
    await running.catch(() => {});
  });

  onWindowFocus();
  assert.equal(
    reclaimed,
    1,
    "a focus event arriving while the picker is initializing must NOT push focus back into the webview"
  );

  held.resolve(null);
  await running;
  onWindowFocus();
  assert.equal(reclaimed, 2, "once the dialog is gone the reclaim resumes");
});

test("the gate is shut from the REQUEST, not from the moment a dialog appears", async (t) => {
  // The window #1564 was lost in: the click has been handled, the IPC hop and
  // the thread spawn are still in flight, and the webview is fully interactive.
  // A gate that only closed once the OS dialog existed would leave exactly the
  // interval a human spends clicking Browse a second time unguarded — so the
  // state has to be observable from inside the opener, before anything awaits.
  const held = deferred<string | null>();
  let sawGateDuringOpen: boolean | null = null;
  const running = withNativeDialog(() => {
    sawGateDuringOpen = nativeDialogOpen();
    return held.promise;
  });
  t.after(async () => {
    held.resolve(null);
    await running.catch(() => {});
  });

  assert.equal(sawGateDuringOpen, true);
  held.resolve(null);
  await running;
});

// ---------- the wiring, read off the source ----------

const SRC = fileURLToPath(new URL("../src/", import.meta.url));

/** Every `.ts` under `src/`, at any depth. Recursive for the reason the seam's
 *  own scan documents: a flat read passes today and goes silently blind the day
 *  someone adds a subdirectory, and a module the scan never opens is not a
 *  violation it reports. */
const MODULES = readdirSync(SRC, { recursive: true })
  .map((entry) => String(entry).replace(/\\/g, "/"))
  .filter((f) => f.endsWith(".ts"))
  .sort();

/** The full `addEventListener(...)` call text starting at `openIdx`, by walking
 *  parentheses to the matching close — NOT a fixed-width window, which would
 *  clear itself the moment a handler grew past it. Returns null when the parens
 *  do not balance (or a string literal contains one), which the caller treats as
 *  a violation rather than as a pass: an unreadable registration is exactly the
 *  case a scan must not wave through. */
function callText(src: string, openIdx: number): string | null {
  let depth = 0;
  for (let i = openIdx; i < src.length; i += 1) {
    if (src[i] === "(") depth += 1;
    else if (src[i] === ")") {
      depth -= 1;
      if (depth === 0) return src.slice(openIdx, i + 1);
    }
  }
  return null;
}

test("every window-level focus listener routes through the suppression (#1564)", () => {
  // The symmetric half of `test/transport.test.ts`'s bypass scan, and it exists
  // for the same reason: the app's reaction to a window-focus event is one of the
  // two things this PR removes from the path the #1564 minidump faulted on, and
  // restoring the pre-fix line is a one-line edit that the compiler and the rest
  // of this file are both blind to.
  //
  // Default-deny and keyed on the ENTITY — a window-scope registration of the
  // `focus` event — so renaming a handler or a variable does not step over it.
  // Stated blind spots, in the sibling scan's style: a listener registered through
  // an ALIASED window reference (`const w = window; w.addEventListener(...)`), a
  // computed event name, or a registration whose argument text contains an
  // unbalanced parenthesis inside a string literal are not matched. None appears in
  // `src/` today. `window.onfocus =` IS matched and is always a violation: the scan
  // cannot read that form's body, so adding one means updating this guard rather
  // than silently escaping it.
  const offenders: string[] = [];
  let routed = 0;
  let scanned = 0;

  for (const file of MODULES) {
    const src = readFileSync(SRC + file, "utf8");
    scanned += 1;

    for (const m of src.matchAll(/\bwindow\s*\.\s*onfocus\s*=/g)) {
      offenders.push(`${file}: window.onfocus= at index ${m.index} — this scan cannot read that form`);
    }

    for (const m of src.matchAll(/\bwindow\s*\.\s*addEventListener\s*\(\s*["']focus["']/g)) {
      const open = src.indexOf("(", m.index + "window".length);
      const call = open === -1 ? null : callText(src, open);
      if (call === null) {
        offenders.push(`${file}: a window focus listener this scan could not read to its end`);
      } else if (call.includes("reclaimFocusOnWindowFocus")) {
        routed += 1;
      } else {
        offenders.push(`${file}: ${call.replace(/\s+/g, " ").slice(0, 120)}`);
      }
    }
  }

  // Population controls. Without these the assertion below passes just as well on
  // a scan that opened nothing, or on a tree where the listener has been deleted
  // outright rather than routed — which is the very revert this test is cut for.
  assert.ok(scanned > 20, `the scan opened only ${scanned} modules`);
  assert.ok(MODULES.includes("main.ts"), "main.ts must be in the scanned set");
  assert.equal(
    routed,
    1,
    "exactly one window-focus listener exists and it goes through reclaimFocusOnWindowFocus"
  );

  assert.deepEqual(
    offenders,
    [],
    "a window-focus listener must not grab focus back on its own — while a native " +
      `dialog is opening that is a cross-thread tug-of-war (#1564); these do: ${offenders.join(", ")}`
  );
});
