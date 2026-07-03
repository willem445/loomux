// Agent mode: the catalog of launchable agent CLIs plus the small persisted
// settings bundle (mode toggle, default agent, custom command, recent repos).
// Everything lives in localStorage — there is no server-side config.

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

const KEY_MODE = "loomux.agentMode";
const KEY_DEFAULT = "loomux.defaultAgent";
const KEY_CUSTOM = "loomux.customAgentCommand";
const KEY_REPOS = "loomux.recentRepos";
const MAX_RECENT_REPOS = 8;

export const getAgentMode = (): boolean => localStorage.getItem(KEY_MODE) === "1";
export const setAgentMode = (on: boolean): void =>
  localStorage.setItem(KEY_MODE, on ? "1" : "0");

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
