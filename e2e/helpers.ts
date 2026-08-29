// Shared page-interaction helpers for the E2E PoC specs. There are no
// `data-testid` hooks in the frontend yet (see doc/design/e2e-testing.md), so
// selectors are structural: label text inside `.dlg-field` wrappers, and
// class names read straight out of src/launcher.ts, src/pane.ts, src/grid.ts.
import { type Page } from "@playwright/test";

/** The most recently opened "New pane" launcher form.
 *
 *  `:visible` is load-bearing, not tidiness. Every empty pane carries a
 *  welcome form, and a session restored from `tabs.json` brings up one per
 *  tab — all of them in the DOM, only the active tab's on screen. Without the
 *  filter `.last()` picks the LAST tab's hidden form, and the first
 *  interaction then waits for a form that will never become visible. With a
 *  single tab (every spec before the soak lane) the filter changes nothing. */
function latestWelcomeForm(page: Page) {
  return page.locator(".welcome-form:visible").last();
}

/** Fills out and submits the launcher form to turn a welcome pane into a
 *  plain shell (terminal) pane — never an agent/orchestrator kind, so this
 *  never spawns a real agent CLI.
 *
 *  `shell` picks the shell-kind option (`src/launcher.ts`'s Shell field);
 *  omitted, the form's own default (PowerShell) stands, which is what every
 *  spec before the soak lane used. A spec that has to read a child's OUTPUT
 *  should pass `"cmd"`: PSReadLine redraws the input line as you type, so a
 *  marker typed into PowerShell arrives in the output stream interleaved
 *  with re-rendered prefixes, while `cmd.exe` echoes plainly. */
export async function createTerminalPane(
  page: Page,
  opts: { name: string; repo?: string; shell?: "powershell" | "cmd" | "gitbash" }
): Promise<void> {
  const form = latestWelcomeForm(page);

  await form.locator('.dlg-field:has(.dlg-label:has-text("Kind")) select').selectOption("terminal");
  if (opts.shell) {
    await form
      .locator('.dlg-field:has(.dlg-label:has-text("Shell")) select')
      .selectOption(opts.shell);
  }
  await form.locator('.dlg-field:has(.dlg-label:has-text("Pane name")) input').fill(opts.name);
  if (opts.repo) {
    await form.locator('.dlg-field:has(.dlg-label:has-text("Repository")) input').fill(opts.repo);
  }
  await form.locator(".dlg-btn.primary").click();

  // Every pane (even an unconfigured welcome one) already has a `.pane-term`
  // div in the DOM (src/pane.ts), so its count can't signal "submit
  // finished" — wait for the launcher form itself to be torn down instead.
  await form.waitFor({ state: "detached", timeout: 15_000 });
}

/** Fills out and submits the launcher form to turn a welcome pane into a WORKFLOW pane over
 *  `repo` (#222, restructured by #880). A content pane — it spawns no process at all, let alone
 *  an agent CLI, so it is safe for automated E2E by construction. */
export async function createWorkflowPane(
  page: Page,
  opts: { name: string; repo: string }
): Promise<void> {
  const form = latestWelcomeForm(page);

  await form.locator('.dlg-field:has(.dlg-label:has-text("Kind")) select').selectOption("workflow");
  await form.locator('.dlg-field:has(.dlg-label:has-text("Pane name")) input').fill(opts.name);
  // The repo field's LABEL changes per kind (launcher.ts `applyKind`): "Repository" for a
  // workflow pane, "Folder" for files/editor. Match on the placeholder-independent label text
  // this kind actually renders.
  await form.locator('.dlg-field:has(.dlg-label:has-text("Repository")) input').fill(opts.repo);
  await form.locator(".dlg-btn.primary").click();

  await form.waitFor({ state: "detached", timeout: 15_000 });
}

/** The `.pane` ancestor of a pane whose header title matches `name`. */
export function paneByName(page: Page, name: string) {
  return page.locator(".pane", { has: page.locator(".pane-title", { hasText: name }) });
}
