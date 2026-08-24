// SSH connection profiles (#887 S1) — sshprofile.ts. Mirrors the shape of
// tabstore.test.ts / settings.test.ts: round-trip, degradation on a malformed
// file, per-entry tolerance — plus the tests that matter most here, the ones
// pinning the NO-SECRETS invariant this schema exists to make safe.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  encodeSshProfiles,
  decodeSshProfiles,
  emptySshProfileStore,
  SSH_PROFILES_SCHEMA_VERSION,
  DEFAULT_REMOTE_SHELL,
  MAX_KEEPALIVE_SECONDS,
  SshProfilesStore,
  type SshProfile,
  type SshProfileIo,
  type SshProfileStore,
} from "../src/sshprofile.ts";

/** A fully-populated profile — every optional field set, so a round-trip that
 *  drops or mangles any one of them fails. */
function fullProfile(over: Partial<SshProfile> = {}): SshProfile {
  return {
    id: "p1",
    name: "build box",
    destination: "dev@build.example.net",
    port: 2222,
    identityFile: "C:\\Users\\me\\.ssh\\id_ed25519",
    remoteCwd: "/srv/app",
    defaultCli: "claude",
    remoteShell: "posix",
    keepaliveSeconds: 30,
    extraArgs: ["-J", "jump.example.net"],
    ...over,
  };
}

function store(...profiles: SshProfile[]): SshProfileStore {
  return { schemaVersion: SSH_PROFILES_SCHEMA_VERSION, profiles };
}

/** Decode, asserting the store survived at all — most tests are about a single
 *  profile's fields and shouldn't each re-narrow the null. */
function decodeOne(raw: string): SshProfile {
  const decoded = decodeSshProfiles(raw);
  assert.ok(decoded, `expected a store, got ${decoded}`);
  assert.equal(decoded.profiles.length, 1, "expected exactly one surviving profile");
  return decoded.profiles[0];
}

// ---------- round-trip ----------

test("a fully-populated profile round-trips field for field", () => {
  const p = fullProfile();
  assert.deepEqual(decodeSshProfiles(encodeSshProfiles(store(p))), store(p));
});

test("a minimal profile round-trips, with every unset field null and the shell defaulted", () => {
  // The other half of the round-trip: unset must survive as unset, because
  // "loomux passes nothing here" is a meaningful state (the user's ssh_config
  // decides) and not a placeholder to be filled in.
  const minimal: SshProfile = {
    id: "p2",
    name: "alias only",
    destination: "myalias",
    port: null,
    identityFile: null,
    remoteCwd: null,
    defaultCli: null,
    remoteShell: DEFAULT_REMOTE_SHELL,
    keepaliveSeconds: null,
    extraArgs: [],
  };
  assert.deepEqual(decodeSshProfiles(encodeSshProfiles(store(minimal))), store(minimal));
});

test("an unset optional field is OMITTED from the file, not written as null", () => {
  // The file is hand-editable; an absent key is how "nothing is passed for
  // this" should read. A literal `"port": null` invites a user to "fix" it.
  const written = JSON.parse(
    encodeSshProfiles(store(fullProfile({ port: null, keepaliveSeconds: null, extraArgs: [] })))
  );
  const wire = written.profiles[0];
  assert.equal("port" in wire, false, "port should be absent when unset");
  assert.equal("keepaliveSeconds" in wire, false, "keepaliveSeconds should be absent when unset");
  assert.equal("extraArgs" in wire, false, "extraArgs should be absent when empty");
  // …while the always-present fields stay present.
  assert.equal(wire.destination, "dev@build.example.net");
  assert.equal(wire.remoteShell, "posix");
});

test("encode stamps the schema version", () => {
  const written = JSON.parse(encodeSshProfiles(store(fullProfile())));
  assert.equal(written.schemaVersion, SSH_PROFILES_SCHEMA_VERSION);
});

test("encode carries a FUTURE file's schemaVersion instead of downgrading it", () => {
  // #907 review NB2, deferred to S3's opening commit. Decode already carries the
  // file's own version into the store; an encode that re-stamped it made an
  // unrelated edit (adding one profile) silently re-label the whole file. The
  // scenario that made it worth fixing before S3 gave this file its first
  // WRITER: a v2 build stamps 2, the user rolls back to a build like this one,
  // and the first save both drops the v2 fields (the allowlist, deliberate) and
  // claims the file was always v1.
  const written = JSON.parse(
    encodeSshProfiles({ schemaVersion: 2, profiles: [fullProfile()] })
  );
  assert.equal(written.schemaVersion, 2, "the store's own version must survive a save");
  // …and a full load→save round trip of a v2 file is version-stable, which is
  // the property a rollback actually depends on.
  const raw = JSON.stringify({ schemaVersion: 2, profiles: [fullProfile()] });
  const reloaded = decodeSshProfiles(encodeSshProfiles(decodeSshProfiles(raw)!));
  assert.equal(reloaded?.schemaVersion, 2);
});

