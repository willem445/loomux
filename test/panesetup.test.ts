// Unit tests for the pure pane-setup core (#194): the kind → result matrix and
// the validation rules that back the welcome/pane-setup screen. Run with
// `npm test`. DOM-free — the form's async side effects (probe, worktree
// creation, autopilot flags) are validated by hand, not here.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  planPaneSetup,
  pathTail,
  worktreeNameFor,
  SubmitLatch,
  shellKindOptions,
  resolveShellKind,
  isContentKind,
  sshDiscardedFieldError,
  sshLaunchArgv,
  sshLaunchParams,
  sshMintsSessionId,
  sshOrchestrationRefusal,
  sshReconnectArgv,
  withSubmitLatch,
  sshRemoteCliWarning,
  sshRemoteCwdWarning,
  type PaneSetupInput,
} from "../src/panesetup.ts";
import type { SshProfile } from "../src/sshprofile.ts";

/** A fully-populated input; each test overrides only the fields it exercises. */
function input(over: Partial<PaneSetupInput>): PaneSetupInput {
  return {
    kind: "agent",
    agentId: "claude",
    isCustom: false,
    builtinCommand: "claude",
    customCommand: "",
    count: 1,
    repo: "",
    worktree: "",
    name: "",
    autopilot: true,
    shellKind: "powershell",
    sshProfile: null,
    ...over,
  };
}

/** A saved SSH connection, as the launcher's fields describe one (#887 S3). */
function sshProfile(over: Partial<SshProfile> = {}): SshProfile {
  return {
    id: "p1",
    name: "build box",
    destination: "dev@build.example.net",
    port: null,
    identityFile: null,
    remoteCwd: null,
    defaultCli: null,
    remoteShell: "posix",
    keepaliveSeconds: null,
    extraArgs: [],
    ...over,
  };
}

// ---------- terminal ----------

test("terminal always validates; empty repo means home", () => {
  const res = planPaneSetup(input({ kind: "terminal", repo: "" }));
  assert.equal(res.ok, true);
  assert.ok(res.ok && res.plan.kind === "terminal");
  if (res.ok && res.plan.kind === "terminal") {
    assert.equal(res.plan.cwd, null);
    assert.equal(res.plan.shellKind, "powershell");
    assert.equal(res.plan.name, "terminal");
  }
});

test("terminal carries the chosen shell kind through", () => {
  for (const shellKind of ["powershell", "gitbash", "cmd"] as const) {
    const res = planPaneSetup(input({ kind: "terminal", shellKind }));
    assert.ok(res.ok && res.plan.kind === "terminal" && res.plan.shellKind === shellKind);
  }
});

test("terminal cwd + default name come from the repo path tail", () => {
  const res = planPaneSetup(input({ kind: "terminal", repo: "  C:\\Projects\\loomux\\  " }));
  assert.ok(res.ok && res.plan.kind === "terminal");
  if (res.ok && res.plan.kind === "terminal") {
    // Whitespace is trimmed (the raw path is otherwise passed through as the
    // shell cwd, exactly like the agent path — no slash normalization).
    assert.equal(res.plan.cwd, "C:\\Projects\\loomux\\");
    assert.equal(res.plan.name, "loomux");
  }
});

// ---------- shell kinds (#194 P2) ----------

test("PowerShell and cmd are always enabled; Git Bash follows discovery", () => {
  const withBash = shellKindOptions({ gitBashPath: "C:\\Program Files\\Git\\bin\\bash.exe" });
  const ps = withBash.find((o) => o.key === "powershell");
  const cmd = withBash.find((o) => o.key === "cmd");
  const bash = withBash.find((o) => o.key === "gitbash");
  assert.ok(ps?.enabled && cmd?.enabled && bash?.enabled);
  // No reason text on an enabled option.
  assert.equal(bash?.reason, "");
});

test("Git Bash is disabled with a reason when not installed", () => {
  const opts = shellKindOptions({ gitBashPath: null });
  const bash = opts.find((o) => o.key === "gitbash");
  assert.equal(bash?.enabled, false);
  assert.match(bash?.reason ?? "", /Git for Windows/);
  // The always-available kinds are unaffected.
  assert.ok(opts.find((o) => o.key === "powershell")?.enabled);
  assert.ok(opts.find((o) => o.key === "cmd")?.enabled);
});

test("resolveShellKind keeps an available kind but falls unavailable ones back to PowerShell", () => {
  const installed = { gitBashPath: "C:\\Git\\bin\\bash.exe" };
  const missing = { gitBashPath: null };
  // Available → unchanged.
  assert.equal(resolveShellKind("gitbash", installed), "gitbash");
  assert.equal(resolveShellKind("cmd", missing), "cmd");
  assert.equal(resolveShellKind("powershell", missing), "powershell");
  // Requested-but-unavailable Git Bash → PowerShell fallback.
  assert.equal(resolveShellKind("gitbash", missing), "powershell");
});

// ---------- orchestrator ----------

test("orchestrator requires a repository", () => {
  const res = planPaneSetup(input({ kind: "orchestrator", repo: "  " }));
  assert.equal(res.ok, false);
  assert.ok(!res.ok && res.focus === "repo");
});

test("orchestrator with a repo validates and trims", () => {
  const res = planPaneSetup(input({ kind: "orchestrator", repo: "  /repo/x  " }));
  assert.ok(res.ok && res.plan.kind === "orchestrator" && res.plan.repo === "/repo/x");
});

// ---------- files (#214) ----------

test("a file explorer needs a folder — it does NOT fall back to home like a terminal", () => {
  // A terminal with no repo opens in home, which is useful. A file tree over the
  // whole home directory is not, and a rootless files pane has no content at all —
  // so this is a hard error that bounces the user back to the field.
  const res = planPaneSetup(input({ kind: "files", repo: "  " }));
  assert.equal(res.ok, false);
  assert.ok(!res.ok && res.focus === "repo");
  assert.match(res.ok ? "" : res.error, /folder/i);
});

