// Unit tests for the pure ssh argv builder (#887 plan, slice S2). Run with
// `npm test` (Node's built-in test runner strips TS types natively — mirrors
// test/layout.test.ts). Most assertions are on the plain string[] the
// builder returns, with no sshd, no network, no process spawn — but the
// adversarial cmd.exe/sh sections below DO spawn a real local shell
// (permitted: frontend-only, no cargo/rustc involved) to execute the built
// remote-command string and check for a marker side effect, because an
// argv-level assertion alone previously certified a real cmd.exe injection
// (#906 review) — see PR body for the repro this closes.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import path from "node:path";
import {
  buildSshArgv,
  claudeSessionArgs,
  rewriteClaudeCommandForResume,
  sshResumeArgv,
  type SshCommandParams,
} from "../src/sshcommand.ts";

const FAKE_SSH = "./fake-ssh.sh"; // the test/hand-validation program-injection seam

// A stand-in for a real agent CLI name in the tests below that actually spawn
// a real local shell to execute the built remote-command string (the
// "real cmd.exe"/"real sh" sections). NEVER "claude" (or any other real agent
// CLI) as the executed program there: if it resolves on PATH, cmd.exe/sh
// launches the real thing, burning the user's paid credits (CLAUDE.md hard
// constraint 3) — the argv-level tests above are fine using "claude" because
// they only ever inspect the returned string[], nothing is spawned.
const NOT_A_REAL_CLI = "loomux-test-nonexistent-cli-8f3c1";

// ---------- login shell (no remote command) ----------

test("no remote command: just program, options, destination — no -t, no --", () => {
  const params: SshCommandParams = { destination: "user@host", remoteShell: "posix" };
  assert.deepEqual(buildSshArgv(FAKE_SSH, params), [FAKE_SSH, "user@host"]);
});

test("login shell still carries option flags when set", () => {
  const params: SshCommandParams = {
    destination: "user@host",
    port: 2222,
    identityFile: "/home/u/.ssh/id_ed25519",
    remoteShell: "posix",
  };
  assert.deepEqual(buildSshArgv(FAKE_SSH, params), [
    FAKE_SSH,
    "-p",
    "2222",
    "-i",
    "/home/u/.ssh/id_ed25519",
    "user@host",
  ]);
});

// ---------- remote command shape: -t, options, destination, --, one string ----------

test("remote command: -t precedes options, -- and one quoted string follow destination", () => {
  const params: SshCommandParams = {
    destination: "user@host",
    port: 22,
    remoteShell: "posix",
    remoteCommand: ["claude"],
  };
  assert.deepEqual(buildSshArgv(FAKE_SSH, params), [
    FAKE_SSH,
    "-t",
    "-p",
    "22",
    "user@host",
    "--",
    "exec 'claude'",
  ]);
});

test("remote command with cwd: posix cd-then-exec", () => {
  const params: SshCommandParams = {
    destination: "user@host",
    remoteShell: "posix",
    remoteCwd: "/srv/app",
    remoteCommand: ["claude", "--session-id", "abc-123"],
  };
  const argv = buildSshArgv(FAKE_SSH, params);
  assert.equal(argv.length, 5);
  assert.equal(argv[argv.length - 1], "cd '/srv/app' && exec 'claude' '--session-id' 'abc-123'");
});

test("remote command with cwd: cmd.exe cd /d then bare command", () => {
  const params: SshCommandParams = {
    destination: "user@host",
    remoteShell: "cmd",
    remoteCwd: "C:\\repos\\app",
    remoteCommand: ["claude"],
  };
  const argv = buildSshArgv(FAKE_SSH, params);
  assert.equal(argv[argv.length - 1], 'cd /d "C:\\repos\\app" && "claude"');
});

// ---------- flag emission matrix ----------

test("flag matrix: port alone", () => {
  const argv = buildSshArgv(FAKE_SSH, { destination: "h", port: 2200, remoteShell: "posix" });
  assert.deepEqual(argv, [FAKE_SSH, "-p", "2200", "h"]);
});

