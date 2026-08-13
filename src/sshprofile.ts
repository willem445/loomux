// SSH connection profiles (#887, slice S1) — the pure schema layer for
// `sshprofiles.json`, a sibling of tabs.json/settings.json in the loomux AppData
// root (src-tauri/src/uistate.rs owns the atomic write + corrupt-quarantine, and
// never parses this schema: the blob is opaque to it, exactly as tabs.json and
// settings.json are). Reached through the typed loadSshProfiles/saveSshProfiles
// wrappers in pty.ts (CLAUDE.md constraint 5). DOM-free and unit-tested
// (test/sshprofile.test.ts) like tabstore.ts/settings.ts.
//
// A profile is the *declaration* of an SSH target: where to connect and how the
// remote end should be addressed. It is not a session, not a credential store,
// and not an argv — building the actual ssh command line is sshcommand.ts's job
// (slice S2), which takes flat parameters rather than this type so the two
// modules stay independent.
//
// ## THE INVARIANT: no secrets, ever
//
// loomux holds no SSH credentials. Authentication is deferred entirely to the
// user's own ssh setup — ssh_config, ssh-agent, and interactive prompts, which
// simply render in the pane because the pane IS a terminal. This mirrors gh.rs's
// posture (loomux stores no GitHub token; it shells out to the user's
// authenticated gh) and it is why `sshprofiles.json` is a plain, hand-readable,
// unencrypted file: it is safe to be one, and it must stay safe to be one.
//
// Two structural guards keep that true rather than merely stated:
//
//  1. **Encode and decode are both allowlists.** `decodeProfile` reads only the
//     fields declared in `SshProfile`; `profileToWire` writes only those fields.
//     A password, passphrase or private key hand-added to the file (or attached
//     to a profile object by some future caller) is dropped on the way in and
//     never written on the way out — it cannot survive one load/save cycle.
//  2. **`identityFile` is a PATH and is checked to be one.** It is the one field
//     through which key *material* could enter by the front door — paste a PEM
//     blob into a field labelled "identity file" and a naive store would write
//     the private key straight into the JSON. A value carrying a newline or a
//     PEM armour header is therefore refused (the field degrades to null, and
//     the profile survives without `-i`): storing key material is the failure
//     this whole design exists to prevent, so this fails closed.
//
// The schema is a PUBLIC CONTRACT — a file on the user's disk that older and
// newer builds both read. Its full write-up (fields, forward-compat, the
// argument above) lands in doc/design/ssh-panes.md with the rest of the feature
// (slice S5).

/** Bump when the persisted shape changes in a way decode must branch on.
 *  v1 is the shape below. Decode treats an absent version as v1: every field
 *  is validated on its own merits, so an unversioned hand-written file works. */
export const SSH_PROFILES_SCHEMA_VERSION = 1;

/** Which shell the REMOTE host runs, as a user declaration rather than a guess.
 *  sshcommand.ts (S2) needs it to quote a remote command, and the remote's
 *  default shell is genuinely unknowable from here — probing it would be a
 *  round trip that can be wrong anyway (a forced command, a chsh'd account).
 *  Guessing "posix because most hosts are" would also bake an assumption about
 *  the user's machines into product code (CLAUDE.md constraint 8). So the user
 *  says which it is, and "posix" is merely the DEFAULT for a profile that never
 *  says. */
export type RemoteShell = "posix" | "windows";

export const REMOTE_SHELLS: readonly RemoteShell[] = ["posix", "windows"];

/** The default `remoteShell` for a profile that doesn't declare one. */
export const DEFAULT_REMOTE_SHELL: RemoteShell = "posix";

/** Inclusive bound on `keepaliveSeconds`. The lower bound is 1 because ssh reads
 *  `ServerAliveInterval 0` as "disabled" — which is what an ABSENT field already
 *  means here, and spelling one meaning two ways in a hand-edited file is how a
 *  user ends up believing they enabled something they disabled. The upper bound
 *  is a day: past that the option cannot plausibly be doing the job it exists
 *  for, so the value is a typo, not a preference. */
export const MIN_KEEPALIVE_SECONDS = 1;
export const MAX_KEEPALIVE_SECONDS = 86_400;

/** One saved SSH target. Every optional field is `null` when unset, and unset
 *  means "loomux passes nothing for this" — the user's own ssh_config then
 *  decides, which is the whole point of the no-credentials posture. */
