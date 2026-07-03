// Unified-diff parser + DOM renderer for the git view's preview panel.
// Renders line rows with old/new number gutters and add/del tinting. All
// file content goes through textContent — never innerHTML.

interface DiffRow {
  kind: "file" | "hunk" | "add" | "del" | "ctx" | "meta";
  text: string;
  oldNo?: number;
  newNo?: number;
}

function parseDiff(raw: string): DiffRow[] {
  const rows: DiffRow[] = [];
  let oldNo = 0;
  let newNo = 0;

  for (const line of raw.split("\n")) {
    if (line.startsWith("diff --git ")) {
      rows.push({ kind: "file", text: line.slice("diff --git ".length) });
    } else if (line.startsWith("@@")) {
      const m = /^@@+ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
      if (m) {
        oldNo = parseInt(m[1], 10);
        newNo = parseInt(m[2], 10);
      }
      rows.push({ kind: "hunk", text: line });
    } else if (line.startsWith("+++") || line.startsWith("---")) {
      // File headers; skip (the panel header already names the file).
    } else if (
      line.startsWith("index ") ||
      line.startsWith("new file") ||
      line.startsWith("deleted file") ||
      line.startsWith("old mode") ||
      line.startsWith("new mode") ||
      line.startsWith("similarity") ||
      line.startsWith("rename ") ||
      line.startsWith("copy ") ||
      line.startsWith("Binary files")
    ) {
      rows.push({ kind: "meta", text: line });
    } else if (line.startsWith("+")) {
      rows.push({ kind: "add", text: line.slice(1), newNo: newNo++ });
    } else if (line.startsWith("-")) {
      rows.push({ kind: "del", text: line.slice(1), oldNo: oldNo++ });
    } else if (line.startsWith("\\")) {
      rows.push({ kind: "meta", text: line.slice(1).trim() }); // "No newline…"
    } else if (line.length > 0 || rows.length > 0) {
      // Context line (leading space stripped; blank context lines kept).
      rows.push({ kind: "ctx", text: line.slice(1), oldNo: oldNo++, newNo: newNo++ });
    }
  }
  // Drop a trailing phantom context row from the final newline split.
  const last = rows[rows.length - 1];
  if (last?.kind === "ctx" && last.text === "" && !raw.endsWith("\n ")) rows.pop();
  return rows;
}

function makeRow(row: DiffRow): HTMLElement {
  const el = document.createElement("div");
  el.className = `diff-line ${row.kind}`;
  if (row.kind === "file" || row.kind === "hunk" || row.kind === "meta") {
    const txt = document.createElement("span");
    txt.className = "txt";
    txt.textContent = row.text;
    el.appendChild(txt);
    return el;
  }
  const oldLn = document.createElement("span");
  oldLn.className = "ln";
  oldLn.textContent = row.oldNo !== undefined ? String(row.oldNo) : "";
  const newLn = document.createElement("span");
  newLn.className = "ln";
  newLn.textContent = row.newNo !== undefined ? String(row.newNo) : "";
  const sign = document.createElement("span");
  sign.className = "sign";
  sign.textContent = row.kind === "add" ? "+" : row.kind === "del" ? "-" : " ";
  const txt = document.createElement("span");
  txt.className = "txt";
  txt.textContent = row.text;
  el.append(oldLn, newLn, sign, txt);
  return el;
}

/** Render `raw` unified diff into `container` (replacing its contents).
 *  Long diffs are capped at `maxLines` rows with a show-more button. */
export function renderDiff(raw: string, container: HTMLElement, maxLines = 4000): void {
  container.replaceChildren();
  if (!raw.trim()) {
    const empty = document.createElement("div");
    empty.className = "git-empty";
    empty.textContent = "No changes.";
    container.appendChild(empty);
    return;
  }

  const rows = parseDiff(raw);
  const frag = document.createDocumentFragment();
  const renderSlice = (from: number, to: number): void => {
    for (let i = from; i < Math.min(to, rows.length); i++) frag.appendChild(makeRow(rows[i]));
  };

  renderSlice(0, maxLines);
  container.appendChild(frag);

  if (rows.length > maxLines) {
    const more = document.createElement("button");
    more.className = "diff-more";
    more.textContent = `Show ${rows.length - maxLines} more lines`;
    more.addEventListener("click", () => {
      renderSlice(maxLines, rows.length);
      more.replaceWith(frag);
    });
    container.appendChild(more);
  }
}
