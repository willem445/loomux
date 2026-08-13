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
  type SshProfile,
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