test("a nonsense schemaVersion falls back to this build's, matching decode", () => {
  // The fallback must agree with `decodeSshProfiles`'s own reading of a mangled
  // header, or a hand-edited file would mean one thing on load and another on
  // save. Non-integers, zero and non-numbers all land on the same answer.
  for (const bogus of [0, -3, 1.5, NaN, "2", null, undefined]) {
    const written = JSON.parse(
      encodeSshProfiles({
        schemaVersion: bogus as unknown as number,
        profiles: [fullProfile()],
      })
    );
    assert.equal(
      written.schemaVersion,
      SSH_PROFILES_SCHEMA_VERSION,
      `schemaVersion ${String(bogus)} must fall back, not be written through`
    );
  }
});

test("an empty store round-trips as an empty store, not as null", () => {
  // Distinct from first run: the user deleted their last profile, and that
  // deletion has to survive a save/load or it undoes itself.
  assert.deepEqual(decodeSshProfiles(encodeSshProfiles(emptySshProfileStore())), {
    schemaVersion: SSH_PROFILES_SCHEMA_VERSION,
    profiles: [],
  });
});

// ---------- the no-secrets invariant ----------

test("a secret hand-added to the file is dropped on load and never written back", () => {
  // The invariant, end to end. sshprofiles.json is a plain unencrypted file
  // precisely because it can never hold a credential; if a key smuggled into it
  // could survive one load/save cycle, loomux would be storing secrets in
  // cleartext on behalf of a user who only hand-edited a config file.
  const raw = JSON.stringify({
    schemaVersion: 1,
    profiles: [
      {
        id: "p1",
        name: "build box",
        destination: "dev@build.example.net",
        password: "hunter2",
        passphrase: "correct horse battery staple",
        privateKey: "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END…",
      },
    ],
  });
  const p = decodeOne(raw);
  assert.equal("password" in p, false, "password must not survive decode");
  assert.equal("passphrase" in p, false, "passphrase must not survive decode");
  assert.equal("privateKey" in p, false, "privateKey must not survive decode");
  // …and the re-written file carries no trace of any of it.
  const rewritten = encodeSshProfiles({ schemaVersion: 1, profiles: [p] });
  assert.equal(rewritten.includes("hunter2"), false);
  assert.equal(rewritten.includes("correct horse"), false);
  assert.equal(rewritten.includes("BEGIN OPENSSH PRIVATE KEY"), false);
});

test("a secret attached to a profile OBJECT is never serialized either", () => {
  // The encode-side half of the allowlist. Decode can only protect the file
  // from itself; this protects it from a caller that hands us an object with
  // one extra property on it.
  const smuggled = { ...fullProfile(), password: "hunter2" } as unknown as SshProfile;
  const written = encodeSshProfiles(store(smuggled));
  assert.equal(written.includes("hunter2"), false, "encode must write only the declared fields");
  assert.equal(written.includes("password"), false);
});

test("identityFile carrying key MATERIAL is refused; the profile survives without it", () => {
  // The one field key material can enter through the front door: paste a PEM
  // blob into something labelled "identity file" and a naive store writes the
  // private key into the JSON. Fail closed — drop the field, keep the profile.
  const pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEA\n-----END OPENSSH PRIVATE KEY-----";
  const p = decodeOne(
    JSON.stringify({ profiles: [{ ...fullProfile(), identityFile: pem }] })
  );
  assert.equal(p.identityFile, null, "PEM armour is key material, not a path");
  assert.equal(p.destination, "dev@build.example.net", "the rest of the profile is untouched");
  // And it cannot get out through encode either.
  const written = encodeSshProfiles(store(fullProfile({ identityFile: pem })));
  assert.equal(written.includes("BEGIN OPENSSH PRIVATE KEY"), false);
  assert.equal(written.includes("b3BlbnNzaC1rZXktdjEA"), false);
});

