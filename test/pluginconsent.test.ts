// What the install-time capability approval prompt says, and in what order
// (#377). DOM-free — the prompt's DOM/modal wiring lives in
// `pluginpaneview.ts` and is hand-validated per CLAUDE.md's convention.
// `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  consentLines,
  describeCapabilities,
  describeCapability,
  hasSensitiveCapability,
} from "../src/pluginconsent.ts";

test("the flagged tier is exactly fs.read and metrics.system", () => {
  // #377's own wording: those two read real data off this machine. `storage`
  // touches only its own namespaced blob and `panel` is inert, so flagging
  // either would train the human to ignore the flag.
  assert.equal(describeCapability("fs.read").sensitive, true);
  assert.equal(describeCapability("metrics.system").sensitive, true);
  assert.equal(describeCapability("storage").sensitive, false);
  assert.equal(describeCapability("panel").sensitive, false);
});

test("an unknown capability is described as unknown and treated as sensitive", () => {
  const d = describeCapability("network.exfiltrate");
  assert.equal(d.id, "network.exfiltrate");
  assert.equal(d.sensitive, true);
  assert.match(d.detail, /Unrecognized/);
});

test("the powerful capabilities sort first, ties alphabetically", () => {
  const order = describeCapabilities(["storage", "panel", "metrics.system", "fs.read"]).map((c) => c.id);
  assert.deepEqual(order, ["fs.read", "metrics.system", "panel", "storage"]);
});

test("the same declared set always renders in the same order", () => {
  // A prompt that reshuffles between openings of the same plugin teaches the
  // human to stop reading it.
  const a = consentLines(["metrics.system", "storage"]);
  const b = consentLines(["storage", "metrics.system"]);
  assert.deepEqual(a, b);
});

test("a duplicated capability is listed once", () => {
  assert.deepEqual(consentLines(["storage", "storage"]).length, 1);
});

test("every line leads with the capability string the manifest itself declares", () => {
  const lines = consentLines(["fs.read", "storage"]);
  assert.equal(lines.length, 2);
  assert.match(lines[0], /^⚠ fs\.read — /);
  assert.match(lines[1], /^storage — /);
});

test("an empty capability set says so instead of rendering an empty list", () => {
  assert.deepEqual(consentLines([]), [
    "No capabilities — it can draw in its pane and nothing else.",
  ]);
});

test("hasSensitiveCapability drives the extra warning sentence, and only for the flagged tier", () => {
  assert.equal(hasSensitiveCapability(["storage", "panel"]), false);
  assert.equal(hasSensitiveCapability(["storage", "fs.read"]), true);
  assert.equal(hasSensitiveCapability([]), false);
});
