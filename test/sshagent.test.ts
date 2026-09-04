// The Key-passphrase decision (#2368 slice A) — sshagent.ts. DOM-free, like
// sshcommand.test.ts: the gate and the refusal text are the whole of what this
// feature decides, and both are pure. What is NOT tested here is the hidden-pty
// conversation, which is Rust and lives in `src-tauri/tests/sshagent.rs` driven
// against a fake `ssh-add` (CLAUDE.md constraint 3 — no real ssh CLI, ever).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  sshAddRefusal,
  sshPassphraseGate,
  type SshAddOutcome,
} from "../src/sshagent.ts";

// ---------- the gate: skip is the default, and it is today's behaviour ----------

test("both a key path and a passphrase means run ssh-add", () => {
  // The negative control for every skip below: if this were also "skip", the
  // whole feature would be dead code and every other assertion here would still
  // pass.
  assert.equal(
    sshPassphraseGate({ identityFile: "C:\\Users\\me\\.ssh\\id_ed25519", passphrase: "hunter2" }),
    "add"
  );
});

test("a blank passphrase skips — ssh asks in the pane, exactly as before #2368", () => {
  const key = "C:\\Users\\me\\.ssh\\id_ed25519";
  assert.equal(sshPassphraseGate({ identityFile: key, passphrase: "" }), "skip");
  // Whitespace-only counts as blank: a stray space in a password field is
  // invisible, and treating it as a passphrase would turn a no-op into a failed
  // launch the human cannot see the cause of.
  assert.equal(sshPassphraseGate({ identityFile: key, passphrase: "   " }), "skip");
  assert.equal(sshPassphraseGate({ identityFile: key, passphrase: "\t\n " }), "skip");
});

test("a passphrase with no identity file skips — there is nothing to name to ssh-add", () => {
  // Guessing which key was meant is precisely the guess this feature must not
  // make: an agent holds keys by path, and the wrong one is not a near miss.
  assert.equal(sshPassphraseGate({ identityFile: null, passphrase: "hunter2" }), "skip");
  assert.equal(sshPassphraseGate({ identityFile: "", passphrase: "hunter2" }), "skip");
  assert.equal(sshPassphraseGate({ identityFile: "   ", passphrase: "hunter2" }), "skip");
});

// ---------- the refusals: one paragraph, and never a dead end ----------

/** Every non-`added` outcome, so the shape assertions below run over the whole
 *  union rather than over the two variants someone remembered. */
const REFUSING: SshAddOutcome[] = [
  { kind: "badPassphrase", detail: "Bad passphrase, try again for /home/a/.ssh/id_ed25519: " },
  { kind: "noAgent", hint: "Set-Service ssh-agent -StartupType Automatic; Start-Service ssh-agent" },
  { kind: "timeout" },
  { kind: "failed", detail: "ssh-add was not found beside your ssh client or on PATH." },
];

test("success says nothing", () => {
  assert.equal(sshAddRefusal({ kind: "added" }), null);
});

test("every refusal is ONE paragraph", () => {
  // CLAUDE.md's user-facing-message rule, pinned as a SHAPE rather than by
  // quoting the wording: no newline, and no run of ten spaces (the two ways a
  // source-literal line break leaks the source's own indentation to a reader).
  assert.equal(REFUSING.length, 4, "every refusing variant must be covered");
  for (const outcome of REFUSING) {
    const msg = sshAddRefusal(outcome);
    assert.ok(msg, `${outcome.kind} must produce a message`);
    assert.ok(!msg.includes("\n"), `${outcome.kind} must not contain a newline`);
    assert.ok(!/ {10}/.test(msg), `${outcome.kind} must not contain a ten-space run`);
  }
});

test("every refusal offers the blank-field escape", () => {
  // The promise the whole feature rests on: a human who cannot make the
  // passphrase work is never locked out — blanking the field restores the
  // pre-#2368 path, where ssh asks inside the pane. A refusal that omits this
  // is a dead end.
  for (const outcome of REFUSING) {
    const msg = sshAddRefusal(outcome)!;
    assert.match(
      msg,
      /leave the Key passphrase field blank/,
      `${outcome.kind} must name the blank-field escape`
    );
  }
});

test("every refusal says no pane was opened", () => {
  // A launch that refuses without saying so reads as a launch that hung.
  for (const outcome of REFUSING) {
    assert.match(sshAddRefusal(outcome)!, /no pane was opened/, `${outcome.kind}`);
  }
});

test("a bad passphrase quotes ssh-add's own words", () => {
  const msg = sshAddRefusal({
    kind: "badPassphrase",
    detail: "Bad passphrase, try again for /home/a/.ssh/id_ed25519: ",
  })!;
  assert.match(msg, /Bad passphrase, try again for \/home\/a\/\.ssh\/id_ed25519/);
  assert.match(msg, /Retype the passphrase/);
});

test("no agent carries the one-time fix verbatim", () => {
  // The refusal is only useful if the human can act on it without going to find
  // the command, so the backend's hint must reach the screen intact.
  const hint = "Set-Service ssh-agent -StartupType Automatic; Start-Service ssh-agent";
  assert.match(sshAddRefusal({ kind: "noAgent", hint })!, /Set-Service ssh-agent -StartupType Automatic; Start-Service ssh-agent/);
});

test("multi-line foreign text is collapsed rather than trusted", () => {
  // `detail` and `hint` come back from a program orrerix did not write, so the
  // one-paragraph property has to hold over whatever they contain. Without the
  // collapse, this outcome alone would break the shape every other refusal is
  // pinned on — which is why it is asserted on a detail that really has a
  // newline in it rather than on the tidy ones above.
  const msg = sshAddRefusal({ kind: "failed", detail: "line one\n          line two" })!;
  assert.ok(!msg.includes("\n"), "a newline in `detail` must not reach the reader");
  assert.ok(!/ {10}/.test(msg), "indentation in `detail` must not reach the reader");
  assert.match(msg, /line one line two/, "…and the words themselves survive");
});
