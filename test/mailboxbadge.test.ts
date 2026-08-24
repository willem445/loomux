// Unit tests for the manager pane's unread-mail chip presentation (#1161 M5).
//
// What these are FOR, since a label test is the easiest tautology to write: the
// chip exists because the manager pane is the one pane loomux never types into,
// so news reaches it by pull and the human is the clock. Each test below pins a
// property whose loss would make the chip fail that brief — the count survives
// with the mouse nowhere near the pane (#813's lesson), an empty mailbox renders
// NOTHING rather than a zero, and the tooltip states the pull model rather than
// restating the number a human can already see.
//
// Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { dockChipMail, mailboxPanes, mailboxPresentation } from "../src/mailboxbadge.ts";

test("the count is in the label, not hover-only", () => {
  // #813's lesson as an assertion: a detail only a hover reveals is a detail
  // nobody sees, and this chip's whole job is being readable at a glance.
  const p = mailboxPresentation(3);
  assert.ok(p, "3 unread must render a chip");
  assert.match(p.label, /\b3\b/, `the count must be in the label: ${p.label}`);
});

test("an empty mailbox renders no chip at all", () => {
  // The common case by an enormous margin — every group with no manager, and
  // every manager whose human just spoke to it. `null` is what hides the chip;
  // a chip reading "0 unread" would be a permanent fixture on a pane that has
  // nothing to say.
  assert.equal(mailboxPresentation(0), null);
  assert.equal(dockChipMail(0), null);
});

test("the tooltip states the PULL model rather than restating the count", () => {
  // The number alone is misread: every other count in this app describes
  // something being delivered. Here nothing is, by design — so the sentence has
  // to say that nothing is typed into this pane AND what makes the manager
  // read, which is the human speaking to it.
  const p = mailboxPresentation(2);
  assert.ok(p);
  assert.match(
    p.title,
    /never typed into this pane|nothing is ever typed/i,
    `the tooltip must say loomux never types here: ${p.title}`
  );
  assert.match(
    p.title,
    /next time you speak to it/i,
    `the tooltip must say what makes it read: ${p.title}`
  );
});

test("one unread reads as singular", () => {
  // Not pedantry: this chip's tooltip is a sentence a human reads once and
  // takes a decision from, and "1 updates are waiting" reads as a broken app,
  // which is exactly the impression the manager pane must not give.
  const p = mailboxPresentation(1);
  assert.ok(p);
  assert.match(p.title, /\b1 update is waiting\b/, p.title);
  const many = mailboxPresentation(4);
  assert.ok(many);
  assert.match(many.title, /\b4 updates are waiting\b/, many.title);
});

test("a count the wire could never carry hides the chip instead of rendering it", () => {
  // `usize` on the Rust side, so none of these can arrive from the real
  // backend. This is the one place that DECIDES what happens if one ever does,
  // and the answer must not be a chip reading "-1 unread" or "2.5 unread" on
  // the human's own pane.
  assert.equal(mailboxPresentation(-1), null);
  assert.equal(mailboxPresentation(Number.NaN), null);
  assert.equal(dockChipMail(Number.NaN), null);
  const frac = mailboxPresentation(2.7);
  assert.ok(frac);
  assert.match(frac.label, /\b2\b/, `a fractional count floors: ${frac.label}`);
  assert.doesNotMatch(frac.label, /2\.7/, frac.label);
});

test("the dock marker carries the count too, since a minimized pane has no header", () => {
  // Same argument the channel chip's dock mirror makes: minimizing a pane must
  // not look like the thing the chip was reporting went away.
  const m = dockChipMail(5);
  assert.ok(m);
  assert.match(m.marker, /\b5\b/, `the dock marker must carry the count: ${m.marker}`);
});

test("the push reaches the manager pane and no other pane in its group", () => {
  // The event carries a group id and nothing else, and a group holds the
  // orchestrator's pane and every delegate's. Routing on the group alone would
  // badge all of them with the manager's mail — so the role test IS the
  // addressing, not a tidy-up.
  const panes = [
    { orchGroupId: "g1", orchRole: "orchestrator", tag: "orch" },
    { orchGroupId: "g1", orchRole: "worker", tag: "w" },
    { orchGroupId: "g1", orchRole: "manager", tag: "mgr" },
    { orchGroupId: "g2", orchRole: "manager", tag: "other-group-mgr" },
    { orchGroupId: null, orchRole: null, tag: "plain shell" },
  ];
  assert.deepEqual(
    mailboxPanes(panes, "g1").map((p) => p.tag),
    ["mgr"]
  );
});
