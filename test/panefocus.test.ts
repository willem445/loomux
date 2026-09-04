// Unit tests for the new-pane focus decision (issue #117). The bug: a pane
// spawned programmatically by the orchestrator (MCP spawn_agent →
// orch-spawn-request → openPane) grabbed keyboard focus, pulling the cursor away
// from the pane the human was typing in. Focus must move to a new pane only when
// the human opened it directly — with the one exception that an empty grid still
// focuses so the app is never left without an active terminal. This pins that
// rule; grid.ts's DOM wiring is validated by hand. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  shouldFocusNewPane,
  shouldRestoreFocus,
  shouldPreserveMaximize,
  revealPlan,
  liveSessionAction,
  type RevealStep,
} from "../src/panefocus.ts";

test("a human-initiated pane on a populated grid takes focus", () => {
  // Split button, launcher fleet, session restore, launching an orchestrator.
  assert.equal(shouldFocusNewPane(true, false), true);
});

test("an orchestrator-driven spawn on a populated grid does NOT take focus", () => {
  // The regression this fixes: the human is typing in another pane and the
  // cursor must stay put when the orchestrator spawns a worker.
  assert.equal(shouldFocusNewPane(false, false), false);
});

test("an orchestrator-driven spawn onto an empty grid still takes focus", () => {
  // No existing pane to leave focus on — focusing anyway is correct, or the app
  // would have no active terminal. (Not a real path today since the orchestrator
  // pane opens first, but the rule must be safe if it ever is.)
  assert.equal(shouldFocusNewPane(false, true), true);
});

test("a human-initiated pane onto an empty grid takes focus", () => {
  // The very first pane at startup.
  assert.equal(shouldFocusNewPane(true, true), true);
});

// --- focus-restore decision (issue #117 round 2) ---
// The live test failed: spawning an agent while the human typed in the steering
// box pulled focus away and their text went nowhere. Cause: inserting a pane
// restructures the grid DOM (renderSplit → replaceChildren), which detaches the
// focused subtree and blurs it to <body>. The caller snapshots focus before the
// relayout and restores it after — but only in the right cases. This pins them.

test("a background spawn restores focus to the human's input", () => {
  // The regression: not taking focus for the new pane, something held focus, and
  // it survived the relayout — hand it straight back so typing continues.
  assert.equal(shouldRestoreFocus(false, true, true), true);
});

test("a focus-taking (human) open does NOT restore prior focus", () => {
  // The new pane is meant to take focus — restoring the old element would fight
  // the intended move even though something was focused and is still connected.
  assert.equal(shouldRestoreFocus(true, true, true), false);
});

test("nothing to restore when no element held focus", () => {
  // Focus was already on <body>/nothing before the open — no caret to preserve.
  assert.equal(shouldRestoreFocus(false, false, true), false);
});

test("don't restore focus to an element the relayout removed", () => {
  // The prior element left the document mid-open (e.g. its pane closed) — there's
  // no live node to focus; guarding avoids a focus() on a detached element.
  assert.equal(shouldRestoreFocus(false, true, false), false);
});

// --- preserve-maximize decision (issue #155) ---
// The bug: with a pane maximized, an orchestrator-driven spawn collapsed the
// fullscreen view because openPane exits maximize before growing the split tree.
// A background spawn must keep the human's fullscreen; a human open still exits
// it (they asked for a pane and want to see the layout). This pins that rule;
// grid.ts's lift/re-lift DOM wiring is validated by hand.

test("a background spawn while maximized preserves fullscreen", () => {
  // The regression: the human is watching one pane full-screen and an agent
  // spawns — the view must stay maximized (new pane grows the tree underneath).
  assert.equal(shouldPreserveMaximize(false, true), true);
});

test("a human-initiated open while maximized exits fullscreen (unchanged)", () => {
  // The human asked for a pane (split/launcher) — show them the layout it
  // landed in, as before.
  assert.equal(shouldPreserveMaximize(true, true), false);
});

