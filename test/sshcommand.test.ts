// Unit tests for the pure ssh argv builder (#887 plan, slice S2). Run with
// `npm test` (Node's built-in test runner strips TS types natively — mirrors
// test/layout.test.ts). No sshd, no network, no process spawn: every
// assertion is on the plain string[] the builder returns.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildSshArgv,
  claudeSessionArgs,
  rewriteClaudeCommandForResume,
  sshResumeArgv,
  type SshCommandParams,
} from "../src/sshcommand.ts";

const FAKE_SSH = "./fake-ssh.sh"; // the test/hand-validation program-injection seam

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

test("remote command with cwd: windows cd /d then bare command", () => {
  const params: SshCommandParams = {
    destination: "user@host",
    remoteShell: "windows",
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

// ---------- adversarial quoting: windows ----------

test("windows quoting: embedded double quote is doubled, not left dangling", () => {
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "windows",
    remoteCommand: ["claude", 'arg-with-"quote'],
  });
  const cmdString = argv[argv.length - 1];
  assert.equal(cmdString, '"claude" "arg-with-""quote"');
  assert.equal(argv.length, 5);
});

test("windows quoting: hostile token with metacharacters stays inside one quoted run", () => {
  const hostile = 'x" & del /f /q C:\\ & "y';
  const argv = buildSshArgv(FAKE_SSH, {
    destination: "h",
    remoteShell: "windows",
    remoteCommand: ["claude", hostile],
  });
  const cmdString = argv[argv.length - 1];
  assert.equal(cmdString, '"claude" "x"" & del /f /q C:\\ & ""y"');
  // [program, -t, destination, --, cmdString] regardless of embedded
  // metacharacters — one trailing argv element, destination untouched.
  assert.equal(argv.length, 5);
  assert.equal(argv[2], "h");
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
