// The engine seam (#905) — `src/transport.ts`.
//
// Two properties are under test here, and they are different kinds of thing.
//
// STRUCTURAL: `@tauri-apps` is imported by exactly one module. That is CLAUDE.md
// constraint 5, which until now was a convention a reviewer had to notice being
// broken — fifteen modules had quietly grown a direct `invoke` import. It is read
// off the source tree rather than the module graph on purpose: an import that
// never executes still forks the IPC surface, and the next one will be added by
// copy-paste from a neighbour, not by anyone reading the constraint.
//
// BEHAVIORAL: the seam is swappable, so the frontend is mockable. Before this
// module, a `pty.ts` wrapper could not be exercised outside a webview at all —
// its `invoke` was bound at import to a function that throws without
// `window.__TAURI_INTERNALS__`. The tests below hand `pty.ts` a fake and assert
// the exact commands and arguments it emits, which is the half of #905 that pays
// off immediately (and the half #888 needs, since a remote transport is just
// another object satisfying this interface).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  setEngineTransport,
  tauriTransport,
  type EngineArgs,
  type EngineEvent,
  type EngineTransport,
  type UnlistenFn,
} from "../src/transport.ts";

// ---------- the fake ----------

interface Call {
  cmd: string;
  args: EngineArgs | undefined;
}

/** Records every command and holds the event subscribers, so a test can both
 *  assert what went out and drive what comes back. */
class RecordingTransport implements EngineTransport {
  readonly calls: Call[] = [];
  readonly subscribers = new Map<string, (event: EngineEvent<unknown>) => void>();
  /** cmd -> what `invoke` resolves (or, for an `Error`, rejects) with. */
  readonly replies = new Map<string, unknown>();
  unlistened = 0;

  invoke<T>(cmd: string, args?: EngineArgs): Promise<T> {
    this.calls.push({ cmd, args });
    const reply = this.replies.get(cmd);
    if (reply instanceof Error) return Promise.reject(reply);
    return Promise.resolve(reply as T);
  }

  listen<T>(event: string, handler: (event: EngineEvent<T>) => void): Promise<UnlistenFn> {
    this.subscribers.set(event, handler as (event: EngineEvent<unknown>) => void);
    return Promise.resolve(() => {
      this.unlistened += 1;
    });
  }

  /** Push a payload to whoever subscribed to `event`. */
  emit(event: string, payload: unknown): void {
    const handler = this.subscribers.get(event);
    assert.ok(handler, `nothing subscribed to "${event}"`);
    handler({ payload });
  }

  hostVersion(): Promise<string> {
    this.calls.push({ cmd: "@hostVersion", args: undefined });
    const reply = this.replies.get("@hostVersion");
    if (reply instanceof Error) return Promise.reject(reply);
    return Promise.resolve((reply as string) ?? "0.0.0-test");
  }

  pickDirectory(opts: { title?: string; defaultPath?: string }): Promise<string | null> {
    this.calls.push({ cmd: "@pickDirectory", args: opts as EngineArgs });
    return Promise.resolve((this.replies.get("@pickDirectory") as string | null) ?? null);
  }

  onCloseRequested(): Promise<void> {
    this.calls.push({ cmd: "@onCloseRequested", args: undefined });
    return Promise.resolve();
  }
}

// Installed BEFORE any test body runs. `pty.ts` resolves the transport per call,
// never at import, so a swap here reaches a module that was imported above it —
// which is itself the property that makes the seam usable at all.
const fake = new RecordingTransport();
setEngineTransport(fake);

const pty = await import("../src/pty.ts");

// ---------- behavior: the frontend is mockable now ----------

test("a pty.ts wrapper emits its command and arguments through the installed transport", async () => {
  fake.calls.length = 0;

  await pty.writePty(7, "ls\r", false);
  await pty.resizePty(7, 120, 40);
  await pty.killPty(7);
  await pty.changeDir(7, "C:\\Projects");

  assert.deepEqual(fake.calls, [
    { cmd: "write_pty", args: { id: 7, data: "ls\r", human: false } },
    { cmd: "resize_pty", args: { id: 7, cols: 120, rows: 40 } },
    { cmd: "kill_pty", args: { id: 7 } },
    { cmd: "change_dir", args: { id: 7, path: "C:\\Projects" } },
  ]);
});