test("a multi-line identityFile is refused even without PEM armour", () => {
  // The armour check alone would pass a key pasted without its header, and a
  // newline is nonsense in an argv word regardless — so the shape, not just the
  // marker, is what's rejected.
  const p = decodeOne(
    JSON.stringify({ profiles: [{ ...fullProfile(), identityFile: "line one\nline two" }] })
  );
  assert.equal(p.identityFile, null);
});

test("PEM armour with its newlines stripped is refused, in either case", () => {
  // #907 review NB3, deferred here: the line-break test is what catches real key
  // material (every key wraps its body), so the armour test only ever fires
  // independently for a header pasted with its newlines already gone — and a
  // paste mangled that far has no reason to have preserved case either. Both
  // spellings of the one case the belt exists for.
  for (const armoured of [
    "-----BEGIN OPENSSH PRIVATE KEY----- b3BlbnNzaC1rZXktdjEA -----END OPENSSH PRIVATE KEY-----",
    "-----begin openssh private key----- b3BlbnNzaC1rZXktdjEA -----end openssh private key-----",
  ]) {
    const p = decodeOne(
      JSON.stringify({ profiles: [{ ...fullProfile(), identityFile: armoured }] })
    );
    assert.equal(p.identityFile, null, `armour must be refused: ${armoured.slice(0, 20)}…`);
    assert.equal(p.destination, "dev@build.example.net", "the rest of the profile survives");
  }
});

test("an ordinary identity PATH is kept — the guard must not reject the real case", () => {
  // Without this, "refuse everything" would pass every test above.
  assert.equal(
    decodeOne(JSON.stringify({ profiles: [fullProfile({ identityFile: "~/.ssh/id_ed25519" })] }))
      .identityFile,
    "~/.ssh/id_ed25519"
  );
});

// ---------- destination: the one field that becomes an ssh argument ----------

test("a destination starting with '-' fails the whole entry", () => {
  // ssh parses argv: a leading dash is an OPTION, not a host. A stored file
  // must not be able to hand the user's ssh an arbitrary flag.
  const decoded = decodeSshProfiles(
    JSON.stringify({ profiles: [fullProfile({ destination: "-oProxyCommand=calc.exe" })] })
  );
  assert.deepEqual(decoded, { schemaVersion: SSH_PROFILES_SCHEMA_VERSION, profiles: [] });
});

test("a HOST starting with '-' fails the entry even though the destination doesn't", () => {
  // The gap a whole-string leading-dash test cannot see: `user@-oProxy…`
  // starts with `u`. But the part after the `@` is the host, and a host is not
  // inert data — ssh_config's ProxyCommand/LocalCommand expand `%h` into a
  // command line, which is local command execution out of a stored file (the
  // shape of OpenSSH's own CVE-2023-51385).
  const decoded = decodeSshProfiles(
    JSON.stringify({ profiles: [fullProfile({ destination: "user@-oProxyCommand=calc.exe" })] })
  );
  assert.deepEqual(decoded, { schemaVersion: SSH_PROFILES_SCHEMA_VERSION, profiles: [] });
});

test("a USER starting with '-' fails the entry — via the WHOLE-WORD check", () => {
  // Named for what it actually witnesses (#907 review NF1). A dashed user shares
  // its first character with the whole destination, so `dest.startsWith("-")`
  // has already rejected this before the `@` split runs: the component guard's
  // user half could never fire, and has been removed rather than left standing
  // as a protection that isn't one. The refusal itself is unchanged — which is
  // the point of keeping this test.
  const decoded = decodeSshProfiles(
    JSON.stringify({ profiles: [fullProfile({ destination: "-oProxyCommand=calc.exe@host" })] })
  );
  assert.equal(decoded?.profiles.length, 0);
});

test("a dashed host cannot be SAVED either, not merely ignored on load", () => {
  // The other direction the finding asks for. Decode-side rejection alone would
  // leave the value sitting in the file, one lenient future reader away from
  // being honoured.
  const written = encodeSshProfiles(
    store(fullProfile({ id: "ok" }), fullProfile({ id: "bad", destination: "user@-oProxyCommand=calc.exe" }))
  );
  assert.equal(written.includes("ProxyCommand"), false, "the dashed host must not reach the file");
  assert.deepEqual(
    JSON.parse(written).profiles.map((p: { id: string }) => p.id),
    ["ok"]
  );
});

test("the component guard splits on the LAST '@', the way ssh does", () => {
  // A user part may legitimately contain '@' (an ssh_config alias, a
  // domain-shaped login). Splitting on the FIRST '@' would check "user" against
  // the wrong half and let a dashed host through — so this pins the split
  // point, not just the rejection.
  const decoded = decodeSshProfiles(
    JSON.stringify({ profiles: [fullProfile({ destination: "me@corp.example@-evil" })] })
  );
  assert.equal(decoded?.profiles.length, 0, "the real host is the part after the last @");
});

