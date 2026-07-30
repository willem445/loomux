// #412: classification of a resume failure's structured tag into what the
// session-browser UI should offer. Pure — resumeerror.ts.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  resumeFailureKind,
  offersStartFresh,
  resumeFailureReason,
} from "../src/resumeerror.ts";

test("recognizes every backend tag", () => {
  assert.equal(
    resumeFailureKind("resume-not-found: session abc was not found in the claude session history"),
    "not-found"
  );
  assert.equal(
    resumeFailureKind("resume-workspace-missing: session abc is recorded under X, but X is gone"),
    "workspace-missing"
  );
  assert.equal(resumeFailureKind("resume-ambiguous: ambiguous session prefix"), "ambiguous");
  assert.equal(
    resumeFailureKind("resume-store-unreadable: could not read the claude session store"),
    "store-unreadable"
  );
  assert.equal(
    resumeFailureKind(
      "resume-group-mismatch: session abc belongs to orchestration group g-2, not g-1 — refusing to rejoin it into another group"
    ),
    "group-mismatch",
    "#485: the backend's wrong-group refusal must be classifiable, not fall through as an opaque string"
  );
  assert.equal(
    resumeFailureKind(
      "resume-group-unknown: session abc has no recorded orchestration membership on this machine, so the group it belongs to cannot be verified"
    ),
    "group-unknown",
    "#485 review f1: 'cannot be verified' is its own kind — distinct from being contradicted"
  );
});

test("an untagged or unrelated error is null, not misclassified", () => {
  assert.equal(resumeFailureKind("group already has a live orchestrator"), null);
  assert.equal(resumeFailureKind(""), null);
  assert.equal(resumeFailureKind("resume-something-nobody-defined-yet: whatever"), null);
});

test("leading/trailing whitespace on the raw error doesn't defeat the tag match", () => {
  assert.equal(resumeFailureKind("  resume-not-found: x  "), "not-found");
});

test("only the two provably-unresolvable kinds offer a start-fresh affordance", () => {
  assert.equal(offersStartFresh("not-found"), true);
  assert.equal(offersStartFresh("workspace-missing"), true);
  assert.equal(offersStartFresh("ambiguous"), false, "ambiguous needs a longer id, not a fresh spawn");
  assert.equal(offersStartFresh("store-unreadable"), false, "an I/O problem isn't fixed by a fresh session");
  assert.equal(
    offersStartFresh("group-mismatch"),
    false,
    "#485: a fresh session would be spawned into the same wrong group — the refusal exists to prevent exactly that"
  );
  assert.equal(
    offersStartFresh("group-unknown"),
    false,
    "#485 review f1: a fresh session would join the same unverified group — the refusal exists to prevent that"
  );
  assert.equal(offersStartFresh(null), false);
});

test("both group refusals read as prose, not as the wire tag (#485)", () => {
  // The rejoin-loop toast shows resumeFailureReason for these two kinds instead
  // of the raw `resume-<tag>: …` backend string, so a human never reads the
  // wire format. Each says what to do, and they don't say the same thing —
  // "belongs to another group" and "we don't know its group" are different
  // situations with different next steps.
  const mismatch = resumeFailureReason("group-mismatch");
  const unknown = resumeFailureReason("group-unknown");
  for (const text of [mismatch, unknown]) {
    assert.doesNotMatch(text, /resume-[a-z-]+:/, "no wire tag leaks into the human-facing sentence");
    assert.notEqual(text, resumeFailureReason(null), "not the generic fallback phrasing");
  }
  assert.match(mismatch, /different orchestration group/);
  assert.match(unknown, /no record/i);
  assert.notEqual(mismatch, unknown);
});

test("reason text is specific per kind, not a generic placeholder", () => {
  const missing = resumeFailureReason("workspace-missing");
  const notFound = resumeFailureReason("not-found");
  assert.match(missing, /workspace no longer exists/);
  assert.match(notFound, /not.*found.*session history/i);
  assert.notEqual(missing, notFound);
});