test("flag matrix: identityFile alone", () => {
  const argv = buildSshArgv(FAKE_SSH, { destination: "h", identityFile: "/k", remoteShell: "posix" });
  assert.deepEqual(argv, [FAKE_SSH, "-i", "/k", "h"]);
});

test("flag matrix: keepaliveSeconds alone emits ServerAliveInterval", () => {
  const argv = buildSshArgv(FAKE_SSH, { destination: "h", keepaliveSeconds: 30, remoteShell: "posix" });
  assert.deepEqual(argv, [FAKE_SSH, "-o", "ServerAliveInterval=30", "h"]);
});

test("flag matrix: keepaliveSeconds unset emits no -o at all", () => {
  const argv = buildSshArgv(FAKE_SSH, { destination: "h", remoteShell: "posix" });
  assert.ok(!argv.includes("-o"), "no -o flag should be present when keepaliveSeconds is unset");
});

test("flag matrix: extraArgs pass through verbatim and in order", () => {
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    extraArgs: ["-o", "Compression=yes", "-4"],
    remoteShell: "posix",
  });
  assert.deepEqual(argv, [FAKE_SSH, "-o", "Compression=yes", "-4", "h"]);
});

test("flag matrix: everything combined, in the documented order", () => {
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "user@host",
    port: 22,
    identityFile: "/k",
    keepaliveSeconds: 15,
    extraArgs: ["-4"],
    remoteShell: "posix",
    remoteCommand: ["claude"],
  });
  assert.deepEqual(argv, [
    FAKE_SSH,
    "-t",
    "-p",
    "22",
    "-i",
    "/k",
    "-o",
    "ServerAliveInterval=15",
    "-4",
    "user@host",
    "--",
    "exec 'claude'",
  ]);
});

// ---------- adversarial quoting: posix ----------

test("posix quoting: embedded single quote in remoteCwd is escaped, not a shell break-out", () => {
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "posix",
    remoteCwd: "/home/o'brien",
    remoteCommand: ["claude"],
  });
  const cmdString = argv[argv.length - 1];
  assert.equal(cmdString, "cd '/home/o'\\''brien' && exec 'claude'");
  // [program, -t, destination, --, cmdString] — the hostile cwd never split
  // the command into extra argv tokens at the local-spawn level.
  assert.equal(argv.length, 5);
});

test("posix quoting: hostile remote-command token with quotes, semicolons and backticks", () => {
  const hostile = "'; rm -rf ~; echo `whoami`";
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "posix",
    remoteCommand: ["claude", hostile],
  });
  const cmdString = argv[argv.length - 1];
  // Every embedded `'` is closed/escaped/reopened; nothing outside quotes.
  assert.equal(cmdString, "exec 'claude' ''\\''; rm -rf ~; echo `whoami`'");
  assert.equal(argv.length, 5);
  assert.equal(argv[2], "h"); // destination untouched by the hostile token
});

// ---------- adversarial quoting: cmd.exe ----------
//
// The cmd.exe path always prefixes the emitted string with `cd /d "." && `
// (or `cd /d "<remoteCwd>" && `) even when the caller passed no remoteCwd —
// see buildCmdRemoteCommand's doc comment for why the leading character
// matters to cmd.exe's own /C parsing. These argv-level assertions pin the
// string; the "real cmd.exe" section below proves what that string actually
// does when a real cmd.exe parses it.

test("cmd quoting: embedded double quote is doubled, not left dangling", () => {
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "cmd",
    remoteCommand: ["claude", 'arg-with-"quote'],
  });
  const cmdString = argv[argv.length - 1];
  assert.equal(cmdString, 'cd /d "." && "claude" "arg-with-""quote"');
  assert.equal(argv.length, 5);
});

test("cmd quoting: hostile token with metacharacters stays inside one quoted run", () => {
  const hostile = 'x" & echo PWNED & "y';
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "cmd",
    remoteCommand: ["claude", hostile],
  });
  const cmdString = argv[argv.length - 1];
  assert.equal(cmdString, 'cd /d "." && "claude" "x"" & echo PWNED & ""y"');
  // [program, -t, destination, --, cmdString] regardless of embedded
  // metacharacters — one trailing argv element, destination untouched.
  assert.equal(argv.length, 5);
  assert.equal(argv[2], "h");
});