test("ordinary destinations survive — the guard must not reject the real cases", () => {
  // Without this, "refuse anything with a dash in it" would pass every test
  // above while breaking hostnames, which routinely contain dashes.
  for (const ok of [
    "dev@build-box.example.net", // dashes INSIDE a host are normal
    "build-box",
    "myalias",
    "me@corp.example@host", // a '@' in the user part is legitimate
    "ssh://dev@host",
  ]) {
    const p = decodeOne(JSON.stringify({ profiles: [fullProfile({ destination: ok })] }));
    assert.equal(p.destination, ok, `destination ${ok} should be kept`);
  }
});

test("a destination with an empty half is a mangled hand-edit, not a target", () => {
  for (const bad of ["@host", "user@"]) {
    const decoded = decodeSshProfiles(
      JSON.stringify({ profiles: [fullProfile({ destination: bad })] })
    );
    assert.equal(decoded?.profiles.length, 0, `destination ${bad}`);
  }
});

test("a destination with whitespace fails the entry", () => {
  const decoded = decodeSshProfiles(
    JSON.stringify({ profiles: [fullProfile({ destination: "host -oBatchMode=yes" })] })
  );
  assert.equal(decoded?.profiles.length, 0);
});

test("encode refuses to write an entry that decode would reject", () => {
  // Otherwise a save could introduce a row the very next load silently drops —
  // the user's profile vanishing one restart later, with nothing to point at.
  const written = JSON.parse(
    encodeSshProfiles(store(fullProfile({ id: "ok" }), fullProfile({ id: "bad", destination: "-J evil" })))
  );
  assert.deepEqual(
    written.profiles.map((p: { id: string }) => p.id),
    ["ok"]
  );
});

// ---------- per-entry tolerance ----------

test("one malformed entry is dropped and the rest of the list survives", () => {
  const decoded = decodeSshProfiles(
    JSON.stringify({
      profiles: [
        fullProfile({ id: "a", name: "first" }),
        { id: "b", name: "no destination" },
        "not an object",
        null,
        fullProfile({ id: "c", name: "third" }),
      ],
    })
  );
  assert.deepEqual(
    decoded?.profiles.map((p) => p.id),
    ["a", "c"],
    "losing one profile must not lose the list"
  );
});

test("an entry missing id or name is dropped — there is nothing to show or point at", () => {
  const decoded = decodeSshProfiles(
    JSON.stringify({
      profiles: [
        { name: "no id", destination: "h" },
        { id: "x", destination: "h" },
        { id: "  ", name: "  ", destination: "h" },
      ],
    })
  );
  assert.equal(decoded?.profiles.length, 0);
});

test("a duplicate id keeps the first entry only", () => {
  // A pane persists the profile ID, so two rows sharing one id make that lookup
  // ambiguous. Resolved once, at the boundary.
  const decoded = decodeSshProfiles(
    JSON.stringify({
      profiles: [fullProfile({ id: "dup", name: "kept" }), fullProfile({ id: "dup", name: "shadow" })],
    })
  );
  assert.deepEqual(
    decoded?.profiles.map((p) => p.name),
    ["kept"]
  );
});

// ---------- whole-file degradation ----------

test("null (first run) decodes to null rather than throwing", () => {
  assert.equal(decodeSshProfiles(null), null);
});

test("invalid JSON decodes to null rather than throwing", () => {
  assert.equal(decodeSshProfiles("{ not json"), null);
});

test("a JSON value that isn't this file decodes to null, not an empty store", () => {
  // "Some other file entirely" and "a store with no profiles" are different
  // facts: seeding defaults over the first is right, claiming the user deleted
  // their profiles is not.
  assert.equal(decodeSshProfiles("42"), null);
  assert.equal(decodeSshProfiles('"a string"'), null);
  assert.equal(decodeSshProfiles("{}"), null);
  assert.equal(decodeSshProfiles('{"profiles":"nope"}'), null);
});

test("emptySshProfileStore hands out a fresh array each time", () => {
  // A shared exported constant would let one caller's push land in another
  // caller's "empty" store.
  const a = emptySshProfileStore();
  a.profiles.push(fullProfile());
  assert.deepEqual(emptySshProfileStore().profiles, []);
});

// ---------- individual field validation ----------