test("a file explorer plans a root + name, and nothing else — no command, no shell", () => {
  const res = planPaneSetup(input({ kind: "files", repo: "  C:/Projects/loomux  ", name: " code " }));
  assert.ok(res.ok);
  // Deep-equal, not a field spot-check: the whole point of this kind is that it
  // carries NO spawn inputs. An extra command/argv/shellKind sneaking into the plan
  // would mean something is about to start a process in a pane that must never have one.
  assert.deepEqual(res.plan, { kind: "files", root: "C:/Projects/loomux", name: "code" });
});

test("a file explorer's name defaults to the folder's own short name", () => {
  const res = planPaneSetup(input({ kind: "files", repo: "C:\\Projects\\loomux\\", name: "" }));
  assert.ok(res.ok && res.plan.kind === "files");
  assert.equal(res.plan.name, "loomux");
});

// ---------- editor + git (#217) ----------

test("an editor pane needs a folder, and a git pane needs a repository", () => {
  // Same rule as the files kind, same reason: "home" is not a project to edit, and it is
  // certainly not a repository. A content pane with no root has no content — so this
  // bounces the user back to the field instead of opening an empty pane.
  const editor = planPaneSetup(input({ kind: "editor", repo: "  " }));
  assert.ok(!editor.ok && editor.focus === "repo");
  assert.match(editor.ok ? "" : editor.error, /folder/i);

  const git = planPaneSetup(input({ kind: "git", repo: "" }));
  assert.ok(!git.ok && git.focus === "repo");
  assert.match(git.ok ? "" : git.error, /repositor/i);
});

test("an editor pane plans a root + name, and nothing else — no command, no shell", () => {
  // Deep-equal, not a spot-check: this kind carries NO spawn inputs, and a command/argv/
  // shellKind sneaking into the plan would mean something is about to start a process in
  // a pane that must never have one.
  const res = planPaneSetup(input({ kind: "editor", repo: "  C:/Projects/loomux  ", name: " code " }));
  assert.ok(res.ok);
  assert.deepEqual(res.plan, { kind: "editor", root: "C:/Projects/loomux", name: "code" });
});

test("a git pane plans a root + name, and nothing else", () => {
  const res = planPaneSetup(input({ kind: "git", repo: " /repo/x ", name: "  " }));
  assert.ok(res.ok);
  // `root`, not `repo`: a content pane has ONE input and every consumer (the pane, the
  // capture, the restore) treats it identically — a synonym here would buy nothing and
  // cost a special case. Whether /repo/x is REALLY a git work tree is I/O: the form asks
  // git (gitRepoRoot) before it fires, and this module doesn't pretend it can know.
  assert.deepEqual(res.plan, { kind: "git", root: "/repo/x", name: "x" });
});

// ---------- workflow (#222) ----------

test("a workflow pane needs a repository, and plans a root + name and nothing else", () => {
  // Same one rule as its three siblings, for the same reason: `.loomux/workflow.yml` is a
  // file IN a repo, so a rootless workflow pane has no workflow to show. What it must NOT
  // do is demand the FILE exist — a repo without one is the normal starting point, and the
  // pane offers to create it (see the launcher's probe, which stops at the directory).
  const missing = planPaneSetup(input({ kind: "workflow", repo: "  " }));
  assert.ok(!missing.ok && missing.focus === "repo");
  assert.match(missing.ok ? "" : missing.error, /repositor/i);

  const res = planPaneSetup(input({ kind: "workflow", repo: " C:/Projects/loomux ", name: " flow " }));
  assert.ok(res.ok);
  assert.deepEqual(res.plan, { kind: "workflow", root: "C:/Projects/loomux", name: "flow" });
});

test("every content kind is a content kind — the predicate the form hides fields off", () => {
  // The form hides its CLI / count / worktree / autopilot / shell fields off this ONE
  // predicate rather than listing the kinds at each site. A kind missing from it would
  // silently render an agent's fields on a pane that can never spawn a process.
  for (const kind of ["files", "editor", "git", "workflow"] as const) {
    assert.equal(isContentKind(kind), true, `${kind} must be a content kind`);
  }
  for (const kind of ["agent", "orchestrator", "terminal"] as const) {
    assert.equal(isContentKind(kind), false);
  }
});

test("both new kinds default their name to the root's own short name", () => {
  const editor = planPaneSetup(input({ kind: "editor", repo: "C:\\Projects\\loomux\\", name: "" }));
  assert.ok(editor.ok && editor.plan.kind === "editor");
  assert.equal(editor.plan.name, "loomux");

  const git = planPaneSetup(input({ kind: "git", repo: "C:\\Projects\\loomux\\", name: "" }));
  assert.ok(git.ok && git.plan.kind === "git");
  assert.equal(git.plan.name, "loomux");
});

test("isContentKind names exactly the PTY-less kinds — the ones that spawn nothing", () => {
  // The welcome form hides every CLI/shell/worktree field off this one predicate, and the
  // pane system keys "no PTY, ever" off the same idea. A kind added to one list and not
  // the other is how a content pane ends up being asked which shell it wants.
  assert.deepEqual(
    (["agent", "orchestrator", "terminal", "files", "editor", "git"] as const).filter(isContentKind),
    ["files", "editor", "git"]
  );
});

// ---------- ssh (#887 S3) ----------

test("an SSH pane needs a connection — a half-filled form is bounced, not launched", () => {
  const res = planPaneSetup(input({ kind: "ssh", sshProfile: null }));
  assert.equal(res.ok, false);
  // Focused at the SSH section, not at the repo field: an SSH pane has no local
  // repository, and sending the human to a hidden field is sending them nowhere.
  assert.ok(!res.ok && res.focus === "ssh");
  assert.match(res.ok ? "" : res.error, /connection/i);
});

