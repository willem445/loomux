// "Open in editor": the persisted editor-command setting plus the helpers the
// pane header button and its Alt+E shortcut use to open a workspace directory
// in the user's external editor.
//
// The editor command lives in localStorage (like the rest of loomux's
// settings — see agents.ts). The actual spawn happens in the Rust
// `open_in_editor` command, which launches it detached with the directory as
// a single argv element (no shell string), so nothing here needs to quote or
// escape paths.

import { invoke } from "@tauri-apps/api/core";
import { showToast } from "./toast";
import { overlayState } from "./overlaystate";

const KEY_EDITOR = "loomux.editorCommand";

/** The configured editor command (e.g. `code`, `zed`, or a full exe path). */
export const getEditorCommand = (): string =>
  (localStorage.getItem(KEY_EDITOR) ?? "").trim();

export const setEditorCommand = (cmd: string): void =>
  localStorage.setItem(KEY_EDITOR, cmd.trim());

/** Open `dir` in the configured editor. If none is configured, prompt for one
 *  first; on backend failure, surface a toast. Returns true if a launch was
 *  attempted. */
export async function openInEditor(dir: string | null): Promise<boolean> {
  if (!dir) {
    showToast("This pane has no folder to open yet.");
    return false;
  }
  let editor = getEditorCommand();
  if (!editor) {
    const chosen = await editorConfigDialog();
    if (!chosen) return false; // user cancelled
    editor = chosen;
  }
  try {
    await invoke("open_in_editor", { editor, dir });
    return true;
  } catch (err) {
    showToast(String(err));
    return false;
  }
}

/** Open the configuration modal so the user can set/change the editor command.
 *  Resolves to the saved command, or null if cancelled. An independent
 *  hand-rolled dialog (not routed through modal.ts — see #391's own
 *  reconnaissance), so it registers with the shared overlay registry
 *  (overlaystate.ts) itself rather than inheriting modal.ts's. */
export function editorConfigDialog(): Promise<string | null> {
  return new Promise((resolve) => {
    const closeOverlaySlot = overlayState.open();
    const overlay = document.createElement("div");
    overlay.className = "launcher-overlay visible";

    const dlg = document.createElement("div");
    dlg.className = "agent-dialog editor-dialog";

    const title = document.createElement("h2");
    title.textContent = "Editor command";

    const field = document.createElement("div");
    field.className = "dlg-field";
    const label = document.createElement("div");
    label.className = "dlg-label";
    label.textContent = "Command or path";
    const input = document.createElement("input");
    input.className = "dlg-input";
    input.placeholder = "code";
    input.value = getEditorCommand();
    const hint = document.createElement("div");
    hint.className = "dlg-hint";
    hint.textContent =
      "e.g. code (VS Code), zed, subl, or a full path to the editor. " +
      "The workspace folder is passed as the argument.";
    field.append(label, input, hint);

    const actions = document.createElement("div");
    actions.className = "dlg-actions";
    const cancel = document.createElement("button");
    cancel.className = "dlg-btn";
    cancel.textContent = "Cancel";
    const save = document.createElement("button");
    save.className = "dlg-btn primary";
    save.textContent = "Save";
    actions.append(cancel, save);

    dlg.append(title, field, actions);
    overlay.appendChild(dlg);
    document.body.appendChild(overlay);
    input.focus();
    input.select();

    let settled = false;
    const close = (result: string | null): void => {
      if (settled) return;
      settled = true;
      closeOverlaySlot();
      overlay.remove();
      resolve(result);
    };
    const commit = (): void => {
      const val = input.value.trim();
      if (!val) {
        input.focus();
        return;
      }
      setEditorCommand(val);
      close(val);
    };

    save.addEventListener("click", commit);
    cancel.addEventListener("click", () => close(null));
    // Clicking the dimmed backdrop cancels; clicks inside the dialog don't.
    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close(null);
    });
    input.addEventListener("keydown", (e) => {
      e.stopPropagation(); // don't let app shortcuts fire behind the modal
      if (e.key === "Enter") commit();
      if (e.key === "Escape") close(null);
    });
  });
}