// ---------- refusals: unknown shell family, newline, trailing backslash, destination ----------

test("unsupported remoteShell is refused, not silently mis-quoted (includes the old 'windows' literal)", () => {
  const params = {
    destination: "h",
    remoteShell: "windows",
    remoteCommand: ["claude"],
  } as unknown as SshCommandParams;
  assert.throws(() => buildSshArgv(FAKE_SSH, params), /unsupported remoteShell/);
});

test("unsupported remoteShell is refused for an arbitrary unrecognized value", () => {
  const params = {
    destination: "h",
    remoteShell: "powershell",
    remoteCommand: ["claude"],
  } as unknown as SshCommandParams;
  assert.throws(() => buildSshArgv(FAKE_SSH, params), /unsupported remoteShell/);
});

test("cmd path: a newline in a remote-command token is refused, not silently truncated", () => {
  assert.throws(
    () =>
      buildSshArgv(FAKE_SSH, {
        destination: "h",
        remoteShell: "cmd",
        remoteCommand: ["claude", "a\nwhoami"],
      }),
    /newline/,
  );
});

test("cmd path: a trailing backslash in a remote-command token is refused, not silently merged", () => {
  assert.throws(
    () =>
      buildSshArgv(FAKE_SSH, {
        destination: "h",
        remoteShell: "cmd",
        remoteCommand: ["claude", "C:\\dir\\"],
      }),
    /trailing/,
  );
});

test("cmd path: a trailing backslash in remoteCwd is NOT refused — cd is internal, not spawned", () => {
  // Unlike a remote-command token, remoteCwd is only ever cmd.exe's own `cd`
  // argument — never a spawned process's argv — so the CommandLineToArgvW
  // backslash-before-quote hazard cmdQuote refuses for tokens doesn't apply
  // here (verified against real cmd.exe in the "real cmd.exe" section
  // below). Refusing it would be strictly more restrictive than necessary
  // for something legitimate (`C:\repos\` is an ordinary Windows path).
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "cmd",
    remoteCwd: "C:\\repos\\",
    remoteCommand: ["claude"],
  });
  assert.equal(argv[argv.length - 1], 'cd /d "C:\\repos\\" && "claude"');
});

test("cmd path: a newline in remoteCwd is still refused, not silently truncated", () => {
  assert.throws(
    () =>
      buildSshArgv(FAKE_SSH, {
        destination: "h",
        remoteShell: "cmd",
        remoteCwd: "C:\\repos\nwhoami",
        remoteCommand: ["claude"],
      }),
    /newline/,
  );
});

test("cmd path: an empty-string remoteCwd is treated as absent, matching the posix path", () => {
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "cmd",
    remoteCwd: "",
    remoteCommand: ["claude"],
  });
  // Same as remoteCwd being entirely unset: `cd /d "." && ...`, not
  // `cd /d "" && ...` (which cmd.exe rejects, short-circuiting the `&&`
  // before the actual command ever runs).
  assert.equal(argv[argv.length - 1], 'cd /d "." && "claude"');
});

test("a destination starting with '-' is refused rather than reaching ssh as an option", () => {
  assert.throws(
    () => buildSshArgv(FAKE_SSH, { destination: "-oProxyCommand=calc", remoteShell: "posix" }),
    /destination/,
  );
});

// ---------- real-shell execution: does the built string actually inject? ----------
//
// Argv-level assertions above pin the *string* the builder returns; they
// cannot see what a real shell does with it, which is exactly how the
// previous cmd.exe scheme's leading-quote defect passed review undetected
// (#906). These spawn a real local cmd.exe / sh with the built string and
// check for a marker side effect, mirroring the reviewer's own repro
// technique (adapted to a harmless marker write instead of `del`).

const SCRATCH_DIR = path.join(process.cwd(), ".scratch");
mkdirSync(SCRATCH_DIR, { recursive: true });

function markerPath(name: string): string {
  return path.join(SCRATCH_DIR, `injection-marker-${process.pid}-${name}.txt`);
}

function cleanupMarker(file: string): void {
  if (existsSync(file)) rmSync(file, { force: true });
}