test("a background spawn with nothing maximized has no fullscreen to preserve", () => {
  // Normal grid — the #117 focus path applies; nothing to keep maximized.
  assert.equal(shouldPreserveMaximize(false, false), false);
});

test("a human open with nothing maximized has no fullscreen to preserve", () => {
  assert.equal(shouldPreserveMaximize(true, false), false);
});

// --- reveal plan (#2365) ---
// The defect: nothing removed the orchestrator pane — a maximized sibling HID
// it, and every "go to this pane" path (Agents row → deps.focus, orch-focus →
// OrchWiring.focusPty, the Sessions live row) was maximize- and dock-blind, so
// setActive flipped a class nobody could see and focus() ran on a display:none
// textarea. These pin the ORDERED plan the reveal executes; grid.ts's DOM
// wiring is validated by hand, as ever.

const at = (plan: RevealStep[], step: RevealStep): number => plan.indexOf(step);

test("a pane hidden behind a maximized sibling is revealed by exiting fullscreen before it is activated", () => {
  // The reported failure. setActive on a display:none pane is invisible, so
  // the fullscreen must drop FIRST — the order is the whole point, not the
  // membership.
  const plan = revealPlan({ tabIsActive: true, docked: false, maximized: "other", humanInitiated: true });
  assert.ok(plan.includes("exit-maximize"));
  assert.ok(at(plan, "exit-maximize") < at(plan, "set-active"));
  assert.ok(at(plan, "set-active") < at(plan, "focus"));
});

test("revealing the maximized pane itself never exits fullscreen", () => {
  // Negative control for the test above: a human who maximized this pane and
  // then clicked its own Agents row must not be yanked out of fullscreen. The
  // pane is already the only thing on screen — there is nothing to reveal.
  const plan = revealPlan({ tabIsActive: true, docked: false, maximized: "self", humanInitiated: true });
  assert.ok(!plan.includes("exit-maximize"));
  assert.deepEqual(plan, ["set-active", "focus"]);
});

test("a docked pane is restored, and restore stands in for exit-maximize", () => {
  // Grid.restore already exits fullscreen on its way in (grid.ts), so emitting
  // both would be one redundant relayout — and Grid.toggleMaximize refuses a
  // docked pane outright, so the dock step is the only one that can put this
  // pane back on screen at all.
  const plan = revealPlan({ tabIsActive: true, docked: true, maximized: "other", humanInitiated: true });
  assert.ok(plan.includes("restore-from-dock"));
  assert.ok(!plan.includes("exit-maximize"));
  assert.ok(at(plan, "restore-from-dock") < at(plan, "set-active"));
});

test("a visible pane in the active tab gets exactly set-active then focus", () => {
  // Negative control: the plain case is EXACTLY the two steps the app already
  // took, so no structural step can sneak into a reveal that needs none.
  const plan = revealPlan({ tabIsActive: true, docked: false, maximized: null, humanInitiated: true });
  assert.deepEqual(plan, ["set-active", "focus"]);
});

test("the tab switch is always the first step when the pane is in another tab", () => {
  // A pane in a background tab is display:none by its WORKSPACE, so every
  // later step is invisible until the tab is showing.
  // Both askers: `switch-tab` is not a structural step and does not resize a
  // PTY, so an agent-initiated reveal is owed it too — an agent asking for
  // attention on a pane in a background tab would otherwise point at nothing.
  for (const humanInitiated of [true, false]) {
    for (const docked of [false, true]) {
      for (const maximized of [null, "self", "other"] as const) {
        const plan = revealPlan({ tabIsActive: false, docked, maximized, humanInitiated });
        assert.equal(
          plan[0],
          "switch-tab",
          `human=${humanInitiated} docked=${docked} maximized=${maximized}`
        );
      }
    }
  }
});

