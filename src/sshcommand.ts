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
 *  quoting choice is the user's explicit, documented call. */
export type RemoteShell = "posix" | "windows";

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
 *  double any embedded double quote (the only escape cmd.exe's own
 *  tokenizer honors inside a quoted run). Unlike POSIX single-quoting, this
 *  is NOT a fully injection-proof scheme — cmd.exe still expands `%VAR%`
 *  even inside double quotes, a known, unclosable cmd.exe hazard. That gap
 *  is why `remoteShell` is documented as a declaration, not a security
 *  boundary: the caller chose "windows" and accepts cmd.exe's own quoting
 *  limits, the same way choosing "posix" accepts whatever the remote's
 *  actual shell turns out to be. */
function windowsQuote(token: string): string {
  if (token === "") return '""';
  return `"${token.replace(/"/g, '""')}"`;
}

function buildPosixRemoteCommand(remoteCwd: string | undefined, remoteCommand: string[]): string {
  const cmd = remoteCommand.map(posixQuote).join(" ");
  return remoteCwd ? `cd ${posixQuote(remoteCwd)} && exec ${cmd}` : `exec ${cmd}`;
}

function buildWindowsRemoteCommand(remoteCwd: string | undefined, remoteCommand: string[]): string {
  const cmd = remoteCommand.map(windowsQuote).join(" ");
  return remoteCwd ? `cd /d ${windowsQuote(remoteCwd)} && ${cmd}` : cmd;
}

/** Builds the ssh argv for one pane. With no `remoteCommand`, the result is
 *  just the program plus any options plus the destination — a login shell,
 *  ssh allocates its own tty. With a `remoteCommand`, `-t` (force pty
 *  allocation — required for a remote TUI) precedes the option flags, and
 *  the destination is followed by the end-of-options separator `--` and
 *  ONE quoted remote-command string, so a hostile remote command can never
 *  be reinterpreted as more ssh options or more argv elements. */
export function buildSshArgv(program: string, params: SshCommandParams): string[] {
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
    argv.push(
      params.remoteShell === "windows"
        ? buildWindowsRemoteCommand(params.remoteCwd, params.remoteCommand!)
        : buildPosixRemoteCommand(params.remoteCwd, params.remoteCommand!),
    );
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
 *  resume shape: strips any existing `--session-id`/`--resume` pair
 *  (wherever it sits) and inserts `--resume <sessionId>` right after the
 *  program name. Pure token surgery — never inspects `sessionId` beyond
 *  using it as the resume value, so it works whether the original command
 *  was minted fresh moments ago or restored from a persisted pane. */
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
