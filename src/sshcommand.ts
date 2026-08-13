// Pure, DOM-free ssh argv builder for SSH panes (#887 plan part 4/6-a, slice
// S2). No Tauri or DOM imports — unit-testable under `node --test` (mirrors
// spawnexpiry.ts / layout.ts). Independent of S1 (src/sshprofile.ts): this
// module takes flat primitive params, never the profile type, so S1 and S2
// can land from parallel worktrees with no shared surface.
//
// The pane spawns via the existing spawnPty argv path with zero backend
// changes (ssh.exe rides pty.rs's direct-native-exe spawn) — this module's
// only job is producing that argv. `program` is a plain parameter rather
// than a hardcoded "ssh" literal so a test (or hand-validation) can
// substitute a local fake-ssh stub, the same seam src-tauri/tests/ uses for
// fake agent CLIs.

/** The remote's default shell, as the user DECLARES it — never detected.
 *  Detecting a remote shell from here would mean probing the remote host
 *  before we can even build the connect command; the field exists so the
 *  quoting choice is the user's explicit, documented call.
 *
 *  `"cmd"` means the remote sshd's `DefaultShell` is `cmd.exe` — the OpenSSH-
 *  for-Windows default. A remote configured with a PowerShell `DefaultShell`
 *  is NOT covered by either scheme and is unsupported in v1: PowerShell's
 *  quoting rules differ from cmd.exe's (`$(...)` expands even inside double
 *  quotes, which is strictly worse than any gap documented below), so
 *  declaring `"cmd"` against a PowerShell remote is a user error, not
 *  something this module can quote its way around. `buildSshArgv` refuses
 *  any `remoteShell` value outside `"posix"` / `"cmd"` rather than guessing. */
export type RemoteShell = "posix" | "cmd";

export interface SshCommandParams {
  /** `user@host`, or an ssh-config `Host` alias — opaque to this module. */
  destination: string;
  port?: number;
  /** Path to a private key, passed as `-i` (a path, never key material). */
  identityFile?: string;
  /** Emits `ServerAliveInterval=<n>` only when set. Unset means nothing is
   *  passed, so the user's own ssh config always wins — same posture as the
   *  rest of the profile (no credential handling, no silent overriding). */
  keepaliveSeconds?: number;
  /** Extra raw ssh option tokens, passed through verbatim and in order. */
  extraArgs?: string[];
  remoteShell: RemoteShell;
  /** Remote working directory to `cd` into before the remote command. */
  remoteCwd?: string;
  /** argv of the program to run on the remote host. Absent/empty means a
   *  plain login shell: ssh allocates its own tty, no `-t`, no trailing
   *  `--`/command. */
  remoteCommand?: string[];
}

/** Escapes one token for a POSIX remote shell: wrap in single quotes, and
 *  end-quote/escaped-quote/re-open-quote any embedded single quote. This is
 *  the standard, provably-safe POSIX quoting scheme — nothing inside single
 *  quotes is special except `'` itself. */
function posixQuote(token: string): string {
  return `'${token.replace(/'/g, `'\\''`)}'`;
}

/** Escapes one token for a `cmd.exe` remote shell: wrap in double quotes,
 *  double any embedded double quote (the convention `cmd.exe`'s own quoted-
 *  run parsing honors — confirmed against a real `cmd.exe` in
 *  test/sshcommand.test.ts).
 *
 *  Refuses rather than silently mangling in two cases a real `cmd.exe` gets
 *  wrong no matter how the token is quoted:
 *  - a newline: `cmd.exe` only ever reads the first line of a `/C` command
 *    line, so a token carrying one doesn't get quoted wrong, it gets
 *    *truncated* — the rest of the command silently never runs.
 *  - a trailing backslash: the receiving program's own argv parser (the
 *    `CommandLineToArgvW` convention) treats a run of backslashes
 *    immediately before a quote as escaping that quote, so a token ending in
 *    `\` merges with whatever quoted text follows instead of closing.
 *
 *  Unlike POSIX single-quoting, this is still NOT a fully injection-proof
 *  scheme even after both of the above: `cmd.exe` expands `%VAR%` even
 *  inside double quotes, a known, unclosable `cmd.exe` hazard. That gap is
 *  why `remoteShell` is documented as a declaration, not a security
 *  boundary: the caller chose "cmd" and accepts cmd.exe's own quoting
 *  limits, the same way choosing "posix" accepts whatever the remote's
 *  actual shell turns out to be. */
function cmdQuote(token: string): string {
  if (token.includes("\n") || token.includes("\r")) {
    throw new Error(
      `cmd.exe quoting: refusing a remote-command token containing a newline — cmd.exe /C reads only ` +
        `the first line and silently drops the rest: ${JSON.stringify(token)}`,
    );
  }
  if (token.endsWith("\\")) {
    throw new Error(
      `cmd.exe quoting: refusing a token ending in '\\' — a trailing backslash immediately before the ` +
        `closing quote escapes it instead of ending the argument, merging it with the next token: ${JSON.stringify(token)}`,
    );
  }
  if (token === "") return '""';
  return `"${token.replace(/"/g, '""')}"`;
}