test("a wrong-typed or out-of-range port degrades to unset, never to a coerced value", () => {
  const cases: [unknown, number | null][] = [
    ["2222", null], // a hand-edited string is not a port
    [0, null],
    [65_536, null],
    [22.5, null],
    [-1, null],
    [22, 22],
    [65_535, 65_535],
  ];
  for (const [input, expected] of cases) {
    const p = decodeOne(JSON.stringify({ profiles: [{ ...fullProfile(), port: input }] }));
    assert.equal(p.port, expected, `port ${JSON.stringify(input)}`);
  }
});

test("keepaliveSeconds 0 is unset, not 'ServerAliveInterval 0'", () => {
  // ssh reads 0 as "disabled", which an ABSENT field already means here.
  // Keeping both spellings would let a user believe they enabled a keepalive
  // they in fact turned off.
  const p = decodeOne(JSON.stringify({ profiles: [{ ...fullProfile(), keepaliveSeconds: 0 }] }));
  assert.equal(p.keepaliveSeconds, null);
});

test("keepaliveSeconds keeps a real value and rejects an implausible one", () => {
  assert.equal(
    decodeOne(JSON.stringify({ profiles: [{ ...fullProfile(), keepaliveSeconds: 15 }] }))
      .keepaliveSeconds,
    15
  );
  assert.equal(
    decodeOne(
      JSON.stringify({
        profiles: [{ ...fullProfile(), keepaliveSeconds: MAX_KEEPALIVE_SECONDS + 1 }],
      })
    ).keepaliveSeconds,
    null
  );
});

test("an unknown remoteShell falls back to the default instead of failing the entry", () => {
  for (const unknown of ["fish", "powershell", "windows"]) {
    // "windows" is in this list deliberately: it was this value's spelling
    // before the rename to "cmd", and it is what a hand-written file copied
    // from an early draft would say. It is NOT a supported alias — a host
    // whose shell we can't name gets the default, not cmd.exe quoting chosen
    // on its behalf. (No migration is owed: the schema has never shipped.)
    const p = decodeOne(JSON.stringify({ profiles: [{ ...fullProfile(), remoteShell: unknown }] }));
    assert.equal(p.remoteShell, DEFAULT_REMOTE_SHELL, `remoteShell ${unknown}`);
  }
});

test("remoteShell 'cmd' is preserved — the field must actually do something", () => {
  const p = decodeOne(JSON.stringify({ profiles: [fullProfile({ remoteShell: "cmd" })] }));
  assert.equal(p.remoteShell, "cmd");
});

test("extraArgs keeps its string words and drops non-strings", () => {
  const p = decodeOne(
    JSON.stringify({ profiles: [{ ...fullProfile(), extraArgs: ["-J", 7, null, "jump"] }] })
  );
  assert.deepEqual(p.extraArgs, ["-J", "jump"]);
});

test("a non-array extraArgs degrades to empty rather than failing the entry", () => {
  const p = decodeOne(JSON.stringify({ profiles: [{ ...fullProfile(), extraArgs: "-J jump" }] }));
  assert.deepEqual(p.extraArgs, []);
});

test("an unknown extra key is ignored rather than rejecting the file", () => {
  // Forward-compat: a NEWER build's field must not cost this build the profile.
  const p = decodeOne(
    JSON.stringify({ profiles: [{ ...fullProfile(), someFutureField: { a: 1 } }] })
  );
  assert.equal(p.id, "p1");
  assert.equal("someFutureField" in p, false);
});

test("an unversioned file decodes as v1 rather than being rejected", () => {
  const decoded = decodeSshProfiles(JSON.stringify({ profiles: [fullProfile()] }));
  assert.equal(decoded?.schemaVersion, SSH_PROFILES_SCHEMA_VERSION);
});

test("string fields are trimmed, so a stray-space hand-edit doesn't become the value", () => {
  const p = decodeOne(
    JSON.stringify({
      profiles: [
        { ...fullProfile(), name: "  build box  ", destination: "  dev@host  ", remoteCwd: " /srv " },
      ],
    })
  );
  assert.equal(p.name, "build box");
  assert.equal(p.destination, "dev@host");
  assert.equal(p.remoteCwd, "/srv");
});

// ---------------------------------------------------------------------------
// SshProfilesStore — the read-before-publish ordering (#1332).
//
// A save republishes the WHOLE file, so a save built from a list that was never
// read publishes the one new connection as the entire truth and destroys every
// saved connection the human had. These are the tests that fail on the racy
// order the launcher used to run: a fire-and-forget load, an in-memory list
// seeded empty, and a rejected read collapsed into "you have none".
// ---------------------------------------------------------------------------