export interface SshProfile {
  /** Stable identity, minted once when the profile is created (the launcher
   *  uses the webview's `crypto.randomUUID` — Web Crypto, NOT a getrandom crate;
   *  CLAUDE.md constraint 2 is about the Rust dependency graph, and nothing in
   *  this path touches it). A persisted pane records this id, not the profile's
   *  contents, so renaming or re-editing a profile keeps the panes that use it
   *  pointed at it. */
  id: string;
  /** Display label — what the picker shows. Free text; not an identity. */
  name: string;
  /** What is handed to ssh as its destination: `user@host`, a bare `host`, or a
   *  `Host` alias out of the user's ssh_config. ONE field rather than separate
   *  host/user fields, because an alias has neither — splitting them would make
   *  the alias case (the one that carries the user's own carefully configured
   *  ProxyJump, IdentityFile, User, …) unrepresentable. */
  destination: string;
  /** `-p` port, or null to let ssh_config / the default 22 decide. */
  port: number | null;
  /** `-i` identity file — a PATH to a private key, never the key itself. See the
   *  invariant at the top of this file. */
  identityFile: string | null;
  /** Directory to `cd` into on the REMOTE host before launching the CLI. Null =
   *  the remote login directory. A remote path, so it is never normalized or
   *  validated against the local filesystem here; S2 quotes it for the declared
   *  `remoteShell`. */
  remoteCwd: string | null;
  /** Which agent CLI this profile launches by default (`claude`, `copilot`, …),
   *  or null for a plain remote login shell. Stored as a free string and
   *  validated against the live CLI catalog at launch time (S3) rather than
   *  here: a profile naming a CLI this build doesn't know is a profile to warn
   *  about, not a profile to silently delete on load. */
  defaultCli: string | null;
  /** The remote's shell family — see `RemoteShell`. Always set (defaulted). */
  remoteShell: RemoteShell;
  /** `ServerAliveInterval`, or null to emit nothing at all so the user's own
   *  ssh_config keepalive settings win untouched. */
  keepaliveSeconds: number | null;
  /** Extra ssh flags, as argv words (`["-J", "jump.example.net"]`). The user's
   *  escape hatch for anything this schema doesn't model. Deliberately NOT
   *  filtered against a list of "dangerous" options: these are the same flags
   *  the user can already put in their own ssh_config, they reach ssh as argv
   *  (not through a shell), and a filter would be security theatre over a
   *  trusted local file while breaking legitimate flags. Empty when unset. */
  extraArgs: string[];
}

export interface SshProfileStore {
  /** Stamped by encode; an unversioned file decodes as v1. */
  schemaVersion: number;
  profiles: SshProfile[];
}

/** The first-run / post-quarantine seed. A FUNCTION and not an exported constant
 *  (unlike `DEFAULT_SETTINGS`, whose fields are all scalars) because this one
 *  carries a mutable array: a shared constant would let one caller's push land
 *  in every other caller's "empty" store. */
export function emptySshProfileStore(): SshProfileStore {
  return { schemaVersion: SSH_PROFILES_SCHEMA_VERSION, profiles: [] };
}

/** A non-blank string, trimmed — or null. The single "is this field usable at
 *  all" test, so blank-vs-absent-vs-whitespace never diverge between fields. */
function trimmedOrNull(v: unknown): string | null {
  if (typeof v !== "string") return null;
  const t = v.trim();
  return t ? t : null;
}

/** An integer inside `[min, max]`, or null. Rejects `NaN`/`Infinity`/floats and
 *  anything non-numeric, so a hand-edited `"22"` (a string) reads as unset
 *  rather than being coerced into a port. */
function boundedInt(v: unknown, min: number, max: number): number | null {
  if (typeof v !== "number" || !Number.isInteger(v)) return null;
  return v >= min && v <= max ? v : null;
}

/** The `identityFile` guard — see guard 2 of the invariant. A key PATH is a
 *  single-line string; a key ITSELF is multi-line and starts with PEM armour.
 *  Anything with a line break or a `-----BEGIN` header is therefore refused
 *  outright rather than written to disk. (A newline would also be nonsense in
 *  an argv word, so nothing legitimate is lost.) */
function identityPathOrNull(v: unknown): string | null {
  const path = trimmedOrNull(v);
  if (path === null) return null;
  if (/[\r\n]/.test(path)) return null;
  if (path.startsWith("-----BEGIN")) return null;
  return path;
}

/** The `destination` guard. ssh parses argv, and a word starting with `-` is an
 *  OPTION to it, not a host — a profile whose destination begins with a dash is
 *  therefore not a destination at all, however it got there, and letting one
 *  through would hand the user's ssh an arbitrary flag from a stored file.
 *  Internal whitespace is refused for the same class of reason: a destination is
 *  one argv word, and a "host" containing a space is a mangled hand-edit rather
 *  than a target that could ever connect. */
function destinationOrNull(v: unknown): string | null {
  const dest = trimmedOrNull(v);
  if (dest === null) return null;
  if (dest.startsWith("-")) return null;
  if (/\s/.test(dest)) return null;
  return dest;
}

function isRemoteShell(v: unknown): v is RemoteShell {
  return typeof v === "string" && (REMOTE_SHELLS as readonly string[]).includes(v);
}

/** Validate one persisted profile, returning null on any malformation so the
 *  caller can drop THAT ENTRY and keep the rest (tabstore.ts's docked-pane
 *  tolerance, for the same reason: losing one profile beats losing the list).
 *  Only `id`, `name` and `destination` can fail an entry — without any one of
 *  them there is nothing to show, nothing to point a pane at, or nothing to
 *  connect to. Every other field degrades to null/default on its own. */