function buildPosixRemoteCommand(remoteCwd: string | undefined, remoteCommand: string[]): string {
  const cmd = remoteCommand.map(posixQuote).join(" ");
  return remoteCwd ? `cd ${posixQuote(remoteCwd)} && exec ${cmd}` : `exec ${cmd}`;
}

/** Builds the cmd.exe-targeted remote command string.
 *
 *  ALWAYS prefixes with `cd /d <quoted-cwd-or-".">` — even when the caller
 *  passed no `remoteCwd` — so the emitted string can never begin with a `"`
 *  character. That leading character matters because of how `cmd.exe`
 *  itself parses a `/C` (or `/K`) command line (`cmd /?`): unless the whole
 *  line is exactly two quote characters wrapping a bare executable name (it
 *  never is here — this scheme always emits at least four), old behavior
 *  applies: "if the first character is a quote character, strip the leading
 *  character and remove the last quote character on the command line". That
 *  strip inverts the quote state of the ENTIRE string — every run this
 *  scheme intended as quoted becomes unquoted, and `&`/`|`/`>` in a token
 *  parse as command separators regardless of how carefully that token was
 *  quoted. A string that does not start with `"` never triggers that strip,
 *  so `cmd.exe` tokenizes it normally and the per-token quoting above holds.
 *  Confirmed against a real `cmd.exe` in test/sshcommand.test.ts (this was
 *  the mechanism of the injection this module used to be vulnerable to). */
function buildCmdRemoteCommand(remoteCwd: string | undefined, remoteCommand: string[]): string {
  const cwd = remoteCwd ?? ".";
  const cmd = remoteCommand.map(cmdQuote).join(" ");
  return `cd /d ${cmdQuote(cwd)} && ${cmd}`;
}

function buildRemoteCommand(remoteShell: RemoteShell, remoteCwd: string | undefined, remoteCommand: string[]): string {
  switch (remoteShell) {
    case "posix":
      return buildPosixRemoteCommand(remoteCwd, remoteCommand);
    case "cmd":
      return buildCmdRemoteCommand(remoteCwd, remoteCommand);
    default: {
      // remoteShell is typed as RemoteShell (two members) but this value can
      // arrive from persisted/parsed data (an SSH profile loaded from disk)
      // that bypasses the type system at runtime — refuse rather than
      // silently mis-quoting under a scheme we don't recognize. See the
      // RemoteShell doc comment for why a PowerShell-DefaultShell remote is
      // the case most likely to land here, and why it's unsupported in v1.
      const unsupported: string = remoteShell as string;
      throw new Error(`buildSshArgv: unsupported remoteShell ${JSON.stringify(unsupported)} — declare "posix" or "cmd"`);
    }
  }
}

/** Builds the ssh argv for one pane. With no `remoteCommand`, the result is
 *  just the program plus any options plus the destination — a login shell,
 *  ssh allocates its own tty. With a `remoteCommand`, `-t` (force pty
 *  allocation — required for a remote TUI) precedes the option flags, and
 *  the destination is followed by the end-of-options separator `--` and
 *  ONE quoted remote-command string, so a hostile remote command can never
 *  be reinterpreted as more ssh options or more argv elements. */