/** A blob holding one OTHER connection — the thing a premature save destroys. */
const OTHERS = JSON.stringify(
  store(fullProfile({ id: "p-other", name: "prod", destination: "root@prod.example.net" }))
);

/** The connection a launch is trying to save. */
const MINE = fullProfile({ id: "p-mine", name: "laptop", destination: "me@laptop.local" });

/** Records what reached the backend, and lets the test decide when the read
 *  resolves. */
function fakeIo(load: () => Promise<string | null>): SshProfileIo & { saved: string[] } {
  const saved: string[] = [];
  return {
    saved,
    load,
    save: async (contents: string) => {
      saved.push(contents);
    },
  };
}

/** The ids in a published blob, read back through the real decoder. */
const publishedIds = (raw: string) => (decodeSshProfiles(raw)?.profiles ?? []).map((p) => p.id);

/** Let every already-scheduled microtask/timer run. */
const settle = () => new Promise((r) => setTimeout(r, 0));

test("a write that beats the read does not publish a list nobody has read", async () => {
  // THE #1332 PIN. The human picks SSH, types a destination and hits Create
  // before the load has come back. A store that writes straight from its empty
  // in-memory list publishes a file containing only this connection — silently
  // deleting every other saved one, with no error anywhere because every
  // individual step succeeded.
  let release: (v: string | null) => void = () => {};
  let reads = 0;
  const io = fakeIo(() => {
    reads += 1;
    return new Promise<string | null>((res) => (release = res));
  });
  const profiles = new SshProfilesStore(io);

  const writing = profiles.write(MINE);
  await settle();
  // Positive control before the absence (CLAUDE.md, #1209): the write really is
  // in flight and really did ask for the file. Without this, "nothing reached
  // disk" is also what a write that threw on entry, or a broken fake, looks
  // like — and the interesting assertion below would be vacuous.
  assert.equal(reads, 1, "the write asked for the file");
  assert.deepEqual(io.saved, [], "…and published nothing while that read was outstanding");

  release(OTHERS); // the file finally arrives, holding the human's other connection
  assert.equal(await writing, "saved");
  assert.equal(io.saved.length, 1);
  assert.deepEqual(
    publishedIds(io.saved[0]),
    ["p-other", "p-mine"],
    "the saved connection survived the launch that raced it, beside the new one"
  );
});

test("a write is declined outright when the file could not be read", async () => {
  // "I could not look" is not "you have no saved connections". Publishing here
  // would turn one transient IPC rejection into permanent data loss — and
  // `persistSshProfile` is best-effort, so nothing downstream would ever say so.
  let reads = 0;
  const io = fakeIo(() => {
    reads += 1;
    return Promise.reject(new Error("ipc rejected"));
  });
  const profiles = new SshProfilesStore(io);
  assert.equal(await profiles.write(MINE), "declined-unread");
  assert.equal(reads, 1, "the write asked for the file");
  assert.deepEqual(io.saved, [], "a list that was never read is never published");
});

test("a failed read is retried by the next launch, not latched for the session", async () => {
  // The other direction, so the guard cannot pass by refusing everything: one
  // rejection must not disable persistence for as long as the form is open.
  let attempt = 0;
  const io = fakeIo(() => {
    attempt += 1;
    return attempt === 1 ? Promise.reject(new Error("transient")) : Promise.resolve(OTHERS);
  });
  const profiles = new SshProfilesStore(io);

  assert.equal(await profiles.write(MINE), "declined-unread");
  assert.equal(await profiles.write(MINE), "saved", "the retry lands");
  assert.equal(attempt, 2);
  assert.deepEqual(publishedIds(io.saved[0]), ["p-other", "p-mine"]);
});

test("an unreadable file reads as null, never as a form with no saved connections", async () => {
  // The caller must be able to tell "there is nothing saved" (open on New
  // connection) from "I cannot see the file" (open the same way, but never let
  // that reading reach a save). This pins the DISTINCTION only; what protects
  // the file once the human launches anyway is the write-side behaviour above.
  const bad = new SshProfilesStore(fakeIo(() => Promise.reject(new Error("nope"))));
  assert.equal(await bad.read(), null);

  const absent = new SshProfilesStore(fakeIo(() => Promise.resolve(null)));
  const first = await absent.read();
  assert.notEqual(first, null, "an ABSENT file is a complete answer, not a failure");
  assert.deepEqual(first?.profiles, []);

  // …and so is a blob that is not this schema at all: uistate.rs has already
  // renamed a corrupt file aside by the time one could get here, so seeding
  // empty (and letting a launch publish over it) is the designed first-run
  // path, not the accident this section is about.
  const mangled = new SshProfilesStore(fakeIo(() => Promise.resolve("{ not json")));
  assert.deepEqual((await mangled.read())?.profiles, []);
});