test("an SSH pane plans the connection itself, plus a name — and no local cwd", () => {
  const profile = sshProfile();
  const res = planPaneSetup(input({ kind: "ssh", sshProfile: profile, name: " prod " }));
  assert.ok(res.ok);
  // Deep-equal, like the content kinds: an SSH pane's LOCAL cwd is deliberately
  // home, so a `cwd`/`repo` sneaking into the plan would be a local directory
  // nobody picked — feeding local chrome (git watch, the folder picker) that
  // cannot mean anything for a pane whose files are on another machine.
  assert.deepEqual(res.plan, { kind: "ssh", profile, name: "prod" });
});

test("an SSH pane's name defaults to the connection's own name, then its destination", () => {
  const named = planPaneSetup(input({ kind: "ssh", sshProfile: sshProfile(), name: "" }));
  assert.ok(named.ok && named.plan.kind === "ssh" && named.plan.name === "build box");
  // A connection whose name is only whitespace still has to produce a title.
  const blank = planPaneSetup(input({ kind: "ssh", sshProfile: sshProfile({ name: "  x  " }), name: "" }));
  assert.ok(blank.ok && blank.plan.kind === "ssh");
  assert.equal(blank.ok && blank.plan.kind === "ssh" ? blank.plan.name : "", "x");
});

test("the SSH kind is not a content kind — it spawns a process like a terminal", () => {
  // The form hides its CLI/shell/worktree fields off `isContentKind`. An SSH pane
  // is PTY-backed (a local ssh client IS its child process), so classifying it as
  // content would hide the wrong fields and, worse, imply a pane with no process.
  assert.equal(isContentKind("ssh"), false);
});

// --- the launch-seam destination guard: the belt on S1's profile-level refusal ---

