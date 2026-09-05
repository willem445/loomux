// The catalog of launchable agent CLIs plus the small persisted settings bundle
// (default agent, custom command, autopilot, recent repos). Everything lives in
// localStorage — there is no server-side config. There is no global "agent mode"
// toggle anymore: every pane declares its kind at creation via the welcome /
// pane-setup screen (#194).

import { mergeRecentDir } from "./recentdirs.ts";
// The catalog's own first-token parse (#452's single derivation), reused here
// rather than re-spelled — see `LAUNCHABLE_AGENT_PROGRAMS`.
import { programFromRestore } from "./panerestore.ts";

export interface AgentDef {
  readonly id: string;
  readonly label: string;
  /** Command line run through the default shell; "" means user-provided. */
  readonly command: string;
}

/** The catalog. `readonly` so a later feature cannot `push` a
 *  user-configured or plugin CLI onto it at runtime: that would widen the
 *  launcher and NOT the Agents tab, because `LAUNCHABLE_AGENT_PROGRAMS`
 *  below is a snapshot taken at import — and the catalog test asserts the
 *  eight names, so it would stay green through it (#2514 review round 2,
 *  premortem 2). A compile error is the loud failure; a runtime freeze
 *  would only be a late one.
 *
 *  `readonly` on the ARRAY refuses `push`; `readonly` on `AgentDef`'s own
 *  fields refuses `AGENTS[0].command = "…"`, which is the same widening one
 *  level down and which the array-level modifier alone still compiled
 *  (#2514 review round 3, premortem 2). Both are needed: the set below is a
 *  snapshot taken at import, so a row rewritten in place after it is built
 *  widens the launcher and not the tab. */
export const AGENTS: readonly AgentDef[] = [
  { id: "claude", label: "Claude Code", command: "claude" },
  { id: "copilot", label: "Copilot CLI", command: "copilot" },
  { id: "codex", label: "Codex", command: "codex" },
  { id: "opencode", label: "OpenCode", command: "opencode" },
  { id: "pi", label: "pi", command: "pi" },
  { id: "gemini", label: "Gemini CLI", command: "gemini" },
  { id: "hermes", label: "Hermes", command: "hermes" },
  { id: "ante", label: "Ante", command: "ante" },
  { id: "custom", label: "Custom…", command: "" },
];

/** The program names loomux's own launcher can start an agent pane on.
 *
 *  DERIVED from `AGENTS` rather than spelled a second time, and through
 *  `programFromRestore` rather than a private first-token parse, so a ninth
 *  CLI is one edit to the catalog above and nothing here. The `custom` row
 *  drops out on its own: its command is empty, which names no program.
 *
 *  **This set answers "is this pane an agent at all", and `agentrows.ts`'s
 *  `isAgentPane` is its one caller (#2514).** It is deliberately NOT the
 *  session-store set (`sessionCliFromCommand`, four names) and deliberately
 *  NOT "does this resolve to any program at all": the first would drop a
 *  `codex` pane out of the Agents tab, and the second would put a
 *  hand-typed `make` pane into it. A custom-command pane naming a program
 *  loomux does not recognise is not an agent as far as this window is
 *  concerned — the honest answer, and the one the Agents tab renders. */
export const LAUNCHABLE_AGENT_PROGRAMS: ReadonlySet<string> = new Set(
  AGENTS.map((a) => programFromRestore(a.command, null)).filter((p): p is string => p !== null),
);

const KEY_DEFAULT = "loomux.defaultAgent";
const KEY_CUSTOM = "loomux.customAgentCommand";
const KEY_REPOS = "loomux.recentRepos";
const KEY_AUTOPILOT = "loomux.singlePaneAutopilot";
const KEY_CHANNEL_TOOLS = "loomux.soloChannelTools";
const KEY_SUBAGENTS = "loomux.orrerixSubagents";

// One-time cleanup (#194): the removed agents-mode toggle left this key behind in
// every existing profile. Drop it on load so stale profiles don't carry it
// forever. Guarded because this module is also imported by DOM-free unit tests
// (no localStorage in Node).
try {
  localStorage.removeItem("loomux.agentMode");
} catch {
  /* no localStorage (unit-test / SSR context) — nothing to clean */
}

/** Interpret a persisted autopilot value. Default ON (#101): only an explicit
 *  "0" is off, so an absent or unrecognized value stays on. Pure so the
 *  default-ON semantics are unit-testable without a localStorage shim. */
export const autopilotFromStored = (v: string | null): boolean => v !== "0";

/** Single-pane / multi-pane "autopilot — allow all" launch toggle (#101).
 *  Defaults ON: an absent key means the user has never opted out. Persisted so
 *  the last choice is the default next time, like the other launcher prefs. */
