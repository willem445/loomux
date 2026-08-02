// The bounded WebGL re-acquire policy (#720) — webglretry.ts.
//
// Same split as panethrottle.test.ts: tests reading WEBGL_RETRY_DELAYS_MS pin
// the SHIPPED policy (that loomux retries at all, and that it stops), and go
// red if the ladder is ever emptied — which is exactly the pre-#720 behaviour,
// a permanent one-way fall to the DOM renderer. Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  planWebglRetry,
  WEBGL_RETRY_DELAYS_MS,
  WEBGL_HEALTHY_MS,
} from "../src/webglretry.ts";

// ---------- the shipped policy ----------

test("SHIPPED: the first context loss schedules a re-acquire rather than staying on DOM forever", () => {
  // The #720 defect in one assertion: before this, `delayMs` was effectively
  // always null — one lost context made a pane invisibly expensive for the
  // whole session.
  const p = planWebglRetry({ priorLosses: 0, healthyMs: 1_000 });
  assert.notEqual(p.delayMs, null, "an empty ladder would leave the pane on the DOM renderer permanently");
  assert.equal(p.delayMs, WEBGL_RETRY_DELAYS_MS[0]);
  assert.equal(p.losses, 1);
});

test("SHIPPED: the ladder backs off rather than retrying at a fixed rate", () => {
  // Fixed-rate retry across N panes sharing one capped context pool is the
  // live-lock this ladder exists to damp; each rung must be strictly longer.
  assert.ok(WEBGL_RETRY_DELAYS_MS.length >= 2, "a single-rung ladder cannot back off");
  for (let i = 1; i < WEBGL_RETRY_DELAYS_MS.length; i++) {
    assert.ok(
      WEBGL_RETRY_DELAYS_MS[i] > WEBGL_RETRY_DELAYS_MS[i - 1],
      `rung ${i} (${WEBGL_RETRY_DELAYS_MS[i]}ms) must exceed rung ${i - 1} (${WEBGL_RETRY_DELAYS_MS[i - 1]}ms)`
    );
  }
  assert.ok(
    WEBGL_RETRY_DELAYS_MS[0] >= 1_000,
    "the browser already spent 3s trying its own restore before onContextLoss fired — racing back in sub-second buys nothing"
  );
});

test("SHIPPED: every rung of the ladder is walked, in order", () => {
  for (let i = 0; i < WEBGL_RETRY_DELAYS_MS.length; i++) {
    assert.equal(
      planWebglRetry({ priorLosses: i, healthyMs: 1_000 }).delayMs,
      WEBGL_RETRY_DELAYS_MS[i],
      `loss ${i + 1} should use rung ${i}`
    );
  }
});

// ---------- the bound (the refusal) ----------

test("REFUSAL: an exhausted streak stops retrying and stays on the DOM renderer", () => {
  // The fail-closed half. Retrying forever would turn a capped context pool
  // into a live-lock across panes, so a streak that has used every rung must
  // return null — not the last rung again, not a floor value.
  const n = WEBGL_RETRY_DELAYS_MS.length;
  assert.equal(planWebglRetry({ priorLosses: n, healthyMs: 1_000 }).delayMs, null);
  assert.equal(planWebglRetry({ priorLosses: n + 5, healthyMs: 1_000 }).delayMs, null);
});

test("REFUSAL: an exhausted streak keeps counting, so it cannot wrap back into a retry", () => {
  const n = WEBGL_RETRY_DELAYS_MS.length;
  const p = planWebglRetry({ priorLosses: n, healthyMs: 1_000 });
  assert.equal(p.losses, n + 1);
  assert.equal(planWebglRetry({ priorLosses: p.losses, healthyMs: 1_000 }).delayMs, null);
});

// ---------- the release (independent evidence, not elapsed time) ----------

test("a context that lived a healthy lifetime opens a fresh streak, even from an exhausted one", () => {
  // Without this, three unlucky losses spread over an eight-hour session would
  // strand a pane on the DOM renderer for the rest of it — a bound outliving
  // its own justification. The evidence is the dead context's OWN lifetime.
  const n = WEBGL_RETRY_DELAYS_MS.length;
  const p = planWebglRetry({ priorLosses: n + 3, healthyMs: WEBGL_HEALTHY_MS });
  assert.equal(p.delayMs, WEBGL_RETRY_DELAYS_MS[0], "a healthy lifetime restores the full budget");
  assert.equal(p.losses, 1, "and restarts the streak at one, not at the old count");
});

test("a lifetime just under the healthy threshold does NOT reset the streak", () => {
  // The storm case: repeated losses minutes apart are still a storm, and the
  // reset must not be reachable by simply waiting out each rung.
  const p = planWebglRetry({ priorLosses: 1, healthyMs: WEBGL_HEALTHY_MS - 1 });
  assert.equal(p.delayMs, WEBGL_RETRY_DELAYS_MS[1], "still on the ladder");
  assert.equal(p.losses, 2);
});

test("the healthy threshold is longer than the whole ladder, so a storm can never satisfy it", () => {
  // If a context could be declared healthy while the ladder was still running,
  // the bound would be unreachable by construction and the live-lock would be
  // back.
  const ladder = WEBGL_RETRY_DELAYS_MS.reduce((a, b) => a + b, 0);
  assert.ok(
    WEBGL_HEALTHY_MS > ladder,
    `healthy threshold ${WEBGL_HEALTHY_MS}ms must exceed the full ladder ${ladder}ms`
  );
});

test("a negative prior-loss count is clamped rather than indexing off the ladder", () => {
  assert.equal(planWebglRetry({ priorLosses: -3, healthyMs: 0 }).delayMs, WEBGL_RETRY_DELAYS_MS[0]);
});
