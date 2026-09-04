// The Key-passphrase decision, as pure functions (#2368 slice A) — DOM-free and
// unit-tested (test/sshagent.test.ts) like layout.ts/steer.ts/sshcommand.ts.
//
// Two questions live here and nothing else does:
//
//  1. **Should orrerix run `ssh-add` at all?** (`sshPassphraseGate`) — a blank
//     passphrase is not an error, it is *today's behaviour*: ssh asks in the
//     pane, because the pane is a terminal. So the gate's default answer is
//     "skip", and the field is an opt-in shortcut rather than a new requirement.
//  2. **What does the human read when it didn't work?** (`sshAddRefusal`) — one
//     paragraph per outcome, each of which names the blank-field escape, because
//     a refusal that leaves someone stuck is worse than the prompt it replaced.
//
// What is deliberately NOT here: the passphrase itself never reaches a type in
// this module beyond the gate's input, and nothing in this file, in
// `panesetup.ts`, or in the spawned argv can carry one. See
// `src-tauri/src/sshagent.rs`'s module doc for the whole posture.

/** What one `ssh-add` run did — the wire shape of the `ssh_add_identity`
 *  command, mirroring `SshAddOutcome` in `src-tauri/src/sshagent.rs`.
 *
 *  Tagged on `kind` so this is a discriminated union the compiler can check
 *  exhaustively: adding a variant on the Rust side and forgetting it here is a
 *  `tsc` error at `sshAddRefusal`'s switch, not a silent fallthrough. */
export type SshAddOutcome =
  | { kind: "added" }
  | { kind: "badPassphrase"; detail: string }
  | { kind: "noAgent"; hint: string }
  | { kind: "timeout" }
  | { kind: "failed"; detail: string };

/** What the launcher does with the Key-passphrase field on submit. */
export type SshPassphraseAction = "skip" | "add";

/** The inputs the gate reads. Both are what the form holds *right now*, not
 *  anything persisted: `sshprofiles.json` has no passphrase field and does not
 *  gain one in this slice. */
export interface SshPassphraseInputs {
  /** The `-i` path, or null/blank when the connection has none. */
  identityFile: string | null;
  /** What the human typed into **Key passphrase**, verbatim. */
  passphrase: string;
}

/** Whether to run `ssh-add` before spawning the pane.
 *
 *  `"skip"` is the default and the safe answer: it is exactly the pre-#2368
 *  behaviour, in which ssh asks for the passphrase inside the pane. It is
 *  returned in two cases, and both are ordinary rather than exceptional:
 *
 *  - **No passphrase typed** — the human is using an agent that already holds
 *    the key, an unencrypted key, or password/2FA auth. Running `ssh-add` with
 *    an empty passphrase would ask ssh-add to load a key it cannot decrypt.
 *  - **No identity file** — there is nothing to name to `ssh-add`. A passphrase
 *    without a key path is a typo, not an instruction, and guessing which key
 *    was meant is exactly the kind of guess this feature must not make.
 *
 *  Whitespace-only counts as blank on both, because a stray space in a password
 *  field is invisible and would otherwise turn a no-op into a failed launch. */
export function sshPassphraseGate(inputs: SshPassphraseInputs): SshPassphraseAction {
  const key = (inputs.identityFile ?? "").trim();
  const pass = inputs.passphrase.trim();
  if (!key || !pass) return "skip";
  return "add";
}

/** Collapse foreign text to a single line.
 *
 *  `detail` and `hint` come back from a program orrerix did not write — the real
 *  `ssh-add`, or something standing in for it — so their shape is not a promise
 *  anyone here made. The one-paragraph rule (`CLAUDE.md`: a user-facing message
 *  is ONE paragraph) has to hold over whatever they contain, which is what this
 *  makes true rather than assumed. */
function oneLine(text: string): string {
  return text.replace(/\s+/g, " ").trim();
}

/** The escape every refusal ends with. One string rather than five copies: the
 *  promise is that a human who cannot get the passphrase to work is never
 *  blocked, and a promise made in five places drifts. */
const BLANK_FIELD_ESCAPE =
  "leave the Key passphrase field blank and let ssh ask you inside the pane instead";

/** What to show the human, or `null` when there is nothing to say.
 *
 *  Every non-`added` outcome states three things in one paragraph: what
 *  happened, that **no pane was opened**, and the blank-field escape. The middle
 *  one matters as much as the first — a launch that refuses without saying so
 *  reads as a launch that hung.
 *
 *  Exhaustive over `SshAddOutcome` by construction: the `never` binding in the
 *  default arm is a compile error the day the Rust enum grows a variant this
 *  file has not been taught. */
export function sshAddRefusal(outcome: SshAddOutcome): string | null {
  switch (outcome.kind) {
    case "added":
      return null;
    case "badPassphrase":
      return `ssh-add rejected that passphrase, so the key was not loaded and no pane was opened — it said: ${oneLine(
        outcome.detail
      )}. Retype the passphrase and try again, or ${BLANK_FIELD_ESCAPE}.`;
    case "noAgent":
      return `orrerix could not reach an ssh-agent to load the key into, so no pane was opened. ${oneLine(
        outcome.hint
      )}. Then try again, or ${BLANK_FIELD_ESCAPE}.`;
    case "timeout":
      return `ssh-add did not answer in time and was stopped, so no pane was opened. Try again, or ${BLANK_FIELD_ESCAPE}.`;
    case "failed":
      return `orrerix could not load the key into your ssh-agent, so no pane was opened — ${oneLine(
        outcome.detail
      )}. Try again, or ${BLANK_FIELD_ESCAPE}.`;
    default: {
      const unreachable: never = outcome;
      return unreachable;
    }
  }
}
