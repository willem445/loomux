// Synthetic corpus builder for the soak/liveness lane (#1603, plan #1600 §3
// Phase 4.1). Writes — into the fixture's own throwaway data dir, before the
// app is spawned — the shape of install every one of the beta4/5/6 hang
// reports was made against: many orchestration groups, each with a large
// audit log, and a large CLI session store behind them.
//
// Two hard rules this file exists to respect:
//
// 1. **No live agent CLI** (CLAUDE.md constraint 3). Nothing here spawns
//    anything. A "session" is a directory of the text the real CLI would have
//    written; a "group" is the on-disk state a real orchestration leaves
//    behind. The app under test reads them exactly as it reads a real
//    install's, and that read is the point: `recorded_orchestrations` walks
//    every group dir and streams every audit log (#1592), and the polled
//    `orch_group_summary`/`orch_group_usage` reads take registry locks per
//    group-bound tab.
// 2. **Never the operator's real session store.** The Claude half of that
//    store (`~/.claude/projects`) has NO production redirect —
//    `claude_projects_root()` is `dirs::home_dir()` plus a thread-local seam
//    only reachable from Rust — so writing hundreds of synthetic sessions
//    there would mean writing into a human's actual transcript directory.
//    This builder therefore uses the COPILOT half, which does have one:
//    `copilot_session_state_root()` honours `COPILOT_HOME`
//    (crates/loomux-engine/src/sessions.rs), so the whole store can live
//    inside the same temp dir the fixture already deletes on teardown. The
//    groups get `agent_cli: "copilot"` for the same reason — it is what makes
//    the boot listing's `resumable` check actually enumerate the synthetic
//    store instead of skipping it.
import * as fs from "node:fs";
import * as path from "node:path";

/** Environment variables a seeded corpus needs the app to be launched with. */
export type CorpusEnv = Record<string, string>;

export interface CorpusSpec {
  /** Orchestration group directories under `<dataDir>/orchestration/`. */
  groups: number;
  /** Audit lines written into each group's `audit.jsonl`. */
  auditLinesPerGroup: number;
  /** Synthetic copilot session directories under the redirected COPILOT_HOME. */
  sessions: number;
  /** Tabs in `tabs.json`, each bound to one group. `src/tabbar.ts`'s 4 s
   *  `pollStatus` iterates the BOUND tabs and issues two `orch_*` invokes for
   *  each, so this is the knob that sets how much polling the soak generates.
   *  Clamped to `groups`. */
  groupBoundTabs: number;
  /** An existing directory to name as each group's repo. */
  repo: string;
}

/** What `buildCorpus` actually wrote. The spec asserts against this rather
 *  than assuming, so a corpus that silently failed to generate cannot pass as
 *  a soak against a large one. */
export interface CorpusReport {
  groupIds: string[];
  sessionIds: string[];
  auditBytes: number;
  env: CorpusEnv;
}

/** `GroupId`'s alphabet is `[A-Za-z0-9_-]` with no leading `-`
 *  (crates/loomux-engine/src/groupid.rs), so zero-padded decimal is always in
 *  it and no id needs escaping or a hash. */
function pad(n: number, width = 4): string {
  return String(n).padStart(width, "0");
}

export function groupIdFor(i: number): string {
  return `soakgrp-${pad(i)}`;
}

export function sessionIdFor(i: number): string {
  return `soaksess-${pad(i, 5)}`;
}

/** One `audit.jsonl` line in the exact shape `AuditEntry` deserializes
 *  (`{ts_ms, actor, action, detail}`). `records_from_audit` streams this file
 *  line by line on every listing — that stream is the cost #1592 was measured
 *  on, so what matters here is only that the file is big. */
function auditLine(tsMs: number, action: string, detail: unknown): string {
  return JSON.stringify({ ts_ms: tsMs, actor: "orrerix", action, detail }) + "\n";
}

function writeJson(file: string, value: unknown): void {
  fs.writeFileSync(file, JSON.stringify(value, null, 2), "utf8");
}

/**
 * Writes the corpus into `dataDir` and returns both what it wrote and the
 * environment the app must be launched with to see it.
 *
 * Synchronous on purpose: it runs inside the fixture's pre-spawn window, where
 * there is nothing to interleave with, and a sync write of a few thousand
 * small files beats the promise churn of doing it any other way.
 */
