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
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
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
    "exec \"$SHELL\" -l -i -c 'exec '\\''claude'\\'''",
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
  // inner (pre-#2395 shape, now the -c argument): cd '/srv/app' && exec 'claude' '--session-id' 'abc-123'
  assert.equal(argv[argv.length - 1], "exec \"$SHELL\" -l -i -c 'cd '\\''/srv/app'\\'' && exec '\\''claude'\\'' '\\''--session-id'\\'' '\\''abc-123'\\'''");
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
    "exec \"$SHELL\" -l -i -c 'exec '\\''claude'\\'''",
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
  // inner (pre-#2395 shape, now the -c argument): cd '/home/o'\''brien' && exec 'claude'
  assert.equal(cmdString, "exec \"$SHELL\" -l -i -c 'cd '\\''/home/o'\\''\\'\\'''\\''brien'\\'' && exec '\\''claude'\\'''");
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
  // inner (pre-#2395 shape, now the -c argument): exec 'claude' ''\''; rm -rf ~; echo `whoami`'
  assert.equal(cmdString, "exec \"$SHELL\" -l -i -c 'exec '\\''claude'\\'' '\\'''\\''\\'\\'''\\''; rm -rf ~; echo `whoami`'\\'''");
  assert.equal(argv.length, 5);
  assert.equal(argv[2], "h"); // destination untouched by the hostile token
});

// ---------- #2395: the posix remote command runs under a login+interactive shell ----------
//
// sshd executes a remote command as `$SHELL -c '<cmd>'` — non-login AND
// non-interactive — so a user-installed CLI (nvm, ~/.local/bin, ~/.npm-global/bin)
// is off PATH and `exec copilot` dies with "not found" even though an interactive
// login on the same host finds it. The posix builder therefore re-enters the
// user's own shell as a login+interactive one and hands it the previous shape as
// a single, once-more-quoted argument.

test("#2395 posix: the remote command is wrapped in an interactive login shell, cwd form", () => {
  const params: SshCommandParams = {
    destination: "user@host",
    remoteShell: "posix",
    remoteCwd: "/srv/app",
    remoteCommand: ["claude", "--session-id", "abc-123"],
  };
  const argv = buildSshArgv(FAKE_SSH, params);
  assert.equal(
    argv[argv.length - 1],
    `exec "$SHELL" -l -i -c 'cd '\\''/srv/app'\\'' && exec '\\''claude'\\'' '\\''--session-id'\\'' '\\''abc-123'\\'''`,
  );
  // Still exactly ONE argv token after `--` — the wrap adds a quoting layer to
  // the string, never a second argv word.
  assert.equal(argv.length, 5);
});

test("#2395 posix: the wrap applies to the no-cwd form too", () => {
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "user@host",
    remoteShell: "posix",
    remoteCommand: ["claude"],
  });
  assert.equal(argv[argv.length - 1], `exec "$SHELL" -l -i -c 'exec '\\''claude'\\'''`);
});

test("#2395 posix: BOTH -l and -i are emitted, and $SHELL is not hardcoded to a shell name", () => {
  // The two flags are load-bearing for different files and neither alone fixes
  // the bug: `-l` misses nvm/`~/.local/bin` exports that live past Ubuntu
  // `.bashrc`'s `case $- in *i*)` early return, `-i` misses `~/.profile`. And
  // `$SHELL` rather than `bash` because a zsh/fish account must not be handed a
  // bash login. See doc/design/ssh-panes.md.
  const cmdString = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "posix",
    remoteCommand: ["copilot"],
  })[4];
  assert.ok(cmdString.startsWith(`exec "$SHELL" -l -i -c `), `unexpected wrap prefix: ${cmdString}`);
  assert.doesNotMatch(cmdString, /\b(bash|zsh|fish|sh) -l/, "the wrap must not name a concrete shell");
});

test("#2395 negative control: with no remoteCommand the argv is untouched — no wrap, no -t, no --", () => {
  // The plain-login-shell form is what already worked on the reported host
  // (sshd runs a real login shell itself), so it must not acquire the wrap.
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "user@host",
    remoteShell: "posix",
    remoteCwd: "/srv/app",
    port: 2222,
  });
  assert.deepEqual(argv, [FAKE_SSH, "-p", "2222", "user@host"]);
  assert.ok(!argv.some((a) => a.includes("$SHELL")));
});