test("the wrapper's own argument shaping survives the seam — spawn_pty is spread, not nested", async () => {
  // `spawnPty` spreads its options object into the command args; a seam that
  // wrapped or renamed anything would show up here as a nested `opts` key, and
  // the backend would silently receive no cols/rows.
  fake.calls.length = 0;
  fake.replies.set("spawn_pty", 42);

  const id = await pty.spawnPty({ cols: 80, rows: 24, cwd: "C:\\", shellKind: "gitbash" });

  assert.equal(id, 42, "the transport's reply must be what the wrapper returns");
  assert.deepEqual(fake.calls, [
    { cmd: "spawn_pty", args: { cols: 80, rows: 24, cwd: "C:\\", shellKind: "gitbash" } },
  ]);
});

test("a backend rejection propagates through the seam unchanged", async () => {
  // Several callers classify on the error the backend produced (resumeerror.ts's
  // structured tags, the git view's conflict text). A seam that wrapped, stringified
  // or swallowed errors would break that quietly, so it is pinned.
  const boom = new Error("no such pty");
  fake.replies.set("dir_info", boom);

  await assert.rejects(() => pty.dirInfo("C:\\nope"), (err: unknown) => err === boom);
});

test("agentCliKnobs still answers null instead of rejecting — the failure path, through the seam", async () => {
  // The wrapper's documented contract (#687): a capability lookup we couldn't make
  // is not worth failing a form over. This is the one behavior in pty.ts that
  // deliberately eats an IPC error, so it is the one most likely to be lost in a
  // refactor of how IPC is reached.
  fake.replies.set("agent_cli_knobs", new Error("backend down"));
  assert.equal(await pty.agentCliKnobs("claude"), null);

  fake.replies.set("agent_cli_knobs", { model: ["opus"] });
  assert.deepEqual(await pty.agentCliKnobs("claude"), { model: ["opus"] });
});

test("appVersion swallows a host failure and answers '' — callers treat that as 'omit the key'", async () => {
  fake.replies.set("@hostVersion", new Error("no host"));
  assert.equal(await pty.appVersion(), "");

  fake.replies.set("@hostVersion", "9.9.9");
  assert.equal(await pty.appVersion(), "9.9.9");
});

test("the pty-output router subscribes through the seam and demultiplexes by pty id", async () => {
  // The end-to-end shape of the event half: pty.ts registers ONE "pty-output"
  // subscription and fans it out. Driving it from a fake proves the seam carries
  // payloads faithfully — including the base64 decode, which is the only place
  // the frontend transforms backend bytes.
  await pty.ensureOutputRouter();
  assert.ok(fake.subscribers.has("pty-output"), "the router must subscribe through the transport");

  const seen: string[] = [];
  pty.attachOutput(3, (bytes) => seen.push(Buffer.from(bytes).toString("utf8")));

  fake.emit("pty-output", { id: 3, data: Buffer.from("hello", "utf8").toString("base64") });
  fake.emit("pty-output", { id: 4, data: Buffer.from("other pane", "utf8").toString("base64") });

  assert.deepEqual(seen, ["hello"], "pane 4's bytes must not reach pane 3's handler");

  // ...and output that arrived before a pane attached is still flushed on attach,
  // which is the invariant the router exists for.
  const late: string[] = [];
  pty.attachOutput(4, (bytes) => late.push(Buffer.from(bytes).toString("utf8")));
  assert.deepEqual(late, ["other pane"]);
});

test("setEngineTransport returns the transport it displaced, so a swap is reversible", () => {
  const second = new RecordingTransport();
  const displaced = setEngineTransport(second);
  assert.equal(displaced, fake);
  assert.equal(setEngineTransport(fake), second);
});

// ---------- structure: one module speaks Tauri ----------

const SRC = fileURLToPath(new URL("../src/", import.meta.url));

/** Every `.ts` under `src/`, AT ANY DEPTH, with `/` separators.
 *
 *  Recursive for the reason `perfpolicy.test.ts` had to learn in review: a flat
 *  read of a flat directory passes today and fails SILENTLY the day someone adds
 *  `src/<subdir>/`. A module the scan never opens is not a violation it reports,
 *  it is a file it cannot see — and this test's whole value is that it sees all
 *  of them. */
const MODULES = readdirSync(SRC, { recursive: true })
  .map((entry) => String(entry).replace(/\\/g, "/"))
  .filter((f) => f.endsWith(".ts"))
  .sort();