export function buildCorpus(dataDir: string, spec: CorpusSpec): CorpusReport {
  const orchRoot = path.join(dataDir, "orchestration");
  fs.mkdirSync(orchRoot, { recursive: true });

  // The redirected copilot store lives INSIDE the fixture's own data dir, so
  // the fixture's existing teardown (`rmSafely(dataDir)`) removes it too and
  // this builder owns no cleanup of its own.
  const copilotHome = path.join(dataDir, "fake-copilot-home");
  const sessionRoot = path.join(copilotHome, "session-state");
  fs.mkdirSync(sessionRoot, { recursive: true });

  const repoForYaml = spec.repo.replace(/\\/g, "/");
  const sessionIds: string[] = [];
  for (let i = 0; i < spec.sessions; i++) {
    const id = sessionIdFor(i);
    const dir = path.join(sessionRoot, id);
    fs.mkdirSync(dir, { recursive: true });
    // Flat, single-level YAML: `yaml_field` is a `key:` line scanner, not a
    // YAML parser (crates/loomux-engine/src/sessions.rs), so anything nested
    // here would simply not be read.
    fs.writeFileSync(
      path.join(dir, "workspace.yaml"),
      `id: ${id}\nname: soak session ${i}\ncwd: ${repoForYaml}\n`,
      "utf8"
    );
    sessionIds.push(id);
  }

  const groupIds: string[] = [];
  let auditBytes = 0;
  const baseTs = Date.now() - spec.groups * 1000;
  const sessionAt = (i: number): string | null =>
    sessionIds.length === 0 ? null : sessionIds[i % sessionIds.length];

  for (let g = 0; g < spec.groups; g++) {
    const groupId = groupIdFor(g);
    const dir = path.join(orchRoot, groupId);
    fs.mkdirSync(dir, { recursive: true });

    // `load_group_file` requires only `repo`; every guardrail falls back to a
    // default. Two are set deliberately rather than left to one:
    // `agent_cli: "copilot"` so the listing's resumable check enumerates the
    // synthetic store above, and `auto_ops: false` — which DEFAULTS TO TRUE on
    // a missing key — because auto-ops would start driving `gh`/`git`
    // subprocesses against a repo these fake groups do not own. The poll paths
    // this soak measures are the frontend's, and they run either way.
    writeJson(path.join(dir, "group.json"), {
      group_id: groupId,
      repo: spec.repo,
      created_ms: baseTs + g * 1000,
      guardrails: { max_agents: 4, agent_cli: "copilot", blocks: {}, auto_ops: false },
    });

    // A roster whose orchestrator row names a session that really exists in
    // the store above — otherwise `resumable` short-circuits and the listing
    // never touches the session corpus at all.
    writeJson(path.join(dir, "agents.json"), [
      {
        id: `o-${pad(g)}`,
        role: "orchestrator",
        block: "orchestrator",
        name: `orchestrator-${g}`,
        session: sessionAt(g),
        cwd: spec.repo,
        status: "idle",
        updated_ms: baseTs + g * 1000,
      },
      {
        id: `w-${pad(g)}`,
        role: "worker",
        block: "worker",
        name: `worker-${g}`,
        session: sessionAt(g + 1),
        cwd: spec.repo,
        status: "idle",
        updated_ms: baseTs + g * 1000,
      },
    ]);

    writeJson(path.join(dir, "state.json"), {});
    writeJson(path.join(dir, "tasks.json"), [
      { id: "t-1", title: `soak task ${g}`, status: "queued", updated_ms: baseTs },
    ]);

    const chunks: string[] = [
      auditLine(baseTs, "agent-spawn", {
        agent: `o-${pad(g)}`,
        role: "orchestrator",
        session: sessionAt(g),
        name: `orchestrator-${g}`,
        block: "orchestrator",
        cwd: spec.repo,
      }),
    ];
    for (let n = 0; n < spec.auditLinesPerGroup; n++) {
      chunks.push(auditLine(baseTs + n, "noise", { n, group: groupId }));
    }
    const audit = chunks.join("");
    fs.writeFileSync(path.join(dir, "audit.jsonl"), audit, "utf8");
    auditBytes += Buffer.byteLength(audit);

    groupIds.push(groupId);
  }

  // tabs.json is an opaque blob to the backend; `src/tabstore.ts` owns the
  // schema. `groupIds` is what `src/main.ts` feeds to `tabs.bindGroup`, and a
  // bound tab is exactly what `src/tabbar.ts`'s 4 s `pollStatus` iterates — so
  // this array IS the poll load. `layout: null` restores each tab to a single
  // empty welcome pane, which starts no process; `restorePref: "restore"`
  // keeps the first-run restore prompt off the screen.
  const boundTabs = Math.min(spec.groupBoundTabs, groupIds.length);
  writeJson(path.join(dataDir, "tabs.json"), {
    tabs: Array.from({ length: boundTabs }, (_unused, i) => ({
      name: `soak-${i}`,
      color: null,
      groupId: groupIds[i],
      groupIds: [groupIds[i]],
      layout: null,
      docked: [],
    })),
    activeIndex: 0,
    restorePref: "restore",
    schemaVersion: 2,
  });

  return { groupIds, sessionIds, auditBytes, env: { COPILOT_HOME: copilotHome } };
}