test("an SSH destination ssh would read as an OPTION is refused at the launch seam", () => {
  // The case this exists for: `-oProxyCommand=<cmd>` makes ssh run <cmd> on the
  // LOCAL machine. The profile store refuses such a destination on the way to and
  // from disk — but a connection launched from this form may never have been near
  // the disk (the inline editor builds one out of text typed seconds ago), so the
  // refusal has to exist HERE too, before anything spawns.
  for (const destination of ["-oProxyCommand=calc.exe", "user@-oProxyCommand=calc.exe"]) {
    const res = planPaneSetup(input({ kind: "ssh", sshProfile: sshProfile({ destination }) }));
    assert.equal(res.ok, false, `${destination} must not launch`);
    assert.ok(!res.ok && res.focus === "ssh");
    // Specific, not the generic "pick a connection" — the human has to be able to
    // tell which field is wrong and why.
    assert.match(res.ok ? "" : res.error, /leading "-"|isn't a destination/i);
    assert.match(res.ok ? "" : res.error, /ProxyCommand/);
  }
});

test("…and an ordinary destination still launches — the guard must not refuse the real case", () => {
  // Without this, "refuse every destination" would pass the test above. All three
  // shapes S1 documents: user@host, a bare host, and an ssh_config alias.
  for (const destination of ["dev@build.example.net", "build.example.net", "buildbox"]) {
    const res = planPaneSetup(input({ kind: "ssh", sshProfile: sshProfile({ destination }) }));
    assert.ok(res.ok && res.plan.kind === "ssh", `${destination} must launch`);
    assert.equal(res.ok && res.plan.kind === "ssh" ? res.plan.profile.destination : "", destination);
  }
});

test("the planned connection is the NORMALIZED one — what launches is what gets saved", () => {
  // The form hands over raw field text; the plan runs it through the profile
  // store's own normalizer, so the connection a pane launches and the connection
  // written to sshprofiles.json are one object rather than two that agree by
  // coincidence.
  //
  // Witnessed on the normalizations that are SILENT BY DESIGN — whitespace, and
  // an unrecognized remote shell falling back to the default. The other half of
  // normalization (a value the human typed that the store would DISCARD: an
  // out-of-range port, a pasted key) deliberately no longer reaches this test,
  // because it is now a refusal rather than a quiet repair — see the
  // no-silent-data-loss tests below. Asserting the old drop-through here would
  // have been asserting the defect.
  const res = planPaneSetup(
    input({
      kind: "ssh",
      sshProfile: sshProfile({
        destination: "  dev@build.example.net  ",
        remoteCwd: "  /srv/app  ",
        name: "  build box  ",
        remoteShell: "fish" as never, // not a shell this schema knows
      }),
    })
  );
  assert.ok(res.ok && res.plan.kind === "ssh");
  if (res.ok && res.plan.kind === "ssh") {
    assert.equal(res.plan.profile.destination, "dev@build.example.net", "trimmed, not raw");
    assert.equal(res.plan.profile.remoteCwd, "/srv/app");
    assert.equal(res.plan.profile.name, "build box");
    assert.equal(res.plan.profile.remoteShell, "posix", "an unknown shell defaults, it doesn't launch");
  }
});

// --- composing a profile (S1) into the builder's parameters (S2) ---

test("a connection with no remote CLI composes a plain login shell", () => {
  const params = sshLaunchParams(sshProfile({ remoteCwd: "/srv/app" }), null);
  assert.equal(params.remoteCommand, undefined, "no CLI means no remote command at all");
  // …and no remote cwd either: with nothing to run, there is nothing to `cd`
  // before. ssh hands the human their own login shell, whose start directory is
  // the remote's business.
  assert.equal(params.remoteCwd, undefined);
  assert.equal(params.destination, "dev@build.example.net");
});

test("claude gets a loomux-minted session id on the remote command line", () => {
  // The one session-identity mechanism that survives the trip: it rides on the
  // command line, where ssh carries it verbatim. Everything else loomux uses is a
  // scan of a LOCAL store.
  const params = sshLaunchParams(sshProfile({ defaultCli: "claude" }), "sess-1");
  assert.deepEqual(params.remoteCommand, ["claude", "--session-id", "sess-1"]);
});

test("a non-claude remote CLI gets NO session flags, even when an id is handed over", () => {
  // The failure this prevents: `--session-id` is not copilot's flag, so a pane
  // that "helpfully" passed it would die on its first line instead of starting.
  const params = sshLaunchParams(sshProfile({ defaultCli: "copilot" }), "sess-1");
  assert.deepEqual(params.remoteCommand, ["copilot"]);
  assert.equal(sshMintsSessionId("copilot"), false);
  assert.equal(sshMintsSessionId("claude"), true);
  assert.equal(sshMintsSessionId(null), false);
});

test("unset profile fields become ABSENT parameters, not zeroes", () => {
  // The profile spells "unset" as null; the builder spells it as an absent key,
  // and emits an option only for a key that is present. A null leaking through as
  // `port: null` would emit `-p null`; as `keepaliveSeconds: 0` it would emit the
  // ServerAlive option S1 refuses to let mean two things.
  const bare = sshLaunchParams(sshProfile({ defaultCli: "claude" }), null);
  assert.equal(bare.port, undefined);
  assert.equal(bare.identityFile, undefined);
  assert.equal(bare.keepaliveSeconds, undefined);
  assert.equal(bare.extraArgs, undefined);
  // …and a set one travels through unchanged.
  const full = sshLaunchParams(
    sshProfile({
      defaultCli: "claude",
      port: 2222,
      identityFile: "~/.ssh/id_ed25519",
      keepaliveSeconds: 30,
      extraArgs: ["-J", "jump.example.net"],
      remoteCwd: "/srv/app",
      remoteShell: "posix",
    }),
    null
  );
  assert.equal(full.port, 2222);
  assert.equal(full.identityFile, "~/.ssh/id_ed25519");
  assert.equal(full.keepaliveSeconds, 30);
  assert.deepEqual(full.extraArgs, ["-J", "jump.example.net"]);
  assert.equal(full.remoteCwd, "/srv/app");
});

test("the composed argv is a real ssh command line, end to end", () => {
  // The whole S1→S2 seam in one assertion, through the same `program` seam a
  // fake-ssh stub substitutes for hand-validation. The shape that matters: `-t`
  // (a remote TUI needs a pty), options BEFORE the destination, then `--` and
  // exactly ONE quoted remote-command string — so nothing in the remote command
  // can be re-read as another ssh option or another argv word.
  const argv = sshLaunchArgv(
    "C:\\Windows\\System32\\OpenSSH\\ssh.exe",
    sshProfile({ defaultCli: "claude", port: 2222, remoteCwd: "/srv/app" }),
    "sess-1"
  );
  assert.deepEqual(argv, [
    "C:\\Windows\\System32\\OpenSSH\\ssh.exe",
    "-t",
    "-p",
    "2222",
    "dev@build.example.net",
    "--",
    "cd '/srv/app' && exec 'claude' '--session-id' 'sess-1'",
  ]);
});

test("a login-shell connection composes an argv with no -t, no -- and no command", () => {
  const argv = sshLaunchArgv("ssh.exe", sshProfile(), null);
  assert.deepEqual(argv, ["ssh.exe", "dev@build.example.net"]);
});

test("an unknown remote CLI warns rather than refusing — it is a remote program, not ours", () => {
  // S1's stated contract: a profile naming a CLI this build doesn't know "is a
  // profile to warn about, not a profile to silently delete". What loomux's
  // catalog decides is which CLIs it can add flags for — not what the remote
  // machine is allowed to have installed.
  const known = ["claude", "copilot"];
  assert.match(sshRemoteCliWarning("aider", known), /aider/);
  assert.match(sshRemoteCliWarning("aider", known), /no session id/i);
  assert.equal(sshRemoteCliWarning("claude", known), "");
  assert.equal(sshRemoteCliWarning(null, known), "", "a login shell is not an unknown CLI");
  // And it stays a warning: the launch still plans.
  const res = planPaneSetup(input({ kind: "ssh", sshProfile: sshProfile({ defaultCli: "aider" }) }));
  assert.ok(res.ok);
});

// --- the #887/#888 boundary: SSH panes are never orchestration members ---

test("an SSH pane can never be given an orchestration identity — the #888 boundary", () => {
  // THE guardrail this slice owes. An SSH pane as a group member is not a feature
  // that degrades, it is one that breaks silently: worktrees are local dirs made
  // by local git, the MCP server is loopback-only, and the gh shim reaches only
  // locally-spawned children — so a remote worker's `gh pr merge` would face NO
  // merge gate at all. Refusal, not best-effort.
  //
  // Every marker refuses ON ITS OWN, so a spawn carrying half an identity is
  // refused too: a group with no role, a role with no group, a bare agent id.
  for (const orch of [
    { orchGroup: "g1" },
    { orchRole: "worker" },
    { orchAgent: "a1" },
    { orchGroup: "g1", orchRole: "worker", orchAgent: "a1" },
  ]) {
    const refusal = sshOrchestrationRefusal({ ssh: true, ...orch });
    assert.ok(refusal, `an ssh pane carrying ${JSON.stringify(orch)} must be refused`);
    assert.match(refusal ?? "", /#887|SSH panes are solo/);
  }
});

test("…and the guardrail refuses ONLY that combination", () => {
  // The other half: an ordinary orchestration pane (the overwhelmingly common
  // case) and an ordinary SSH pane both have to keep working. A guard that
  // refused either would have failed loudly in the app rather than quietly here,
  // but it would also have made the test above vacuous.
  assert.equal(sshOrchestrationRefusal({ ssh: false, orchGroup: "g1", orchRole: "worker" }), null);
  assert.equal(sshOrchestrationRefusal({ ssh: true }), null);
  assert.equal(sshOrchestrationRefusal({ ssh: true, orchGroup: null, orchRole: null, orchAgent: null }), null);
  assert.equal(sshOrchestrationRefusal({ ssh: false }), null);
  // …including when the two sides are read together: an orchestration pane
  // relaunched as an orchestration pane is still perfectly ordinary.
  assert.equal(sshOrchestrationRefusal({ orchGroup: "g1" }, { orchGroup: "g1", orchRole: "worker" }), null);
});

test("the guardrail reads BOTH sides by the same rule — either signal, from either side", () => {
  // PR #921 review (rev-441): the guard used to read ssh-ness from the options
  // OR the pane, but the orchestration identity from the options ALONE. That
  // asymmetry is a hole the width of one relaunch — an already-orchestrated pane
  // relaunched with `ssh` in its options carries its group on the PANE and
  // nothing in the options, so an opts-only read of the identity waves it through
  // and starts an ssh client inside a live group member.
  //
  // All four crossings of {which side says ssh} × {which side says orchestrated}
  // must refuse, or the rule isn't one rule.
  const cases: [string, Parameters<typeof sshOrchestrationRefusal>][] = [
    ["opts ssh + opts identity", [{ ssh: true, orchGroup: "g1" }, {}]],
    ["opts ssh + PANE identity", [{ ssh: true }, { orchGroup: "g1" }]],
    ["pane ssh + opts identity", [{ orchGroup: "g1" }, { ssh: true }]],
    ["pane ssh + PANE identity", [{}, { ssh: true, orchGroup: "g1" }]],
  ];
  for (const [label, args] of cases) {
    assert.ok(sshOrchestrationRefusal(...args), `${label} must be refused`);
  }
  // And per-field, not just for `orchGroup`: a role or a bare agent id on the
  // pane is as much an orchestration identity as a group is.
  assert.ok(sshOrchestrationRefusal({ ssh: true }, { orchRole: "worker" }));
  assert.ok(sshOrchestrationRefusal({ ssh: true }, { orchAgent: "a1" }));
});

// --- no silent data loss: a typed value is honoured or refused, never dropped ---

test("an out-of-range port is REFUSED, not silently dropped on the way to ssh", () => {
  // The defect this pins (PR #921 review, rev-441): normalization is right to
  // drop a port outside TCP's range, but dropping it quietly meant a human typed
  // 99999, hit Create, connected on 22, and was never told. The refusal names the
  // real bound rather than a second copy of it.
  const res = planPaneSetup(input({ kind: "ssh", sshProfile: sshProfile({ port: 99_999 }) }));
  assert.equal(res.ok, false);
  assert.ok(!res.ok && res.focus === "ssh");
  assert.match(res.ok ? "" : res.error, /port must be a whole number between 1 and 65535/i);
  // A fractional port is the same defect wearing different clothes.
  const fractional = planPaneSetup(input({ kind: "ssh", sshProfile: sshProfile({ port: 22.5 }) }));
  assert.equal(fractional.ok, false);
  // …while a real port, and no port at all, both still launch: the refusal must
  // not have become "no port is ever acceptable".
  const ok = planPaneSetup(input({ kind: "ssh", sshProfile: sshProfile({ port: 2222 }) }));
  assert.ok(ok.ok && ok.plan.kind === "ssh" && ok.plan.profile.port === 2222);
  const unset = planPaneSetup(input({ kind: "ssh", sshProfile: sshProfile({ port: null }) }));
  assert.ok(unset.ok && unset.plan.kind === "ssh" && unset.plan.profile.port === null);
});

test("an out-of-range keepalive and a pasted KEY are refused for the same reason", () => {
  // Same rule, the other two fields normalization can drop. The identity-file one
  // matters most: silently dropping it means connecting with no `-i` at all, and
  // the human sees an authentication failure with no visible cause.
  const keepalive = planPaneSetup(
    input({ kind: "ssh", sshProfile: sshProfile({ keepaliveSeconds: 0 }) })
  );
  assert.equal(keepalive.ok, false);
  assert.match(keepalive.ok ? "" : keepalive.error, /keepalive/i);

  const pasted = planPaneSetup(
    input({
      kind: "ssh",
      sshProfile: sshProfile({
        identityFile: "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEA\n-----END…",
      }),
    })
  );
  assert.equal(pasted.ok, false);
  assert.match(pasted.ok ? "" : pasted.error, /must be a PATH/i);
  // The real case still passes — a path is a path.
  const path = planPaneSetup(
    input({ kind: "ssh", sshProfile: sshProfile({ identityFile: "~/.ssh/id_ed25519", keepaliveSeconds: 30 }) })
  );
  assert.ok(path.ok && path.plan.kind === "ssh");
  assert.equal(path.ok && path.plan.kind === "ssh" ? path.plan.profile.identityFile : "", "~/.ssh/id_ed25519");
});

test("a saved connection off disk is never bounced by that refusal", () => {
  // The rule only ever fires on values a human just typed. Anything loaded from
  // sshprofiles.json was normalized on the way in, so what was typed and what
  // survived are the same object and nothing is refused — otherwise this would
  // have turned an old saved connection into a pane that refuses to open.
  const fromDisk = sshProfile({ port: 2222, keepaliveSeconds: 30, identityFile: "~/.ssh/id_ed25519" });
  assert.equal(sshDiscardedFieldError(fromDisk, fromDisk), "");
});

test("a remote folder with no remote CLI warns — the value is kept, not dropped in silence", () => {
  // `remoteCwd` is a prefix to the REMOTE COMMAND, so with no CLI there is
  // nothing to prefix and it cannot apply (S2's builder has always worked this
  // way). Honouring it would mean guessing the remote's login shell — the exact
  // guess `remoteShell` exists to refuse — and refusing to save it would throw
  // away a setting that becomes correct the moment a CLI is picked. So: warn.
  assert.match(sshRemoteCwdWarning(null, "/srv/app"), /only when a remote CLI is set/i);
  assert.match(sshRemoteCwdWarning(null, "/srv/app"), /stays saved/i);
  // Silent in every case where it does apply, or where there is nothing to say.
  assert.equal(sshRemoteCwdWarning("claude", "/srv/app"), "");
  assert.equal(sshRemoteCwdWarning(null, null), "");
  // And the warning is exactly that — the launch still plans, with the folder
  // preserved on the connection that gets saved.
  const res = planPaneSetup(
    input({ kind: "ssh", sshProfile: sshProfile({ remoteCwd: "/srv/app", defaultCli: null }) })
  );
  assert.ok(res.ok && res.plan.kind === "ssh");
  assert.equal(res.ok && res.plan.kind === "ssh" ? res.plan.profile.remoteCwd : "", "/srv/app");
});

// ---------- SubmitLatch's second consumer: the app-quit confirm (#219) ----------

test("the quit confirm reuses the latch: a second ✕ while the dialog is up is refused", () => {
  // Same async-reentrancy shape as the welcome form's submit (#194 P1) and Pane.
  // requestClose (#217): the guard awaits a modal, and meanwhile a second ✕ / Alt+F4 /
  // impatient double-click fires the close request again. Without the latch that stacks a
  // SECOND quit dialog whose answer races the first one's. The in-flight ask owns the
  // decision, so the duplicate is refused and the window simply stays.
  const latch = new SubmitLatch();
  assert.equal(latch.begin(), true, "the ✕ that opened the dialog");
  assert.equal(latch.begin(), false, "a second ✕ while it is up — no second dialog");
  assert.equal(latch.begin(), false, "…nor a third");

  // Cancel → the app stays, and a LATER ✕ must ask again (release, not finish).
  latch.release();
  assert.equal(latch.begin(), true);

  // "Quit anyway" → finish: the window is going away, so nothing further is admitted even
  // if a late close event lands while it does.
  latch.finish();
  assert.equal(latch.begin(), false);
});

// ---------- agent ----------

test("agent with a built-in CLI validates", () => {
  const res = planPaneSetup(input({ kind: "agent", builtinCommand: "claude" }));
  assert.ok(res.ok && res.plan.kind === "agent");
  if (res.ok && res.plan.kind === "agent") {
    assert.equal(res.plan.command, "claude");
    assert.equal(res.plan.count, 1);
    assert.equal(res.plan.baseName, "claude"); // blank name → command
  }
});

test("custom agent needs a command", () => {
  const res = planPaneSetup(input({ isCustom: true, builtinCommand: "", customCommand: "   " }));
  assert.equal(res.ok, false);
  assert.ok(!res.ok && res.focus === "custom");
});

test("custom agent uses the custom command, not the built-in", () => {
  const res = planPaneSetup(
    input({ isCustom: true, builtinCommand: "claude", customCommand: " aider --model sonnet " })
  );
  assert.ok(res.ok && res.plan.kind === "agent");
  if (res.ok && res.plan.kind === "agent") {
    assert.equal(res.plan.command, "aider --model sonnet");
    assert.equal(res.plan.isCustom, true);
  }
});

test("a worktree without a repo is rejected", () => {
  const res = planPaneSetup(input({ worktree: "fix-auth", repo: "" }));
  assert.equal(res.ok, false);
  assert.ok(!res.ok && res.focus === "repo");
});

test("a worktree with a repo is allowed", () => {
  const res = planPaneSetup(input({ worktree: "fix-auth", repo: "/repo" }));
  assert.ok(res.ok && res.plan.kind === "agent" && res.plan.worktree === "fix-auth");
});

test("pane count is clamped into [1, 8]", () => {
  assert.equal(pick(planPaneSetup(input({ count: 0 }))), 1);
  assert.equal(pick(planPaneSetup(input({ count: 3 }))), 3);
  assert.equal(pick(planPaneSetup(input({ count: 99 }))), 8);
  assert.equal(pick(planPaneSetup(input({ count: 2.9 }))), 2); // truncated
  assert.equal(pick(planPaneSetup(input({ count: NaN }))), 1);
});

/** Extract the clamped count from an agent result (test helper). */
function pick(res: ReturnType<typeof planPaneSetup>): number {
  assert.ok(res.ok && res.plan.kind === "agent");
  return res.ok && res.plan.kind === "agent" ? res.plan.count : -1;
}

test("a typed name overrides the command default", () => {
  const res = planPaneSetup(input({ name: "  my pane  " }));
  assert.ok(res.ok && res.plan.kind === "agent" && res.plan.baseName === "my pane");
});

test("autopilot flag rides through to the plan", () => {
  const on = planPaneSetup(input({ autopilot: true }));
  const off = planPaneSetup(input({ autopilot: false }));
  assert.ok(on.ok && on.plan.kind === "agent" && on.plan.autopilot === true);
  assert.ok(off.ok && off.plan.kind === "agent" && off.plan.autopilot === false);
});

// ---------- pure helpers ----------

test("pathTail returns the last non-empty segment", () => {
  assert.equal(pathTail("C:\\a\\b\\c"), "c");
  assert.equal(pathTail("/x/y/z/"), "z");
  assert.equal(pathTail(""), "");
  assert.equal(pathTail("solo"), "solo");
});

test("worktreeNameFor keeps a single name but fans out a fleet", () => {
  assert.equal(worktreeNameFor("fix-auth", 1, 1), "fix-auth");
  assert.equal(worktreeNameFor("fix-auth", 1, 3), "fix-auth-1");
  assert.equal(worktreeNameFor("fix-auth", 3, 3), "fix-auth-3");
});

// ---------- submit latch (rev-74 HIGH-1: no duplicate launches) ----------

test("SubmitLatch admits only the first of concurrent begins", () => {
  const latch = new SubmitLatch();
  assert.equal(latch.begin(), true); // first click enters
  assert.equal(latch.begin(), false); // double-click / Enter-repeat is rejected
  assert.equal(latch.begin(), false); // …and every further re-entry while in flight
});

test("SubmitLatch reopens after a validation error so the user can retry", () => {
  const latch = new SubmitLatch();
  assert.equal(latch.begin(), true);
  latch.release(); // planPaneSetup returned an error; allow a fixed retry
  assert.equal(latch.settled, false);
  assert.equal(latch.begin(), true); // retry admitted
});

test("SubmitLatch is one-shot once a submit finishes", () => {
  const latch = new SubmitLatch();
  assert.equal(latch.begin(), true);
  latch.finish(); // onSubmit fired — the pane is being converted/retired
  assert.equal(latch.settled, true);
  assert.equal(latch.begin(), false); // a late re-entry must never fire again
  latch.release(); // even an errant release can't reopen a finished latch
  assert.equal(latch.begin(), false);
});

// --- #887 S4: the reconnect a restored (or disconnected) SSH pane replays ---

/** A mint that is obviously a mint — so a test can tell "loomux minted a new
 *  remote session" from "loomux resumed the recorded one" by reading the argv. */
const MINT = () => "minted-id";

test("a recorded claude session reconnects in RESUME form, never a second --session-id", () => {
  // The whole point of recording the id: the remote conversation comes back
  // instead of a fresh one starting beside it. `--session-id` CREATES a session,
  // so replaying it against one the earlier run already made is an error, not a
  // reconnect — the rewrite is what keeps that from being the reconnect path.
  const { argv, sessionId, mode } = sshReconnectArgv(
    "C:/Windows/System32/OpenSSH/ssh.exe",
    sshProfile({ defaultCli: "claude", remoteCwd: "/srv/app" }),
    "remote-sess-1",
    MINT
  );
  assert.equal(mode, "resume");
  assert.equal(sessionId, "remote-sess-1", "the pane keeps the SAME identity across a reconnect");
  const remote = argv[argv.length - 1];
  assert.match(remote, /'claude' '--resume' 'remote-sess-1'/, `resume form expected, got: ${remote}`);
  assert.ok(!remote.includes("--session-id"), `no create flag may survive: ${remote}`);
  assert.ok(!remote.includes("minted-id"), "a recorded session is resumed, not replaced by a fresh mint");
  // …and it is still a full, ordinary ssh command line: the connection's own
  // settings are re-derived from the profile, not carried over from anywhere.
  assert.match(remote, /cd '\/srv\/app'/);
});

test("no recorded session on a claude profile MINTS one, so the reconnect is resumable next time", () => {
  // The parallel of `agentFreshCommand`'s local rule: a fresh start still gets an
  // identity, or the pane would be unresumable forever after one lost id.
  const { argv, sessionId, mode } = sshReconnectArgv(
    "ssh",
    sshProfile({ defaultCli: "claude" }),
    null,
    MINT
  );
  assert.equal(mode, "fresh");
  assert.equal(sessionId, "minted-id");
  const remote = argv[argv.length - 1];
  assert.match(remote, /'claude' '--session-id' 'minted-id'/, `fresh form expected, got: ${remote}`);
  assert.ok(!remote.includes("--resume"), `nothing to resume: ${remote}`);
});

test("a profile edited to a NON-minting CLI never receives the recorded claude id", () => {
  // The profile as it is NOW decides, not the record. A session id minted for
  // claude is meaningless to copilot — it names a conversation on the far host
  // that copilot cannot read — and `--session-id` is not even a flag it would
  // accept, so the pane would die on its first line.
  const { argv, sessionId, mode } = sshReconnectArgv(
    "ssh",
    sshProfile({ defaultCli: "copilot" }),
    "remote-sess-1",
    MINT
  );
  assert.equal(mode, "fresh");
  assert.equal(sessionId, null, "no id is recorded for a CLI whose identity loomux cannot mint");
  const remote = argv[argv.length - 1];
  assert.ok(!remote.includes("remote-sess-1"), `the stale id must not reach copilot: ${remote}`);
  assert.ok(!remote.includes("--session-id") && !remote.includes("--resume"), remote);
  assert.match(remote, /exec 'copilot'/);
});

test("a profile edited down to a plain login shell reconnects as a login shell", () => {
  // No remote command at all — so no `-t`, no `--`, and certainly no session
  // flag. The recorded id is simply not applicable any more, and inventing a
  // remote command to hang it on would be the guess `remoteShell` exists to
  // refuse.
  const { argv, sessionId, mode } = sshReconnectArgv("ssh", sshProfile({ defaultCli: null }), "remote-sess-1", MINT);
  assert.equal(mode, "fresh");
  assert.equal(sessionId, null);
  assert.deepEqual(argv, ["ssh", "dev@build.example.net"]);
});

test("a reconnect re-derives every connection setting from the profile AS IT IS NOW", () => {
  // S1's `SshProfile.id` contract, made observable: a pane records the
  // connection, not its contents, so an edit between boots is what reconnects.
  // A captured argv could never do this — it would still be dialling last week's
  // port.
  const { argv } = sshReconnectArgv(
    "ssh",
    sshProfile({ defaultCli: "claude", port: 2222, identityFile: "C:/keys/id_ed25519", keepaliveSeconds: 30 }),
    "remote-sess-1",
    MINT
  );
  assert.deepEqual(argv.slice(0, 9), [
    "ssh",
    "-t",
    "-p",
    "2222",
    "-i",
    "C:/keys/id_ed25519",
    "-o",
    "ServerAliveInterval=30",
    "dev@build.example.net",
  ]);
});

test("the reconnect surfaces a cmd.exe-unquotable value as a throw, exactly as a fresh launch does", () => {
  // Same refusal, same place (sshcommand.ts), so a reconnect can never be the
  // path that silently ships a truncated `/C` command line the launch form would
  // have refused.
  assert.throws(
    () =>
      sshReconnectArgv(
        "ssh",
        // `\\` is a real backslash and `\n` a real newline: a Windows-looking
        // remote folder with a line break smuggled into it. (Review NB5: written
        // as `"C:\srv\n…"` this was `C:srv` + a newline — the backslash silently
        // dropped by JS's unknown-escape rule — so it read as covering a
        // backslash case it never exercised.) The NEWLINE is what the refusal is
        // about: `cmd.exe /C` reads only the first line and drops the rest.
        sshProfile({ defaultCli: "claude", remoteShell: "cmd", remoteCwd: "C:\\srv\nrm -rf" }),
        null,
        MINT
      ),
    /newline/
  );
});

test("the fresh ESCAPE (a recorded session that can never be resumed) forces a new remote session", () => {
  // PR #926 review NB3. A remote conversation can be gone while the id naming it
  // is still recorded here — deleted on the far host, a cleared `~/.claude`, a
  // rebuilt box — and then `--resume` fails every single time, so plain Reconnect
  // loops. The card's escape hatch is this exact call with the recorded id
  // withheld: same profile, same connection settings, a brand-new session.
  const recorded = "remote-sess-1";
  const profile = sshProfile({ defaultCli: "claude", remoteCwd: "/srv/app" });
  const escape = sshReconnectArgv("ssh", profile, null, MINT);
  assert.equal(escape.mode, "fresh");
  assert.equal(escape.sessionId, "minted-id", "a NEW id, so the new conversation is itself resumable");
  const remote = escape.argv[escape.argv.length - 1];
  assert.ok(!remote.includes(recorded), `the unresumable id must not come back: ${remote}`);
  assert.match(remote, /'claude' '--session-id' 'minted-id'/);
  // …and it is the SAME connection otherwise — the escape changes the session,
  // never where or how the pane connects.
  const normal = sshReconnectArgv("ssh", profile, recorded, MINT);
  assert.deepEqual(
    escape.argv.slice(0, -1),
    normal.argv.slice(0, -1),
    "everything before the remote command is identical"
  );
});

// --- #887 S4 / PR #926 round 2 B1: a reconnect is SINGLE-FLIGHT ---

test("two near-simultaneous reconnects spawn exactly ONE ssh client", async () => {
  // The defect this pins, concretely: the SSH reconnect card has two actions
  // (Reconnect, Reconnect fresh). With them ungated, a click on each — the
  // expected gesture, since the escape exists for someone who has just watched a
  // resume fail — started TWO ssh clients and two remote agent CLIs against one
  // pane. The pane binds whichever spawn resolves last; the loser's output routes
  // nowhere, its exit lands unclaimed, and a kill goes through the pane's ptyId,
  // so nothing in loomux can stop it — a remote agent left running, unaccountable,
  // on someone else's machine, in a feature whose restore policy is argued from
  // not spending remote credits unattended.
  let spawns = 0;
  let finish!: () => void;
  const inFlight = new Promise<void>((resolve) => (finish = resolve));
  const latch = new SubmitLatch();
  const reconnect = (): Promise<{ ok: boolean; message?: string }> =>
    withSubmitLatch(
      latch,
      () => ({ ok: false, message: "busy" }),
      async () => {
        spawns++;
        await inFlight; // stand in for discoverSsh + loadSshProfiles + spawnPty
        return { ok: true };
      }
    );

  const first = reconnect(); // still in flight — nothing has resolved it
  const second = reconnect(); // the second button, ~300ms later
  // Checked BEFORE anything is awaited, and deliberately so: an async function
  // runs synchronously up to its first `await`, so the spawn count is already
  // final here. It also keeps this test from DEADLOCKING under the mutation it
  // exists to catch — awaiting `second` first would hang forever once the gate
  // is gone (an ungated second call waits on `inFlight`, which is released
  // below), and a hang is not a red.
  assert.equal(spawns, 1, "exactly one spawn while an attempt is in flight");
  finish();
  assert.deepEqual(await second, { ok: false, message: "busy" }, "the second click is refused, not queued");
  assert.deepEqual(await first, { ok: true }, "the first attempt is untouched by the refusal");
  // …and the latch REOPENS: a reconnect that failed must stay retryable — the
  // card's whole purpose is the retry — so this is never one-shot.
  assert.deepEqual(await reconnect(), { ok: true });
  assert.equal(spawns, 2, "a later attempt runs once the first has settled");
});

test("a reconnect that THROWS still releases the latch (a failure must stay retryable)", () => {
  // The `finally` in `withSubmitLatch`, pinned: without it, one failed reconnect
  // would wedge the card forever — every later click answered "already
  // reconnecting" while nothing was, which is worse than the double-spawn it
  // exists to prevent, because there is no way out of it at all.
  const latch = new SubmitLatch();
  let attempts = 0;
  const boom = (): Promise<{ ok: boolean }> =>
    withSubmitLatch(
      latch,
      () => ({ ok: false }),
      async () => {
        attempts++;
        throw new Error("spawn failed");
      }
    );
  return boom().then(
    () => assert.fail("the rejection must propagate to the caller"),
    async (err) => {
      assert.match(String(err), /spawn failed/);
      await boom().then(
        () => assert.fail("still expected to reject"),
        () => assert.equal(attempts, 2, "the second attempt ran — the latch reopened")
      );
    }
  );
});