test("no plan ever contains a removing step", () => {
  // All 2x2x3 crossings, enumerated rather than sampled. A reveal is allowed to
  // be exactly these five steps — nothing that closes, minimizes, or rebuilds
  // the tree — because the reported symptom was a pane the human could not get
  // back, and a reveal that could remove one would be the same bug with a new
  // trigger.
  const allowed: RevealStep[] = [
    "switch-tab",
    "restore-from-dock",
    "exit-maximize",
    "set-active",
    "focus",
  ];
  let crossings = 0;
  for (const humanInitiated of [true, false]) {
    for (const tabIsActive of [true, false]) {
      for (const docked of [true, false]) {
        for (const maximized of [null, "self", "other"] as const) {
          crossings++;
          const plan = revealPlan({ tabIsActive, docked, maximized, humanInitiated });
          const label = `human=${humanInitiated} tab=${tabIsActive} docked=${docked} max=${maximized}`;
          for (const step of plan) assert.ok(allowed.includes(step), `${label}: ${step}`);
          // Every plan ENDS by making the pane the one receiving keystrokes —
          // a reveal that stops short of that is the blindness this fixes.
          assert.deepEqual(plan.slice(-2), ["set-active", "focus"], label);
          // No step is emitted twice: a duplicated exit-maximize or dock restore
          // is a second relayout, which constraint 1 counts as a second PTY fit.
          assert.equal(new Set(plan).size, plan.length, label);
        }
      }
    }
  }
  // The population control: an empty or truncated crossing sweep would pass
  // every assertion above without having asked a single question.
  assert.equal(crossings, 24);
});

// --- what a live-group session row does (#2365) ---

test("a live group whose orchestrator pane is in this window reveals it", () => {
  assert.equal(
    liveSessionAction({ groupLive: true, paneInWindow: true, isOrchestratorRow: true }),
    "reveal"
  );
});

test("a live group with no pane in this window explains instead of calling a resume that refuses", () => {
  // resume_orch_session's pre-check returns "already has a live orchestrator —
  // focus its pane instead" (mod.rs). Calling into a known refusal to render
  // its error as a toast is a round-trip that can only fail.
  assert.equal(
    liveSessionAction({ groupLive: true, paneInWindow: false, isOrchestratorRow: true }),
    "explain"
  );
});

test("a dead group with a leftover pane in this window still resumes", () => {
  // Negative control: `paneInWindow` alone must never short-circuit the
  // resume. A dormant placeholder card from a restored tab set is exactly this
  // state, and it is the resume path's own case.
  assert.equal(
    liveSessionAction({ groupLive: false, paneInWindow: true, isOrchestratorRow: true }),
    "resume"
  );
});

test("a dead group with nothing open resumes", () => {
  assert.equal(
    liveSessionAction({ groupLive: false, paneInWindow: false, isOrchestratorRow: true }),
    "resume"
  );
});

test("a worker row in a live group still rejoins, however the orchestrator pane is placed", () => {
  // The backend refusal this stands in for is role-gated: mod.rs reads
  // `if record.role == "orchestrator" { if record.group_live { … } }`, so a
  // worker/reviewer row in a live group is REJOINED, not refused. Short-
  // circuiting it to a reveal of the orchestrator pane would be a regression
  // dressed as a fix — the human asked for that worker, not for the
  // orchestrator. Both pane placements, so the reveal arm cannot leak in.
  for (const paneInWindow of [true, false]) {
    assert.equal(
      liveSessionAction({ groupLive: true, paneInWindow, isOrchestratorRow: false }),
      "resume",
      `paneInWindow=${paneInWindow}`
    );
  }
});

test("every crossing of the live-session decision is one of the three actions", () => {
  let crossings = 0;
  for (const groupLive of [true, false]) {
    for (const paneInWindow of [true, false]) {
      for (const isOrchestratorRow of [true, false]) {
        crossings++;
        const action = liveSessionAction({ groupLive, paneInWindow, isOrchestratorRow });
        assert.ok(["reveal", "resume", "explain"].includes(action));
        // Only a LIVE orchestrator row may skip the resume — the one rule the
        // backend actually enforces.
        if (action !== "resume") assert.ok(groupLive && isOrchestratorRow);
      }
    }
  }
  assert.equal(crossings, 8);
});

