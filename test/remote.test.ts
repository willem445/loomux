// Unit tests for remote SSH+tmux argv assembly (#122, Part A). Pure helpers
// only — the launcher DOM wiring is validated by hand. Run with `npm test`.
//
// The point of these tests is the injection surface: user-supplied host,
// session name, and remote directory all flow into an ssh argv and a remote
// shell command. Like git.rs/gh.rs guard `-`-prefixed args, we prove a hostile
// value can't add ssh options or break out of the remote shell word.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildRemoteInvocation,
  validateHost,
  sanitizeSessionName,
  suggestSessionName,
  validateRemoteCwd,
  mergeRemote,
  type RemoteTarget,
} from "../src/remote.ts";

test("buildRemoteInvocation assembles the ssh + tmux new -A argv", () => {
  const { argv, command } = buildRemoteInvocation({ host: "me@box", session: "work" });
  assert.deepEqual(argv, ["ssh", "-t", "me@box", "tmux new -A -s work"]);
  assert.equal(command, 'ssh -t me@box "tmux new -A -s work"');
});

test("reattach is idempotent: same host+session yields identical argv", () => {
  const a = buildRemoteInvocation({ host: "me@box", session: "work" });
  // A later reopen (even with a different cwd, which only matters on create)
  // produces the same attach command — that is `tmux new -A`'s whole point.
  const b = buildRemoteInvocation({ host: "me@box", session: "work", cwd: "/tmp" });
  assert.deepEqual(a.argv.slice(0, 3), b.argv.slice(0, 3));
  assert.match(a.argv[3], /tmux new -A -s work$/);
  assert.match(b.argv[3], /tmux new -A -s work -c '\/tmp'$/);
});

test("optional remote cwd is single-quoted into -c", () => {
  const { argv } = buildRemoteInvocation({ host: "h", session: "s", cwd: "/srv/app" });
  assert.equal(argv[3], "tmux new -A -s s -c '/srv/app'");
});

test("validateHost accepts user@host and bare host, trimming", () => {
  assert.equal(validateHost("  user@host.example  "), "user@host.example");
  assert.equal(validateHost("server1"), "server1");
});

test("validateHost rejects an option-injecting or malformed host", () => {
  // A leading '-' would be read by ssh as an option (e.g. -oProxyCommand=...).
  assert.throws(() => validateHost("-oProxyCommand=calc"), /invalid/);
  assert.throws(() => validateHost("host with space"), /invalid/);
  assert.throws(() => validateHost("a@b@c"), /more than one/);
  assert.throws(() => validateHost(""), /Enter a host/);
});

test("sanitizeSessionName strips anything a tmux/shell word can't hold", () => {
  assert.equal(sanitizeSessionName("my session"), "my-session");
  // A shell-injection attempt collapses to inert dashes, never survives.
  assert.equal(sanitizeSessionName("x; rm -rf ~"), "x-rm-rf");
  assert.equal(sanitizeSessionName("$(evil)"), "evil");
  assert.equal(sanitizeSessionName("  --weird--  "), "weird");
  assert.equal(sanitizeSessionName(""), "loomux");
  assert.equal(sanitizeSessionName("!@#$%"), "loomux");
});

test("a hostile session name cannot escape the remote command word", () => {
  const { argv } = buildRemoteInvocation({ host: "h", session: "a; reboot #" });
  // The sanitized name is a single safe word — no `;`, no space injection.
  assert.equal(argv[3], "tmux new -A -s a-reboot");
});

test("suggestSessionName derives from cwd basename, then host label", () => {
  assert.equal(suggestSessionName("me@box", "/home/me/my-proj"), "my-proj");
  assert.equal(suggestSessionName("me@build.example.com"), "build");
  assert.equal(suggestSessionName("me@box", "/home/me/proj/"), "proj");
});

test("validateRemoteCwd rejects shell-breaking characters, passes clean paths", () => {
  assert.equal(validateRemoteCwd("/srv/app"), "/srv/app");
  assert.equal(validateRemoteCwd("  "), undefined);
  assert.equal(validateRemoteCwd(undefined), undefined);
  for (const bad of ["/a'/b", "/a$(x)", "/a`x`", "/a\\b", '/a"b']) {
    assert.throws(() => validateRemoteCwd(bad), /aren't allowed/);
  }
});

test("a hostile cwd is rejected rather than injected", () => {
  assert.throws(
    () => buildRemoteInvocation({ host: "h", session: "s", cwd: "/tmp'; rm -rf ~ #" }),
    /aren't allowed/
  );
});

test("mergeRemote de-dupes by host+session and caps length", () => {
  const t = (host: string, session: string, cwd?: string): RemoteTarget => ({ host, session, cwd });
  let list: RemoteTarget[] = [];
  list = mergeRemote(list, t("box", "a"));
  list = mergeRemote(list, t("box", "b"));
  // Re-adding host+session "box/a" (new cwd) moves it to front, no duplicate.
  list = mergeRemote(list, t("box", "a", "/new"));
  assert.deepEqual(
    list.map((x) => `${x.host}/${x.session}`),
    ["box/a", "box/b"]
  );
  assert.equal(list[0].cwd, "/new");

  let capped: RemoteTarget[] = [];
  for (let i = 0; i < 12; i++) capped = mergeRemote(capped, t("box", `s${i}`), 8);
  assert.equal(capped.length, 8);
  assert.equal(capped[0].session, "s11"); // most recent first
});