function decodeProfile(v: unknown): SshProfile | null {
  if (!v || typeof v !== "object") return null;
  const r = v as Record<string, unknown>;
  const id = trimmedOrNull(r.id);
  const name = trimmedOrNull(r.name);
  const destination = destinationOrNull(r.destination);
  if (!id || !name || !destination) return null;
  return {
    id,
    name,
    destination,
    port: boundedInt(r.port, 1, 65_535),
    identityFile: identityPathOrNull(r.identityFile),
    remoteCwd: trimmedOrNull(r.remoteCwd),
    defaultCli: trimmedOrNull(r.defaultCli),
    remoteShell: isRemoteShell(r.remoteShell) ? r.remoteShell : DEFAULT_REMOTE_SHELL,
    keepaliveSeconds: boundedInt(r.keepaliveSeconds, MIN_KEEPALIVE_SECONDS, MAX_KEEPALIVE_SECONDS),
    extraArgs: Array.isArray(r.extraArgs)
      ? r.extraArgs.filter((a): a is string => typeof a === "string")
      : [],
  };
}

/** One VALIDATED profile as it is written. An explicit key-by-key projection —
 *  the encode-side half of the allowlist, so a property some caller hung on the
 *  object (a `password` it thought it needed, a stray field from a future
 *  build) is never serialized. Optional fields are OMITTED when unset rather
 *  than written as `null`: this file is hand-editable, and an absent key is how
 *  "loomux passes nothing, your ssh_config decides" should look when read.
 *
 *  Takes an already-`decodeProfile`d value, which is what makes the two
 *  directions provably agree: there is ONE implementation of every field guard,
 *  not a write-side copy that can drift from the read-side one. */
function profileToWire(p: SshProfile): Record<string, unknown> {
  return {
    id: p.id,
    name: p.name,
    destination: p.destination,
    remoteShell: p.remoteShell,
    ...(p.port !== null ? { port: p.port } : {}),
    ...(p.identityFile !== null ? { identityFile: p.identityFile } : {}),
    ...(p.remoteCwd !== null ? { remoteCwd: p.remoteCwd } : {}),
    ...(p.defaultCli !== null ? { defaultCli: p.defaultCli } : {}),
    ...(p.keepaliveSeconds !== null ? { keepaliveSeconds: p.keepaliveSeconds } : {}),
    ...(p.extraArgs.length ? { extraArgs: p.extraArgs } : {}),
  };
}

/** Serialize the profile store for `saveSshProfiles`. Pretty-printed like
 *  settings.ts (this is a file a user may reasonably open and edit by hand),
 *  and it runs the SAME per-field guards decode does — a store assembled in
 *  memory gets no weaker validation than one read off disk, which is what makes
 *  the no-secrets invariant hold in both directions.
 *
 *  Entries that could not be a profile at all (no id / name / destination) are
 *  dropped rather than written, so a save can never introduce a row the next
 *  load would silently discard. */
export function encodeSshProfiles(store: SshProfileStore): string {
  const validated = (store.profiles ?? [])
    .map(decodeProfile)
    .filter((p): p is SshProfile => p !== null);
  return JSON.stringify(
    { schemaVersion: SSH_PROFILES_SCHEMA_VERSION, profiles: dedupeById(validated).map(profileToWire) },
    null,
    2
  );
}

/** First occurrence of each id wins. Ids are what a persisted pane stores to
 *  find its profile again, so a duplicated id (a hand-copied entry, most
 *  likely) makes that lookup ambiguous; resolving it here — once, at the
 *  boundary — means no consumer has to. */
function dedupeById<T extends { id: string }>(items: T[]): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const item of items) {
    if (!item || seen.has(item.id)) continue;
    seen.add(item.id);
    out.push(item);
  }
  return out;
}

/** Parse the persisted profile store, tolerating anything malformed by
 *  returning null (the caller then seeds `emptySshProfileStore()`), and
 *  tolerating a malformed ENTRY by dropping just that entry. Never throws: this
 *  runs at boot on a file the user is invited to hand-edit. */
export function decodeSshProfiles(raw: string | null): SshProfileStore | null {
  if (!raw) return null;
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!v || typeof v !== "object") return null;
  const obj = v as { profiles?: unknown; schemaVersion?: unknown };
  // A file without a `profiles` array is not this file — degrade to defaults
  // rather than inventing an empty store over whatever it actually is.
  if (!Array.isArray(obj.profiles)) return null;
  const profiles = dedupeById(
    obj.profiles.map(decodeProfile).filter((p): p is SshProfile => p !== null)
  );
  const schemaVersion =
    typeof obj.schemaVersion === "number" && Number.isInteger(obj.schemaVersion)
      ? obj.schemaVersion
      : SSH_PROFILES_SCHEMA_VERSION;
  return { schemaVersion, profiles };
}