/** True if child_process can actually find/run `cmd.exe` here (skip, don't
 *  fail, off Windows or in an environment with no ComSpec). */
function cmdExeAvailable(): boolean {
  if (process.platform !== "win32") return false;
  const probe = spawnSync(process.env.ComSpec || "cmd.exe", ["/c", "exit 0"], { encoding: "utf8" });
  return probe.error === undefined && probe.status === 0;
}

function runRemoteStringInCmdExe(cmdString: string): { stdout: string; stderr: string; status: number | null } {
  // windowsVerbatimArguments: true is required here — without it Node
  // re-quotes the argv itself before CreateProcess, which would mask
  // exactly the leading-quote defect this suite exists to catch. This
  // directly exercises cmd.exe's own documented `/C` parsing rule (`cmd
  // /?`), which is the mechanism buildCmdRemoteCommand's fix depends on.
  // How an OpenSSH-for-Windows sshd itself invokes the remote command was
  // NOT independently verified here (not sourced from OpenSSH's own code
  // or docs) — this is `cmd.exe /c <string>` as an assumption, consistent
  // with `src/sshcommand.ts`'s own module comment, not a cited fact.
  const result = spawnSync(process.env.ComSpec || "cmd.exe", ["/c", cmdString], {
    windowsVerbatimArguments: true,
    encoding: "utf8",
  });
  return { stdout: result.stdout ?? "", stderr: result.stderr ?? "", status: result.status };
}

const CMD_AVAILABLE = cmdExeAvailable();
if (!CMD_AVAILABLE) {
  test("real cmd.exe injection probes — SKIPPED (no cmd.exe on this platform)", { skip: true }, () => {});
}

test(
  "real cmd.exe: reviewer's repro shape (embedded quote + '&') no longer injects when remoteCwd is unset",
  { skip: !CMD_AVAILABLE },
  () => {
    const marker = markerPath("repro-embedded-quote");
    cleanupMarker(marker);
    try {
      const hostile = `x" & echo PWNED>${marker} & "y`;
      const argv = buildSshArgv(FAKE_SSH, {
        destination: "h",
        remoteShell: "cmd",
        remoteCommand: [NOT_A_REAL_CLI, hostile],
      });
      const cmdString = argv[argv.length - 1];
      runRemoteStringInCmdExe(cmdString);
      assert.ok(
        !existsSync(marker),
        `injection marker was created — the embedded '&' ran as a separate command: ${cmdString}`,
      );
    } finally {
      cleanupMarker(marker);
    }
  },
);

test(
  "real cmd.exe: a bare metacharacter with no embedded quote does not inject (remoteCwd unset)",
  { skip: !CMD_AVAILABLE },
  () => {
    const marker = markerPath("bare-metachar");
    cleanupMarker(marker);
    try {
      const hostile = `a|echo PWNED>${marker}|b`;
      const argv = buildSshArgv(FAKE_SSH, {
        destination: "h",
        remoteShell: "cmd",
        remoteCommand: ["cmd", "/c", "echo", hostile],
      });
      const cmdString = argv[argv.length - 1];
      runRemoteStringInCmdExe(cmdString);
      assert.ok(!existsSync(marker), `injection marker was created via a bare '|': ${cmdString}`);
    } finally {
      cleanupMarker(marker);
    }
  },
);

test(
  "real cmd.exe: a metacharacter in a middle token of a realistic claude line does not inject",
  { skip: !CMD_AVAILABLE },
  () => {
    const marker = markerPath("middle-token");
    cleanupMarker(marker);
    try {
      const hostile = `x" & echo PWNED>${marker} & "y`;
      const argv = buildSshArgv(FAKE_SSH, {
        destination: "h",
        remoteShell: "cmd",
        remoteCommand: [NOT_A_REAL_CLI, "--append-system-prompt", hostile, "--session-id", "abc-123"],
      });
      const cmdString = argv[argv.length - 1];
      runRemoteStringInCmdExe(cmdString);
      assert.ok(!existsSync(marker), `injection marker was created from a middle token: ${cmdString}`);
    } finally {
      cleanupMarker(marker);
    }
  },
);