export const getAutopilot = (): boolean => autopilotFromStored(localStorage.getItem(KEY_AUTOPILOT));
export const setAutopilot = (on: boolean): void =>
  localStorage.setItem(KEY_AUTOPILOT, on ? "1" : "0");

/** Interpret a persisted channel-tools value. Default ON, same shape as
 *  `autopilotFromStored` — see `getChannelTools`. */
export const channelToolsFromStored = (v: string | null): boolean => v !== "0";

/** Standalone channel tools toggle (#271 W3 addendum, part A2 / PR #289
 *  review round 2, N1): whether launching a claude/copilot Agent pane
 *  eagerly mints it a channel-scoped MCP token (`orch_solo_prepare`) so it's
 *  a full member from the moment it boots. Defaults ON — the addendum's
 *  stated contract is "claude/copilot = full membership at spawn," and an
 *  eagerly-minted token confers no group-scoped power (Role::Solo's
 *  two-tool surface, independently re-verified in review). Turning it OFF
 *  trades that zero-friction default for a smaller live-token surface: a
 *  pane launched with it off starts with no channel identity at all and
 *  becomes a **delivery-only** member on its first Connect gesture instead
 *  (the same adopt-on-connect path every other CLI already uses) — never a
 *  prompt at launch or mid-connect either way, just a persisted preference,
 *  like autopilot above. */
export const getChannelTools = (): boolean => channelToolsFromStored(localStorage.getItem(KEY_CHANNEL_TOOLS));
export const setChannelTools = (on: boolean): void =>
  localStorage.setItem(KEY_CHANNEL_TOOLS, on ? "1" : "0");

/** Interpret a persisted orrerix-subagents value. Default OFF — the INVERSE of
 *  `channelToolsFromStored`'s polarity, and deliberately so (#2519 C1): the
 *  other launcher toggles default ON because doing nothing should not silently
 *  downgrade a capability the user already has, while this toggle gates minting
 *  a lead pane a real group it can spawn workers into — real groups, real
 *  processes, a cap's worth of live agents. That is a gesture to opt INTO, so
 *  an absent, stale, or corrupted value reads OFF, and only the exact "1"
 *  `setSubagents(true)` writes turns it on. Pure so the default-OFF semantics
 *  are unit-testable without a localStorage shim. */
export const subagentsFromStored = (v: string | null): boolean => v === "1";

/** The "orrerix subagents" toggle (#2519): whether the launcher offers minting
 *  a pane a real lead group it can spawn `worker` children into. Persisted like
 *  the other launcher prefs, and guarded with try/catch around BOTH the read
 *  and the write because this module is also imported by DOM-free unit tests —
 *  a refused read degrades to OFF (the default), a refused write is swallowed
 *  (there is nothing to recover, same policy as `addRecentRepo`). */
export const getSubagents = (): boolean => {
  try {
    return subagentsFromStored(localStorage.getItem(KEY_SUBAGENTS));
  } catch {
    return false; // no localStorage (unit-test context) or the read was refused
  }
};
export const setSubagents = (on: boolean): void => {
  try {
    localStorage.setItem(KEY_SUBAGENTS, on ? "1" : "0");
  } catch {
    /* write failed (quota / security) — nothing to recover, same as before */
  }
};

/** The agent preselected in the launcher; updated on every launch. */
export function getDefaultAgent(): AgentDef {
  const id = localStorage.getItem(KEY_DEFAULT);
  return AGENTS.find((a) => a.id === id) ?? AGENTS[0];
}
export const setDefaultAgent = (id: string): void => localStorage.setItem(KEY_DEFAULT, id);

export const getCustomCommand = (): string => localStorage.getItem(KEY_CUSTOM) ?? "";
export const setCustomCommand = (cmd: string): void => localStorage.setItem(KEY_CUSTOM, cmd);

export function getRecentRepos(): string[] {
  return readRepoList() ?? [];
}

/** The stored recents, or `null` when the list could not be READ at all — a
 *  distinct state from "empty", so a write can decline rather than overwrite
 *  what it never saw (#2010; the single-tenant cousin of the BoardPrefsStore
 *  rule). A blob that parses to garbage is NOT this case: the data itself is
 *  gone and there is nothing to preserve, so that reads as an empty list. */
function readRepoList(): string[] | null {
  let raw: string | null;
  try {
    raw = localStorage.getItem(KEY_REPOS);
  } catch {
    return null;
  }
  try {
    const v = raw === null ? [] : JSON.parse(raw);
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function addRecentRepo(path: string): void {
  const next = mergeRecentDir(readRepoList(), path);
  if (next === null) return; // the read failed — decline the write, never wipe (#2010)
  try {
    localStorage.setItem(KEY_REPOS, JSON.stringify(next));
  } catch {
    /* write failed (quota / security) — nothing to recover, same as before */
  }
}