test("a write carries the file's own schemaVersion, never this build's", async () => {
  // The caller cannot supply a schemaVersion — it has no way to reach one — so a
  // form whose list came back empty cannot re-stamp a v2 file as v1 on the
  // strength of its own constructed default (`stampedVersion`, #907 NB2).
  const future = JSON.stringify({ schemaVersion: 2, profiles: [fullProfile({ id: "p-other" })] });
  const io = fakeIo(() => Promise.resolve(future));
  const profiles = new SshProfilesStore(io);
  assert.equal(await profiles.write(MINE), "saved");
  assert.equal(decodeSshProfiles(io.saved[0])?.schemaVersion, 2);
});

test("the file is read once however many launches arrive", async () => {
  let reads = 0;
  const io = fakeIo(() => {
    reads += 1;
    return Promise.resolve(OTHERS);
  });
  const profiles = new SshProfilesStore(io);
  // Concurrent, then sequential — one shared in-flight read, then the memo.
  await Promise.all([profiles.read(), profiles.read(), profiles.write(MINE)]);
  await profiles.write(fullProfile({ id: "p-third" }));
  assert.equal(reads, 1, "a burst of launches must not each re-read the blob");
});

test("read hands out a copy, so an edit to it cannot reach disk without a write", async () => {
  const io = fakeIo(() => Promise.resolve(OTHERS));
  const profiles = new SshProfilesStore(io);
  const snapshot = await profiles.read();
  assert.equal(snapshot?.profiles.length, 1, "the read really returned the stored connection");
  snapshot!.profiles[0].destination = "attacker@elsewhere";
  snapshot!.profiles[0].extraArgs.push("-o", "ProxyCommand=evil");
  snapshot!.profiles.push(MINE);

  assert.equal(await profiles.write(fullProfile({ id: "p-third" })), "saved");
  const back = decodeSshProfiles(io.saved[0])?.profiles ?? [];
  assert.deepEqual(
    back.map((p) => p.id),
    ["p-other", "p-third"],
    "the pushed entry never reached the file"
  );
  assert.equal(back[0].destination, "root@prod.example.net", "…nor the edited destination");
  assert.deepEqual(back[0].extraArgs, ["-J", "jump.example.net"], "…nor the appended argv words");
});

test("write copies the profile in, so an edit made afterwards cannot ride the next save", async () => {
  // The mirror of the test above, and the reason the copy is on BOTH sides: the
  // store outlives the call, so a caller that keeps its object could otherwise
  // change what a later write publishes without ever handing anything over.
  const io = fakeIo(() => Promise.resolve(OTHERS));
  const profiles = new SshProfilesStore(io);
  const handed = fullProfile({ id: "p-mine", destination: "me@laptop.local", extraArgs: ["-J", "gate"] });
  assert.equal(await profiles.write(handed), "saved");
  assert.equal(
    decodeSshProfiles(io.saved[0])?.profiles.find((p) => p.id === "p-mine")?.destination,
    "me@laptop.local",
    "the first save really carried the handed-over connection"
  );

  handed.destination = "attacker@elsewhere";
  handed.extraArgs.push("-o", "ProxyCommand=evil");
  assert.equal(await profiles.write(fullProfile({ id: "p-third" })), "saved");

  const mine = decodeSshProfiles(io.saved[1])?.profiles.find((p) => p.id === "p-mine");
  assert.equal(mine?.destination, "me@laptop.local", "the later edit never reached the file");
  assert.deepEqual(mine?.extraArgs, ["-J", "gate"], "…nor the appended argv words");
});

test("a save that fails says so, and leaves the newer value in memory", async () => {
  const io: SshProfileIo = {
    load: () => Promise.resolve(OTHERS),
    save: () => Promise.reject(new Error("disk full")),
  };
  const profiles = new SshProfilesStore(io);
  assert.equal(await profiles.write(MINE), "failed");
  // Not "declined-unread": the read succeeded, so the failure is the write's.
  // The store keeps the value, which is what makes the next launch re-offer it.
  assert.deepEqual(
    (await profiles.read())?.profiles.map((p) => p.id),
    ["p-other", "p-mine"]
  );
});