test(
  "real cmd.exe: remoteCwd-present form stays safe too (regression guard on the pre-fix-safe case)",
  { skip: !CMD_AVAILABLE },
  () => {
    const marker = markerPath("cwd-present");
    cleanupMarker(marker);
    try {
      const hostile = `x" & echo PWNED>${marker} & "y`;
      const argv = buildSshArgv(FAKE_SSH, {
        destination: "h",
        remoteShell: "cmd",
        remoteCwd: "C:\\Windows",
        remoteCommand: [NOT_A_REAL_CLI, hostile],
      });
      const cmdString = argv[argv.length - 1];
      runRemoteStringInCmdExe(cmdString);
      assert.ok(!existsSync(marker), `injection marker was created with remoteCwd set: ${cmdString}`);
    } finally {
      cleanupMarker(marker);
    }
  },
);

test("real cmd.exe: a benign quoted argument is delivered to the child intact", { skip: !CMD_AVAILABLE }, () => {
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "cmd",
    remoteCommand: ["cmd", "/c", "echo", "hello world"],
  });
  const cmdString = argv[argv.length - 1];
  const result = runRemoteStringInCmdExe(cmdString);
  assert.ok(result.stdout.includes("hello world"), `expected literal echo, got stdout=${result.stdout}`);
});

test(
  "real cmd.exe: a trailing-backslash remoteCwd is consumed correctly by cd (not mangled)",
  { skip: !CMD_AVAILABLE },
  () => {
    // Positive control for the trailing-backslash relaxation above: `cd` is
    // internal to cmd.exe, so a trailing backslash before the closing quote
    // doesn't trigger the CommandLineToArgvW-style merge a spawned
    // program's argv would suffer from cmdQuote (see the "benign quoted
    // argument" test, which stays on the token path, not the cwd path).
    // `dir`'s output is the observable: a directory listing that names
    // something System32-specific (`notepad.exe`, always present) and NOT
    // this repo's own directory contents proves the `cd /d` actually landed
    // there. (A nested `cmd /c cd` was tried here first to read `%CD%`/the
    // bare cd-with-no-args back directly, but ran into an unrelated cmd.exe
    // quirk — a bare "cd" as a nested command's sole argument right after an
    // outer `cd` intermittently fails to resolve as a command at all, on
    // both trailing-backslash and plain cwds alike — so it says nothing
    // about the backslash relaxation being tested here; `dir` sidesteps it.)
    const argv = buildSshArgv(FAKE_SSH, {
      destination: "h",
      remoteShell: "cmd",
      remoteCwd: "C:\\Windows\\System32\\",
      remoteCommand: ["cmd", "/c", "dir"],
    });
    const cmdString = argv[argv.length - 1];
    const result = runRemoteStringInCmdExe(cmdString);
    assert.ok(
      result.stdout.toLowerCase().includes("notepad.exe"),
      `expected a System32 directory listing, got stdout=${result.stdout}`,
    );
    assert.ok(!result.stdout.includes("CLAUDE.md"), "cd did not actually leave the repo directory");
  },
);

/** True if child_process can actually find/run `sh` here. */
function shAvailable(): boolean {
  const probe = spawnSync("sh", ["-c", "exit 0"], { encoding: "utf8" });
  return probe.error === undefined && probe.status === 0;
}

const SH_AVAILABLE = shAvailable();
if (!SH_AVAILABLE) {
  test("real sh injection probes — SKIPPED (no sh on PATH in this environment)", { skip: true }, () => {});
}

test("real sh: hostile token with quotes, semicolons and backticks does not inject", { skip: !SH_AVAILABLE }, () => {
  const marker = markerPath("posix-hostile");
  cleanupMarker(marker);
  try {
    const hostile = `'; touch ${marker}; echo \`whoami\``;
    const argv = buildSshArgv(FAKE_SSH, {
      destination: "h",
      remoteShell: "posix",
      remoteCommand: ["echo", hostile],
    });
    const cmdString = argv[argv.length - 1];
    spawnSync("sh", ["-c", cmdString], { encoding: "utf8" });
    assert.ok(!existsSync(marker), `injection marker was created via posix quoting: ${cmdString}`);
  } finally {
    cleanupMarker(marker);
  }
});

