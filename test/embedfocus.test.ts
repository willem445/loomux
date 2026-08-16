// Unit tests for the embed focus-request hook's pure core (#1091 slice C).
//
// The property under test is the one the hook exists for and the one a
// hand-check would never catch: a request parked for a view that has not been
// CONSTRUCTED yet must still be there when it first renders, and must be gone
// on the render after that. Everything else about the hook is DOM (pane.ts
// opens the view and the view scrolls a row into sight), validated by hand per
// this repo's convention. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { PendingEmbedFocus } from "../src/embedfocus.ts";

test("a request parked before the view exists survives until that view's first render", () => {
  // The whole reason this is a store and not a method call: at `request` time
  // the decisions panel may be an embed nobody has constructed yet.
  const focus = new PendingEmbedFocus();
  focus.request("tasks", "t-7");
  // ... lazy construction happens here, arbitrarily later ...
  assert.equal(focus.take("tasks"), "t-7");
});

test("a request is consumed exactly once, so a later refresh does not re-scroll", () => {
  // The failure this prevents: a board that yanks itself back to t-7 on every
  // `orch-tasks-changed` burst, long after the human scrolled elsewhere.
  const focus = new PendingEmbedFocus();
  focus.request("tasks", "t-7");
  assert.equal(focus.take("tasks"), "t-7");
  assert.equal(focus.take("tasks"), null, "the second render must find nothing parked");
  assert.equal(focus.take("tasks"), null);
});

test("an ordinary render finds nothing parked", () => {
  const focus = new PendingEmbedFocus();
  assert.equal(focus.take("decisions"), null);
});

test("a second request replaces an undrained first — the human wants the LATEST target", () => {
  const focus = new PendingEmbedFocus();
  focus.request("tasks", "t-1");
  focus.request("tasks", "t-2");
  assert.equal(focus.take("tasks"), "t-2");
  assert.equal(focus.take("tasks"), null, "the superseded target is gone, not queued behind it");
});

test("kinds are independent — focusing the board never disturbs the panel's slot", () => {
  const focus = new PendingEmbedFocus();
  focus.request("tasks", "t-3");
  focus.request("decisions", "q-4");
  assert.equal(focus.take("tasks"), "t-3");
  assert.equal(focus.peek("decisions"), "q-4", "draining one kind left the other alone");
  assert.equal(focus.take("decisions"), "q-4");
});

test("a blank target is refused rather than parked", () => {
  // Otherwise it consumes the slot and then focuses nothing — a caller with no
  // id can pass what it has without a guard of its own.
  const focus = new PendingEmbedFocus();
  assert.equal(focus.request("tasks", "   "), false);
  assert.equal(focus.request("tasks", ""), false);
  assert.equal(focus.take("tasks"), null);
  assert.equal(focus.request("tasks", " t-5 "), true, "and a padded one is trimmed, not refused");
  assert.equal(focus.take("tasks"), "t-5");
});

test("a blank request does not clobber a live one", () => {
  const focus = new PendingEmbedFocus();
  focus.request("tasks", "t-6");
  focus.request("tasks", "");
  assert.equal(focus.take("tasks"), "t-6");
});

test("peek does not consume — it is for assertions, never for the render path", () => {
  const focus = new PendingEmbedFocus();
  focus.request("decisions", "q-1");
  assert.equal(focus.peek("decisions"), "q-1");
  assert.equal(focus.peek("decisions"), "q-1");
  assert.equal(focus.take("decisions"), "q-1");
  assert.equal(focus.peek("decisions"), null);
});

test("clear drops a request for a view being disposed, so the next instance cannot pick it up", () => {
  const focus = new PendingEmbedFocus();
  focus.request("decisions", "q-2");
  focus.clear("decisions");
  assert.equal(focus.take("decisions"), null);
});