test("#2395 negative control: the cmd.exe scheme is byte-identical to its pre-#2395 shape", () => {
  // cmd.exe has no rc-file PATH problem of this shape: it has no login/
  // non-login distinction and no per-user rc file that sshd's invocation
  // skips, so there is nothing for a wrap to recover — and `cmd.exe` would
  // parse `"$SHELL"` as a literal, not an expansion.
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "user@host",
    remoteShell: "cmd",
    remoteCwd: "C:\\repos\\app",
    remoteCommand: ["claude", "--session-id", "abc-123"],
  });
  assert.equal(argv[argv.length - 1], 'cd /d "C:\\repos\\app" && "claude" "--session-id" "abc-123"');
  assert.ok(!argv[argv.length - 1].includes("$SHELL"));
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

test(
  "real cmd.exe: hostile remoteCwd (embedded quote + metacharacter) does not inject — pins cmdQuoteCwd's doubling",
  { skip: !CMD_AVAILABLE },
  () => {
    // Regression pin: cmdQuoteCwd doubles an embedded `"` in remoteCwd the
    // same way cmdQuote does for a remote-command token. Every OTHER
    // cmd.exe test above puts its hostile character in the remote-command
    // token, not remoteCwd, so none of them would catch a regression in
    // cmdQuoteCwd's own doubling specifically — deleting the
    // `.replace(/"/g, '""')` there leaves the rest of this suite green
    // while the emitted string carries a live injection. Mirrors "real sh:
    // remoteCwd break-out attempt fails closed" below, on the cmd.exe side.
    const marker = markerPath("cmd-hostile-cwd");
    cleanupMarker(marker);
    try {
      const hostileCwd = `x" & echo PWNED>${marker} & "y`;
      const argv = buildSshArgv(FAKE_SSH, {
        destination: "h",
        remoteShell: "cmd",
        remoteCwd: hostileCwd,
        remoteCommand: ["cmd", "/c", "echo", "ok"],
      });
      const cmdString = argv[argv.length - 1];
      runRemoteStringInCmdExe(cmdString);
      assert.ok(!existsSync(marker), `injection marker was created via a hostile remoteCwd: ${cmdString}`);
    } finally {
      cleanupMarker(marker);
    }
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

/** Runs a built posix remote-command string the way sshd would: hand it to a
 *  shell as `-c <one string>`.
 *
 *  `SHELL` is forced rather than inherited because the #2395 wrap expands it,
 *  and a CI runner need not have it set at all (a Windows runner typically does
 *  not) — which would leave `exec "" -l -i -c …` and turn a quoting test into an
 *  environment test. On the real path there is nothing to force: sshd sets
 *  `SHELL` in the remote command's environment from the account's passwd entry,
 *  falling back to `_PATH_BSHELL` when that field is empty
 *  (openssh-portable `session.c`: `child_set_env(&env, &envsize, "SHELL", shell)`
 *  over `shell = (pw->pw_shell[0] == '\0') ? _PATH_BSHELL : pw->pw_shell`), so it
 *  is always set and always non-empty there.
 *
 *  Note `-i` makes bash complain about job control on stderr when stdin is not a
 *  tty ("cannot set terminal process group"); assertions below read stdout. */
function runRemoteStringInSh(cmdString: string, rcFile?: string): ReturnType<typeof spawnSync> {
  const env: Record<string, string | undefined> = { ...process.env, SHELL: "/bin/sh" };
  // POSIX `ENV`: the file an INTERACTIVE POSIX shell sources at startup — the
  // one lever that lets a test observe "did an rc file run?" without touching
  // the runner's HOME. `delete` rather than leave inherited, so a run that
  // passes no rcFile cannot accidentally source the developer's own.
  if (rcFile) env.ENV = rcFile.split(path.sep).join("/");
  else delete env.ENV;
  return spawnSync("sh", ["-c", cmdString], { encoding: "utf8", env });
}

const RC_FILES: string[] = [];
process.on("exit", () => {
  for (const f of RC_FILES) if (existsSync(f)) rmSync(f, { force: true });
});

/** Writes the rc file `runRemoteStringInSh`'s `ENV` points at. It both prints a
 *  marker and EXPORTS one, because those are two different claims: that the rc
 *  file ran at all, and that what it exported reaches the environment of the
 *  program the remote command finally `exec`s — which is the shape of the actual
 *  #2395 bug, where the missing export is `PATH`. */
function writeRcFile(name: string): string {
  const file = path.join(SCRATCH_DIR, `rc-${process.pid}-${name}.sh`);
  writeFileSync(file, "echo RC-2395-SOURCED\nLOOMUX_2395_FROM_RC=on\nexport LOOMUX_2395_FROM_RC\n");
  RC_FILES.push(file);
  return file;
}

/** The pre-#2395 single-layer posix shape, reimplemented here on purpose: it is
 *  the positive control for the round-trips below, so it must not move when the
 *  builder does. Same posix quoting scheme, no `"$SHELL" -l -i -c` wrap. */
function legacyPosixRemoteCommand(remoteCwd: string | undefined, remoteCommand: string[]): string {
  const q = (t: string) => `'${t.replace(/'/g, `'\\''`)}'`;
  const cmd = remoteCommand.map(q).join(" ");
  return remoteCwd ? `cd ${q(remoteCwd)} && exec ${cmd}` : `exec ${cmd}`;
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
    runRemoteStringInSh(cmdString);
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
    runRemoteStringInSh(cmdString);
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
  const result = runRemoteStringInSh(cmdString);
  assert.ok(result.stdout.includes("hello world"), `expected literal echo, got stdout=${result.stdout}`);
});

// ---------- #2395 round-trips through a real sh ----------
//
// The argv-level tests above pin the string; these EXECUTE it the way sshd does
// — `<the account's shell> -c <one string>` — so two claims get measured rather
// than asserted:
//
//  - that the wrap really re-enters an interactive LOGIN shell. The lever is
//    POSIX `ENV`, the file an interactive POSIX shell sources at startup: the
//    pre-#2395 shape reaches sshd's non-interactive shell, which sources
//    nothing, so `RC-2395-SOURCED` appearing at all is the fix. That is the
//    same mechanism as the reported bug one variable over — there it is `PATH`
//    that an unsourced `~/.profile`/`.bashrc` never exported, and `copilot` is
//    then not found.
//  - that the inner command is quoted exactly ONCE more. Under-quote it and the
//    outer shell eats the `&&`, the spaces and the `$`; over-quote it and
//    literal quote characters reach the payload. Neither shows up in a string
//    comparison written by the same hand that wrote the builder.

test("#2395 real sh: an rc file the pre-fix shape never sourced now reaches the exec'd program", { skip: !SH_AVAILABLE }, () => {
  const rc = writeRcFile("reaches-env");
  const legacyInner = legacyPosixRemoteCommand(undefined, ["sh", "-c", "echo cli-sees:$LOOMUX_2395_FROM_RC"]);
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "posix",
    remoteCommand: ["sh", "-c", "echo cli-sees:$LOOMUX_2395_FROM_RC"],
  });

  const fixed = runRemoteStringInSh(argv[argv.length - 1], rc);
  assert.equal(fixed.status, 0, `remote string did not parse: stderr=${fixed.stderr}`);
  assert.match(fixed.stdout, /RC-2395-SOURCED/, `the rc file did not run: stdout=${fixed.stdout}`);
  assert.match(fixed.stdout, /cli-sees:on/, `the rc file's export did not reach the exec'd program: stdout=${fixed.stdout}`);

  // The control that makes the two assertions above mean something: the SAME
  // rc file, the SAME probe, under the pre-#2395 single-layer shape — the
  // program still runs, and sees neither. So what changed is the shell the
  // command runs under, not the harness.
  const legacy = runRemoteStringInSh(legacyInner, rc);
  assert.equal(legacy.status, 0, `control did not parse: stderr=${legacy.stderr}`);
  assert.doesNotMatch(legacy.stdout, /RC-2395-SOURCED/);
  assert.match(legacy.stdout, /cli-sees:$/m, `control probe did not run at all: stdout=${legacy.stdout}`);
});

test("#2395 real sh: the inner command is quoted exactly once — a quote/space/$ payload arrives verbatim", { skip: !SH_AVAILABLE }, () => {
  // `'` proves the escape/reopen ran at both layers, `$NOT_EXPANDED_2395`
  // proves nothing was left unquoted for a shell to expand, and the double
  // space proves no word-splitting happened on the way through.
  const rc = writeRcFile("verbatim");
  const payload = "a'b  $NOT_EXPANDED_2395 c";
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "posix",
    remoteCommand: ["echo", payload],
  });
  const result = runRemoteStringInSh(argv[argv.length - 1], rc);
  assert.equal(result.status, 0, `remote string did not parse: stderr=${result.stderr}`);
  // The rc marker pins that this ran through the wrap, so the verbatim
  // assertion below is a statement about TWO quoting layers, not one.
  assert.match(result.stdout, /RC-2395-SOURCED/, `not running under the wrap: stdout=${result.stdout}`);
  assert.equal(result.stdout.trimEnd().split("\n").pop(), payload);
});