test("real sh: remoteCwd break-out attempt fails closed, no marker", { skip: !SH_AVAILABLE }, () => {
  const marker = markerPath("posix-cwd-breakout");
  cleanupMarker(marker);
  try {
    const argv = buildSshArgv(FAKE_SSH, {
      destination: "h",
      remoteShell: "posix",
      remoteCwd: `/nonexistent' && touch ${marker} && echo '`,
      remoteCommand: ["echo", "hi"],
    });
    const cmdString = argv[argv.length - 1];
    spawnSync("sh", ["-c", cmdString], { encoding: "utf8" });
    assert.ok(!existsSync(marker), `injection marker was created via a hostile remoteCwd: ${cmdString}`);
  } finally {
    cleanupMarker(marker);
  }
});

test("real sh: benign command's literal output is observable (positive control)", { skip: !SH_AVAILABLE }, () => {
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "posix",
    remoteCommand: ["echo", "hello world"],
  });
  const cmdString = argv[argv.length - 1];
  const result = spawnSync("sh", ["-c", cmdString], { encoding: "utf8" });
  assert.ok(result.stdout.includes("hello world"), `expected literal echo, got stdout=${result.stdout}`);
});

// ---------- program injection seam ----------

test("program parameter is fully substitutable (the fake-ssh test seam)", () => {
  const argv = buildSshArgv("/tmp/stub-not-real-ssh", { destination: "h", remoteShell: "posix" });
  assert.equal(argv[0], "/tmp/stub-not-real-ssh");
});

// ---------- fresh vs resume ----------

test("claudeSessionArgs: fresh mints --session-id, resume uses --resume, same id", () => {
  assert.deepEqual(claudeSessionArgs("sess-1", "fresh"), ["--session-id", "sess-1"]);
  assert.deepEqual(claudeSessionArgs("sess-1", "resume"), ["--resume", "sess-1"]);
});

test("rewriteClaudeCommandForResume: replaces a fresh --session-id pair with --resume", () => {
  const fresh = ["claude", "--session-id", "sess-1", "--extra", "flag"];
  assert.deepEqual(rewriteClaudeCommandForResume(fresh, "sess-1"), [
    "claude",
    "--resume",
    "sess-1",
    "--extra",
    "flag",
  ]);
});

test("rewriteClaudeCommandForResume: also handles an already-resume command idempotently", () => {
  const alreadyResume = ["claude", "--resume", "sess-1"];
  assert.deepEqual(rewriteClaudeCommandForResume(alreadyResume, "sess-1"), ["claude", "--resume", "sess-1"]);
});

test("rewriteClaudeCommandForResume: no session flag present just prepends --resume", () => {
  assert.deepEqual(rewriteClaudeCommandForResume(["claude"], "sess-1"), ["claude", "--resume", "sess-1"]);
});

test("sshResumeArgv: fresh vs resume produce different command-string shapes for the same session", () => {
  const params: SshCommandParams = {
    destination: "user@host",
    remoteShell: "posix",
    remoteCommand: ["claude", ...claudeSessionArgs("sess-42", "fresh")],
  };
  const freshArgv = buildSshArgv(FAKE_SSH, params);
  const resumeArgv = sshResumeArgv(FAKE_SSH, params, "sess-42");

  const freshCmd = freshArgv[freshArgv.length - 1];
  const resumeCmd = resumeArgv[resumeArgv.length - 1];

  assert.equal(freshCmd, "exec 'claude' '--session-id' 'sess-42'");
  assert.equal(resumeCmd, "exec 'claude' '--resume' 'sess-42'");
  assert.notEqual(freshCmd, resumeCmd);
  // Everything else about the argv (options, destination, --) is unchanged.
  assert.deepEqual(freshArgv.slice(0, -1), resumeArgv.slice(0, -1));
});

test("sshResumeArgv: throws when there is no remote command to rewrite (login-shell panes never resume)", () => {
  const params: SshCommandParams = { destination: "user@host", remoteShell: "posix" };
  assert.throws(() => sshResumeArgv(FAKE_SSH, params, "sess-1"));
});
