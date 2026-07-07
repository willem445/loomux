// Unit tests for the issues-view core (src/issuesmodel.ts). Pure helpers only;
// the DOM wiring in issuesview.ts is exercised by hand. Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  AGENT_READY,
  AGENT_INVESTIGATE,
  AGENT_MANAGED,
  isLabeledForAgents,
  matchesQuery,
  filterAndSortIssues,
  labelDelta,
  validateNewIssue,
} from "../src/issuesmodel.ts";
import type { GhIssue } from "../src/issues.ts";

function issue(over: Partial<GhIssue> = {}): GhIssue {
  return {
    number: 1,
    title: "Something is broken",
    labels: [],
    state: "OPEN",
    updated_at: "2026-07-01T00:00:00Z",
    url: "https://github.com/o/r/issues/1",
    ...over,
  };
}

test("label constants match the backend allow-list exactly", () => {
  // gh_issue_set_labels rejects anything outside { agent-ready,
  // agent-investigation, agent-managed } — note "agent-investigation", not the
  // plan text's "agent-investigate". Pin the literals so a rename can't silently
  // start sending a label the backend refuses.
  assert.equal(AGENT_READY, "agent-ready");
  assert.equal(AGENT_INVESTIGATE, "agent-investigation");
  assert.equal(AGENT_MANAGED, "agent-managed");
});

test("isLabeledForAgents is true only for go-signal labels", () => {
  assert.equal(isLabeledForAgents(issue({ labels: [AGENT_READY] })), true);
  assert.equal(isLabeledForAgents(issue({ labels: [AGENT_INVESTIGATE] })), true);
  // agent-managed alone is an orchestrator-owned marker, not a go-signal.
  assert.equal(isLabeledForAgents(issue({ labels: [AGENT_MANAGED] })), false);
  assert.equal(isLabeledForAgents(issue({ labels: ["bug", "docs"] })), false);
  assert.equal(isLabeledForAgents(issue({ labels: [] })), false);
});

test("matchesQuery matches number (with or without #), title, and labels", () => {
  const i = issue({ number: 82, title: "Add GitHub issues view", labels: ["enhancement"] });
  assert.equal(matchesQuery(i, ""), true, "empty query matches all");
  assert.equal(matchesQuery(i, "   "), true, "whitespace query matches all");
  assert.equal(matchesQuery(i, "82"), true, "bare number");
  assert.equal(matchesQuery(i, "#82"), true, "number with leading #");
  assert.equal(matchesQuery(i, "GITHUB"), true, "title is case-insensitive");
  assert.equal(matchesQuery(i, "enhance"), true, "label substring");
  assert.equal(matchesQuery(i, "nonsense"), false);
  assert.equal(matchesQuery(i, "83"), false, "different number");
});

test("filterAndSortIssues filters by query and sorts newest-updated first", () => {
  const issues = [
    issue({ number: 1, title: "old", updated_at: "2026-01-01T00:00:00Z" }),
    issue({ number: 2, title: "new", updated_at: "2026-07-05T00:00:00Z" }),
    issue({ number: 3, title: "mid", updated_at: "2026-04-01T00:00:00Z" }),
  ];
  const sorted = filterAndSortIssues(issues, "");
  assert.deepEqual(sorted.map((i) => i.number), [2, 3, 1]);
  // Filtering narrows the set.
  const onlyNew = filterAndSortIssues(issues, "new");
  assert.deepEqual(onlyNew.map((i) => i.number), [2]);
});

test("filterAndSortIssues does not mutate its input", () => {
  const issues = [
    issue({ number: 1, updated_at: "2026-01-01T00:00:00Z" }),
    issue({ number: 2, updated_at: "2026-07-05T00:00:00Z" }),
  ];
  const before = issues.map((i) => i.number);
  filterAndSortIssues(issues, "");
  assert.deepEqual(issues.map((i) => i.number), before);
});

test("filterAndSortIssues breaks timestamp ties by descending number", () => {
  const ts = "2026-07-05T00:00:00Z";
  const issues = [
    issue({ number: 5, updated_at: ts }),
    issue({ number: 9, updated_at: ts }),
    issue({ number: 7, updated_at: ts }),
  ];
  assert.deepEqual(filterAndSortIssues(issues, "").map((i) => i.number), [9, 7, 5]);
});

test("labelDelta adds a missing label", () => {
  assert.deepEqual(labelDelta([], AGENT_READY, true), { add: [AGENT_READY], remove: [] });
});

test("labelDelta removes a present label", () => {
  assert.deepEqual(labelDelta([AGENT_READY], AGENT_READY, false), {
    add: [],
    remove: [AGENT_READY],
  });
});

test("labelDelta is a no-op when the label is already in the desired state", () => {
  // Desired present, already present.
  assert.deepEqual(labelDelta([AGENT_READY], AGENT_READY, true), { add: [], remove: [] });
  // Desired absent, already absent.
  assert.deepEqual(labelDelta([], AGENT_READY, false), { add: [], remove: [] });
});

test("labelDelta leaves other labels (agent-managed) untouched", () => {
  // Toggling agent-ready ON an issue that already carries agent-managed must
  // only add agent-ready — never disturb agent-managed.
  const delta = labelDelta([AGENT_MANAGED], AGENT_READY, true);
  assert.deepEqual(delta, { add: [AGENT_READY], remove: [] });
  // And toggling it back OFF only removes agent-ready.
  const off = labelDelta([AGENT_MANAGED, AGENT_READY], AGENT_READY, false);
  assert.deepEqual(off, { add: [], remove: [AGENT_READY] });
});

test("validateNewIssue rejects an empty or whitespace-only title", () => {
  assert.deepEqual(validateNewIssue({ title: "", body: "x" }), {
    ok: false,
    error: "A title is required.",
  });
  assert.deepEqual(validateNewIssue({ title: "   ", body: "x" }), {
    ok: false,
    error: "A title is required.",
  });
});

test("validateNewIssue trims a valid title and body", () => {
  assert.deepEqual(validateNewIssue({ title: "  Fix it  ", body: "  details  " }), {
    ok: true,
    title: "Fix it",
    body: "details",
  });
});

test("validateNewIssue allows an empty body", () => {
  assert.deepEqual(validateNewIssue({ title: "Just a title", body: "" }), {
    ok: true,
    title: "Just a title",
    body: "",
  });
});