test("#2395 real sh: a cwd with a quote, a space and a $ survives both layers", { skip: !SH_AVAILABLE }, () => {
  // A REAL directory, so the `cd` either succeeds and the `&&` runs, or fails
  // and nothing is echoed — the marker is the whole assertion. It lives in the
  // same SCRATCH_DIR the injection markers use, so no temp-dir symlink (macOS's
  // /var -> /private/var) can make the created path and the `cd` argument
  // differ for a reason unrelated to quoting.
  const rc = writeRcFile("hostile-cwd");
  const dir = path.join(SCRATCH_DIR, "loomux-2395 o'brien $HOME");
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  try {
    const argv = buildSshArgv(FAKE_SSH, {
      destination: "h",
      remoteShell: "posix",
      remoteCwd: dir,
      remoteCommand: ["echo", "CWD-2395-OK"],
    });
    const result = runRemoteStringInSh(argv[argv.length - 1], rc);
    assert.match(result.stdout, /RC-2395-SOURCED/, `not running under the wrap: stdout=${result.stdout}`);
    assert.match(
      result.stdout,
      /CWD-2395-OK/,
      `cd into a quote/space/$ cwd did not survive the wrap: stdout=${result.stdout} stderr=${result.stderr}`,
    );

    // Positive control on the PAYLOAD: the same cwd under the pre-#2395
    // single-layer shape reaches `cd` too (and, being non-interactive, sources
    // no rc). Both shapes delivering the same directory is what makes this a
    // statement about the added layer being inert to the payload, rather than a
    // re-measurement of quoting that already worked.
    const legacy = runRemoteStringInSh(legacyPosixRemoteCommand(dir, ["echo", "CWD-2395-LEGACY"]), rc);
    assert.doesNotMatch(legacy.stdout, /RC-2395-SOURCED/);
    assert.match(
      legacy.stdout,
      /CWD-2395-LEGACY/,
      `control failed — the single-layer shape could not reach this cwd either: stderr=${legacy.stderr}`,
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("#2395 real sh: the wrapped form still refuses a remoteCwd break-out", { skip: !SH_AVAILABLE }, () => {
  // A REGRESSION guard, not a claim about the wrap: the identical probe passes
  // against the pre-#2395 shape too (see "real sh: remoteCwd break-out attempt
  // fails closed" above). It is restated against the wrap so a future edit to
  // the wrap cannot quietly reopen the hole the extra layer sits on top of.
  const marker = markerPath("posix-2395-wrapped-breakout");
  cleanupMarker(marker);
  try {
    const argv = buildSshArgv(FAKE_SSH, {
      destination: "h",
      remoteShell: "posix",
      remoteCwd: `/nonexistent' && touch ${marker} && echo '`,
      remoteCommand: ["echo", "hi"],
    });
    runRemoteStringInSh(argv[argv.length - 1]);
    assert.ok(!existsSync(marker), `the wrap reopened a remoteCwd break-out: ${argv[argv.length - 1]}`);
  } finally {
    cleanupMarker(marker);
  }
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

  // inner (pre-#2395 shape, now the -c argument): exec 'claude' '--session-id' 'sess-42'
  assert.equal(freshCmd, "exec \"$SHELL\" -l -i -c 'exec '\\''claude'\\'' '\\''--session-id'\\'' '\\''sess-42'\\'''");
  // inner (pre-#2395 shape, now the -c argument): exec 'claude' '--resume' 'sess-42'
  assert.equal(resumeCmd, "exec \"$SHELL\" -l -i -c 'exec '\\''claude'\\'' '\\''--resume'\\'' '\\''sess-42'\\'''");
  assert.notEqual(freshCmd, resumeCmd);
  // Everything else about the argv (options, destination, --) is unchanged.
  assert.deepEqual(freshArgv.slice(0, -1), resumeArgv.slice(0, -1));
});

test("sshResumeArgv: throws when there is no remote command to rewrite (login-shell panes never resume)", () => {
  const params: SshCommandParams = { destination: "user@host", remoteShell: "posix" };
  assert.throws(() => sshResumeArgv(FAKE_SSH, params, "sess-1"));
});