/** Every `@tauri-apps/*` a module actually imports.
 *
 *  Anchored at STATEMENT position (`^\s*import`) rather than matching the package
 *  string anywhere, because this file and the seam both discuss the import in
 *  prose — a scanner that reads comments would flag the argument for the rule as
 *  a violation of it. The four forms that exist: `import … from "…"` (the `[^;]`
 *  run crosses newlines, so a multi-line named import is covered), a bare
 *  side-effect import, a dynamic `import("…")` (which can sit mid-expression, so it
 *  is the one match that is not line-anchored), and `export … from "…"` — a
 *  re-export is an import wearing a different keyword, and omitting it would leave
 *  a one-line way to hand `invoke` to every module that asks for it. */
function tauriImports(source: string): string[] {
  const patterns = [
    /^[ \t]*import\b[^;]*?from\s*["'](@tauri-apps\/[^"']+)["']/gm,
    /^[ \t]*import\s*["'](@tauri-apps\/[^"']+)["']/gm,
    /\bimport\s*\(\s*["'](@tauri-apps\/[^"']+)["']\s*\)/g,
    /^[ \t]*export\b[^;]*?from\s*["'](@tauri-apps\/[^"']+)["']/gm,
  ];
  return patterns.flatMap((re) => [...source.matchAll(re)].map((m) => m[1]));
}

test("the scanner sees every way a module could reach Tauri, and ignores prose", () => {
  // Anti-vacuity. The structural test below is worth exactly what this one is: a
  // scanner that has quietly stopped matching reports an empty offender list,
  // which is indistinguishable from a clean tree.
  assert.deepEqual(
    tauriImports(
      [
        `import { invoke } from "@tauri-apps/api/core";`,
        `import {`,
        `  listen,`,
        `} from "@tauri-apps/api/event";`,
        `import * as win from "@tauri-apps/api/window";`,
        `import "@tauri-apps/plugin-dialog";`,
        `export { invoke } from "@tauri-apps/api/core";`,
        `const m = await import("@tauri-apps/api/app");`,
      ].join("\n")
    ).sort(),
    [
      "@tauri-apps/api/app",
      "@tauri-apps/api/core",
      "@tauri-apps/api/core",
      "@tauri-apps/api/event",
      "@tauri-apps/api/window",
      "@tauri-apps/plugin-dialog",
    ]
  );

  // ...and the false positive that would make the rule unstateable: this repo's
  // comments explain WHY, which means they quote the import they forbid.
  assert.deepEqual(
    tauriImports(
      [
        `// Never write \`import { invoke } from "@tauri-apps/api/core"\` here.`,
        `/** See @tauri-apps/api/event for the Event<T> this narrows. */`,
        `import { invoke } from "./transport.ts";`,
      ].join("\n")
    ),
    []
  );
});

test("`src/transport.ts` is the ONLY module that imports @tauri-apps", () => {
  // The constraint, stated over the whole tree rather than the modules that
  // happened to be swept in #905. Adding a capability by importing `invoke` where
  // you need it is the natural move and the one this must refuse: the point of a
  // single seam is that it is single.
  const offenders: string[] = [];
  for (const file of MODULES) {
    if (file === "transport.ts") continue;
    for (const pkg of tauriImports(readFileSync(SRC + file, "utf8"))) {
      offenders.push(`${file} -> ${pkg}`);
    }
  }
  assert.deepEqual(
    offenders,
    [],
    "every backend capability goes through src/transport.ts (CLAUDE.md constraint 5); " +
      `these reach past it: ${offenders.join(", ")}`
  );
});

test("the seam itself does import Tauri — the rule above is a chokepoint, not a ban", () => {
  // Guards the degenerate way to make the test above pass: delete the imports and
  // the app stops talking to its backend at all.
  const imported = tauriImports(readFileSync(SRC + "transport.ts", "utf8")).sort();
  assert.deepEqual(imported, [
    "@tauri-apps/api/app",
    "@tauri-apps/api/core",
    "@tauri-apps/api/event",
    "@tauri-apps/api/window",
    "@tauri-apps/plugin-dialog",
  ]);
});

test("the transport surface is exactly the declared capability set", () => {
  // What a remote transport (#888) would have to implement, pinned. A new Tauri
  // API reached from a feature module fails the structural test above; a new one
  // added HERE has to be added to this list too, which is the moment to ask
  // whether it is engine-side or display-side (see the interface's doc comment).
  assert.deepEqual(Object.keys(tauriTransport).sort(), [
    "hostVersion",
    "invoke",
    "listen",
    "onCloseRequested",
    "pickDirectory",
  ]);
});
