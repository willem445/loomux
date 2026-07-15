// The catalog of launchable agent CLIs plus the small persisted settings bundle
// (default agent, custom command, autopilot, recent repos). Everything lives in
// localStorage — there is no server-side config. There is no global "agent mode"
// toggle anymore: every pane declares its kind at creation via the welcome /
// pane-setup screen (#194).

export interface AgentDef {
  id: string;
  label: string;
  /** Command line run through the default shell; "" means user-provided. */
  command: string;
}

export const AGENTS: AgentDef[] = [
  { id: "claude", label: "Claude Code", command: "claude" },
  { id: "copilot", label: "Copilot CLI", command: "copilot" },
  { id: "codex", label: "Codex", command: "codex" },
  { id: "opencode", label: "OpenCode", command: "opencode" },
  { id: "gemini", label: "Gemini CLI", command: "gemini" },
  { id: "custom", label: "Custom…", command: "" },
];

const KEY_DEFAULT = "loomux.defaultAgent";
const KEY_CUSTOM = "loomux.customAgentCommand";
const KEY_REPOS = "loomux.recentRepos";
const KEY_AUTOPILOT = "loomux.singlePaneAutopilot";
const KEY_CHANNEL_TOOLS = "loomux.soloChannelTools";
const MAX_RECENT_REPOS = 8;

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

/** The agent preselected in the launcher; updated on every launch. */
export function getDefaultAgent(): AgentDef {
  const id = localStorage.getItem(KEY_DEFAULT);
  return AGENTS.find((a) => a.id === id) ?? AGENTS[0];
}
export const setDefaultAgent = (id: string): void => localStorage.setItem(KEY_DEFAULT, id);

export const getCustomCommand = (): string => localStorage.getItem(KEY_CUSTOM) ?? "";
export const setCustomCommand = (cmd: string): void => localStorage.setItem(KEY_CUSTOM, cmd);

export function getRecentRepos(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY_REPOS) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function addRecentRepo(path: string): void {
  const list = [path, ...getRecentRepos().filter((p) => p !== path)].slice(0, MAX_RECENT_REPOS);
  localStorage.setItem(KEY_REPOS, JSON.stringify(list));
}