export function buildSshArgv(program: string, params: SshCommandParams): string[] {
  // Cheap defensive refusal, in depth: `destination` sits before `--`, so
  // it's still raw ssh option surface, and a leading `-` would be parsed by
  // ssh as an option (e.g. `-oProxyCommand=<cmd>` runs `<cmd>` locally).
  // Today `destination` is always user-typed (agents don't reach SSH
  // profiles), so this isn't a live hole; full destination validation is
  // S1/S3's, this is just the one-line backstop if it ever does.
  if (params.destination.startsWith("-")) {
    throw new Error(
      `buildSshArgv: refusing a destination starting with '-' (${JSON.stringify(params.destination)}) — ` +
        `ssh would parse it as an option, not a target`,
    );
  }

  const hasRemoteCommand = !!params.remoteCommand && params.remoteCommand.length > 0;
  const argv: string[] = [program];

  if (hasRemoteCommand) argv.push("-t");

  if (params.port !== undefined) argv.push("-p", String(params.port));
  if (params.identityFile) argv.push("-i", params.identityFile);
  if (params.keepaliveSeconds !== undefined) {
    argv.push("-o", `ServerAliveInterval=${params.keepaliveSeconds}`);
  }
  if (params.extraArgs) argv.push(...params.extraArgs);

  argv.push(params.destination);

  if (hasRemoteCommand) {
    argv.push("--");
    argv.push(buildRemoteCommand(params.remoteShell, params.remoteCwd, params.remoteCommand!));
  }

  return argv;
}

/** Whether a claude remote command line is being freshly minted or resumed —
 *  the loomux-minted session id is the same either way, only the flag
 *  changes (mirrors the local pre-mint mechanism launcher.ts already uses:
 *  `--session-id <id>` to create, `--resume <id>` to reattach). */
export type ClaudeSessionMode = "fresh" | "resume";

/** The claude session-identity flag pair for `mode`, space form (matches
 *  the flag shape claude itself documents; loomux's local launch path uses
 *  the same space-form convention). */
export function claudeSessionArgs(sessionId: string, mode: ClaudeSessionMode): string[] {
  return mode === "resume" ? ["--resume", sessionId] : ["--session-id", sessionId];
}

/** Rewrites a claude remote command's argv from its fresh shape to its
 *  resume shape: inserts `--resume <sessionId>` right after the program
 *  name. Only understands the space form (`--session-id <id>` / `--resume
 *  <id>`) — the same form `claudeSessionArgs` emits and the only form the
 *  local launch path ever produces — so it does NOT recognize an `=` form
 *  (`--resume=abc` survives untouched, alongside the newly-inserted
 *  `--resume <sessionId>`). The match is also purely by token identity, not
 *  position: ANY token that equals `--session-id` or `--resume` is stripped
 *  together with the token right after it, even if that token is actually
 *  another flag's value rather than a session flag. Pure token surgery —
 *  never inspects `sessionId` beyond using it as the resume value, so it
 *  works whether the original command was minted fresh moments ago or
 *  restored from a persisted pane. */
export function rewriteClaudeCommandForResume(remoteCommand: string[], sessionId: string): string[] {
  const kept: string[] = [];
  for (let i = 0; i < remoteCommand.length; i++) {
    const token = remoteCommand[i];
    if (token === "--session-id" || token === "--resume") {
      i++; // also drop the id token that follows the flag
      continue;
    }
    kept.push(token);
  }
  const [program, ...rest] = kept;
  return program === undefined ? kept : [program, "--resume", sessionId, ...rest];
}

/** The reconnect-time rewrite: rebuilds a full ssh argv in resume form from
 *  the same params a fresh connect used, given the session id that fresh
 *  connect recorded. This is what a dormant-pane Reconnect action (or a
 *  post-disconnect exit-banner Reconnect) calls — no reason to instead
 *  express it as a plain `buildSshArgv` at that call site, and the
 *  once-implemented rewrite gets the same adversarial quoting coverage as
 *  the fresh builder. */
export function sshResumeArgv(program: string, params: SshCommandParams, sessionId: string): string[] {
  if (!params.remoteCommand || params.remoteCommand.length === 0) {
    throw new Error("sshResumeArgv requires a remoteCommand to rewrite into resume form");
  }
  return buildSshArgv(program, {
    ...params,
    remoteCommand: rewriteClaudeCommandForResume(params.remoteCommand, sessionId),
  });
}