test("an edit updates the connection in place; only a create appends", async () => {
  // A picker that reorders itself under the human every time they launch is its
  // own small bug, and a second row carrying the same id would make the
  // persisted-pane lookup ambiguous (`dedupeById` would then pick one for them).
  const io = fakeIo(() =>
    Promise.resolve(JSON.stringify(store(fullProfile({ id: "p-a" }), fullProfile({ id: "p-b" }))))
  );
  const profiles = new SshProfilesStore(io);
  assert.equal(await profiles.write(fullProfile({ id: "p-a", name: "renamed" })), "saved");
  const back = decodeSshProfiles(io.saved[0])?.profiles ?? [];
  assert.deepEqual(
    back.map((p) => p.id),
    ["p-a", "p-b"],
    "the edited connection kept its position"
  );
  assert.equal(back[0].name, "renamed", "…and took the edit");
});

test("two overlapping writes serialize, so neither publishes over the other", async () => {
  // #1358 review N2. Two writes each publish a WHOLE blob, and the backend
  // applies them in COMPLETION order, not call order — so unserialized, the
  // blob computed first can land second and drop what the other one added.
  // Same lost update this class exists to prevent, one level up.
  //
  // `applied` records at RESOLVE time, not at call time, because that is the
  // order the file actually ends up in.
  const applied: string[] = [];
  const inFlight: Array<() => void> = [];
  const io: SshProfileIo = {
    load: () => Promise.resolve(OTHERS),
    save: (contents: string) =>
      new Promise<void>((res) =>
        inFlight.push(() => {
          applied.push(contents);
          res();
        })
      ),
  };
  const profiles = new SshProfilesStore(io);

  const first = profiles.write(fullProfile({ id: "p-a", name: "alpha" }));
  const second = profiles.write(fullProfile({ id: "p-b", name: "beta" }));
  await settle();

  // Positive control, then the pin. The control is that a write really did
  // reach the backend — without it, "only one is in flight" is also what two
  // writes that both threw on entry would look like.
  assert.equal(inFlight.length, 1, "the first write really reached the backend");
  assert.deepEqual(applied, [], "…and nothing has landed yet");
  // THE PIN: the second write has NOT published. Unserialized, both blobs are in
  // flight at once and the backend may apply them in either order.

  inFlight[0]();
  assert.equal(await first, "saved");
  await settle();
  assert.equal(inFlight.length, 2, "the second write starts only once the first has landed");

  inFlight[1]();
  assert.equal(await second, "saved");
  assert.deepEqual(
    publishedIds(applied[applied.length - 1]),
    ["p-other", "p-a", "p-b"],
    "the file the backend ends up with holds every connection, in call order"
  );
});

test("a write queued behind a FAILING one still waits for it, then still runs", async () => {
  // The other direction, so serializing cannot pass by never draining: a save
  // that rejects must neither release the next write early nor wedge the queue
  // behind it. Both halves are asserted, because each fails a different way —
  // early release is the lost update again, and a wedge is the latching this
  // class refuses on the read side.
  const outcomes: string[] = [];
  const inFlight: Array<(ok: boolean) => void> = [];
  const io: SshProfileIo = {
    load: () => Promise.resolve(OTHERS),
    save: () =>
      new Promise<void>((res, rej) =>
        inFlight.push((ok) => {
          outcomes.push(ok ? "ok" : "threw");
          ok ? res() : rej(new Error("disk full"));
        })
      ),
  };
  const profiles = new SshProfilesStore(io);
  const first = profiles.write(fullProfile({ id: "p-a" }));
  const second = profiles.write(fullProfile({ id: "p-b" }));
  await settle();

  assert.equal(inFlight.length, 1, "the first write really reached the backend");
  // The serialization pin, on the failure path specifically.
  assert.deepEqual(outcomes, [], "…and the second has not published behind its back");

  inFlight[0](false); // the first save rejects
  assert.equal(await first, "failed");
  await settle();
  assert.equal(inFlight.length, 2, "the queue drained past the failure rather than wedging");

  inFlight[1](true);
  assert.equal(await second, "saved");
  assert.deepEqual(outcomes, ["threw", "ok"], "in that order, never overlapping");
  // The failed write's value is still in memory, so the one behind it carries
  // both — the `persistTabs` best-effort contract, held across the queue.
  assert.deepEqual(
    (await profiles.read())?.profiles.map((p) => p.id),
    ["p-other", "p-a", "p-b"]
  );
});
