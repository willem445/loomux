// Unit tests for the manager pane's unread-mail chip presentation (#1161 M5).
//
// What these are FOR, since a label test is the easiest tautology to write: the
// chip exists because the manager pane is the one pane no fleet traffic is
// delivered into (`deliver_prompt` permits only the two kickoffs and D2's
// post-compact re-grounding notice), so news reaches it by pull and the human is
// the clock. Each test below pins a
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
    /no status is ever delivered into this pane/i,
    `the tooltip must say no status is delivered here: ${p.title}`
  );
  // …and it must NOT say it as an absolute. `deliver_prompt` permits three
  // deliveries into a manager pane — the two kickoffs and D2's post-compact
  // re-grounding notice — so a tooltip promising the human that orrerix never
  // types here is a claim the code refutes after their first compact.
  assert.doesNotMatch(
    p.title,
    /never types|nothing is ever typed/i,
    `the tooltip must not overclaim past the D2 carve-out: ${p.title}`
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

// ── the seed latch, scanned as text (#1502 review N6) ──
//
// `src/pane.ts` has no test file of its own — this repo hand-validates DOM
// wiring rather than simulating a DOM — so without this scan every claim about
// the seed latch would rest on `tsc --noEmit` alone.
//
// That residual is stated STRUCTURALLY, and deliberately not as a list of which
// mutation modes a type-checker misses (CLAUDE.md): an enumeration of modes is a
// SAMPLE of an open set — deletion, polarity, placement, operator swap, operand
// substitution, and whatever the next round names — so "the compiler catches A
// and B but not C" generalises from whichever cases somebody happened to try.
// The checkable statement is the structural one: THIS MODULE HAS NO TEST FILE,
// nothing executes it, and so every claim about it rests on what a type-checker
// and a text scan can see between them.
//
// A source scan closes the polarity half, and it has direct precedent here:
// `test/agenticons.test.ts` scans `pane.ts` as text and scopes the scan to ONE
// method's body, for the reason quoted in its own doc — a consumer scan that can
// be satisfied by the wrong line elsewhere in a ten-thousand-line file is a pin
// that reads like coverage and isn't.
//
// This scan's own residual, stated because it cuts against the technique: it decides
// partly on a binding's NAME, which CLAUDE.md warns is a shape a rename steps
// over. It is benign only because a PARTIAL rename fails `tsc` — `mailPushed` is
// read here and written in `setMailUnread`, so renaming one and not the other
// does not compile — and a COMPLETE rename fails this test loudly with the
// message below rather than silently passing.

import { readFileSync } from "node:fs";

/** One method's body from `src/pane.ts`, by name. Scoped to the method rather
 *  than the file, `agenticons.test.ts`'s rule and for its reason. */
function paneMethodBody(signature: string): string {
  const src = readFileSync(new URL("../src/pane.ts", import.meta.url), "utf8");
  const at = src.indexOf(signature);
  assert.ok(
    at >= 0,
    `Pane's ${JSON.stringify(signature)} is gone or no longer matches the expected shape — ` +
      `move this scan with it rather than deleting it`
  );
  const rest = src.slice(at);
  const end = rest.indexOf("\n  }");
  assert.ok(end > 0, `${signature}: could not find the method's closing brace`);
  return rest.slice(0, end);
}

test("the seed defers to a push, and the guard's polarity is the pinned part", () => {
  // The defect this exists for is real and was found by self-review, not
  // hypothesised: `applyMailSeed` is resolved a round trip after it was asked
  // for, and by then the pane is already a push target — so without the latch
  // the seed's older number overwrites a newer one, INCLUDING a push of 0, which
  // is how an emptied mailbox is reported.
  const seed = paneMethodBody("applyMailSeed(unread: number): void {");
  assert.match(
    seed,
    /if \(this\.mailPushed\) return;/,
    `applyMailSeed must bail when a push has already landed, and the polarity is the whole ` +
      `point — \`if (!this.mailPushed) return\` compiles, type-checks, and silently makes the ` +
      `seed the ONLY thing that can ever render the chip: ${seed}`
  );

  // The other half of the latch, which the scan above cannot see: something has
  // to SET the flag, and it has to be the push path rather than the seed path.
  const push = paneMethodBody("setMailUnread(unread: number): void {");
  assert.match(
    push,
    /this\.mailPushed = true;/,
    `setMailUnread is the push path and must latch the flag: ${push}`
  );
  assert.doesNotMatch(
    seed,
    /this\.mailPushed = true;/,
    `applyMailSeed must NOT latch — a seed that marks itself as a push would let the next ` +
      `seed lose to the previous one: ${seed}`
  );

  // POSITIVE CONTROL on the instrument, not just on the subject: this scan is
  // two `indexOf`s over a large file, and a scoping bug that returned an empty
  // string would make every `doesNotMatch` above pass vacuously.
  assert.ok(seed.length > 20, `the seed scan read nothing useful: ${JSON.stringify(seed)}`);
  assert.ok(push.length > 20, `the push scan read nothing useful: ${JSON.stringify(push)}`);
  assert.notEqual(seed, push, "the two scans must not have resolved to the same method body");
});