// --- who asked: the constraint-1 gate on the two resizing steps (#2365 review
// round 2, B1) ---
//
// `restore-from-dock` and `exit-maximize` are the only two steps that resize a
// PTY (`Grid.restore` "triggers a single genuine fit" and makes its new siblings
// fit too; `exitMaximize` re-seats the pane that "alone issues one debounced
// fit"). Constraint 1 permits that from a discrete human click and bars it from
// anything else — and `orch-focus`, the one caller of `OrchWiring.focusPty`, is
// emitted only by `Registry::focus_agent`, reached only from the `focus_agent`
// MCP tool, which no human gates. So the agent half of every crossing must
// produce neither step. Same ruling as `shouldPreserveMaximize` (#155) above.

test("an agent-initiated reveal never exits the human's fullscreen", () => {
  // The failure B1 describes: the human maximizes a worker pane to read a long
  // transcript, the orchestrator calls focus_agent mid-run, and the app drops
  // out of fullscreen and repaints the very scrollback they maximized to read.
  const plan = revealPlan({
    tabIsActive: true,
    docked: false,
    maximized: "other",
    humanInitiated: false,
  });
  assert.ok(!plan.includes("exit-maximize"));
  assert.deepEqual(plan, ["set-active", "focus"]);
});

test("an agent-initiated reveal never pulls a pane out of the dock", () => {
  // The worse half: a dock restore re-seats the pane beside the active one, so
  // the siblings that make room for it fit their PTYs as well — several
  // resizes from one unprompted agent call.
  const plan = revealPlan({
    tabIsActive: true,
    docked: true,
    maximized: null,
    humanInitiated: false,
  });
  assert.ok(!plan.includes("restore-from-dock"));
  assert.deepEqual(plan, ["set-active", "focus"]);
});

test("no agent crossing emits a resizing step, and every human one that should still does", () => {
  // The whole table, both halves, with the negative control built in: for each
  // of the twelve states the agent plan must carry neither structural step AND
  // the SAME state asked for by a human must carry exactly the one it is owed.
  // Asserting only the agent half would pass just as well against a `revealPlan`
  // that had stopped emitting structural steps for anybody — which is the
  // regression this pair exists to tell apart.
  const RESIZING: RevealStep[] = ["restore-from-dock", "exit-maximize"];
  let agentPlans = 0;
  let humanStructural = 0;
  for (const tabIsActive of [true, false]) {
    for (const docked of [true, false]) {
      for (const maximized of [null, "self", "other"] as const) {
        const label = `tab=${tabIsActive} docked=${docked} max=${maximized}`;
        const base = { tabIsActive, docked, maximized };

        const agent = revealPlan({ ...base, humanInitiated: false });
        agentPlans++;
        for (const step of RESIZING) {
          assert.ok(!agent.includes(step), `${label}: agent plan contains ${step}`);
        }
        // What the agent DOES get: the tab step (free) plus activate + focus.
        assert.deepEqual(
          agent,
          tabIsActive ? ["set-active", "focus"] : ["switch-tab", "set-active", "focus"],
          label
        );

        // Negative control: the identical state, asked for by a human.
        const human = revealPlan({ ...base, humanInitiated: true });
        const owed = docked ? "restore-from-dock" : maximized === "other" ? "exit-maximize" : null;
        if (owed) {
          humanStructural++;
          assert.ok(human.includes(owed), `${label}: human plan is missing ${owed}`);
        } else {
          assert.deepEqual(human, agent, `${label}: with nothing to undo the two must agree`);
        }
      }
    }
  }
  // Population controls. The first says the sweep ran the whole table; the
  // second says the control half is not vacuous — if NO human crossing were
  // owed a structural step, "the agent gets none" would be true of a function
  // that emits none for anyone, and this test would be measuring nothing.
  assert.equal(agentPlans, 12);
  assert.equal(humanStructural, 8);
});
