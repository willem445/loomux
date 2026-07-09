// The swappable code-editor widget behind the file-editor overlay (issue #174).
//
// Everything the FileEditView needs from an editor is the `EditorWidget`
// interface below. Two implementations satisfy it:
//   * CodeMirror 6 (primary) — line numbers, per-language syntax highlighting,
//     undo/redo, large-file virtualization, and an in-file find/replace panel
//     (`@codemirror/search`) that directly covers the issue's "search-and-
//     replace" ask *inside* the open file. It is **lazy-loaded** via dynamic
//     `import()`, so its ~40-package dependency only enters the bundle chunk
//     that loads on first overlay open, not the main bundle.
//   * a plain `<textarea>` fallback — zero-dependency, always works, and used
//     automatically if the CM6 chunk fails to load. Documented in the PR as the
//     "swap to zero-dep" option if the human prefers not to vendor CM6.
//
// The interface is the seam that keeps that choice cheap: the FileEditView never
// imports CodeMirror directly.
//
// The in-file find/replace uses a CUSTOM CodeMirror search panel (a compact
// two-row inline-icon widget, VS-Code-inspired but our own styling) built via
// `search({createPanel})`; its logic (regex build, match count) is the pure
// `findwidget` module, its DOM is here, and it drives CM6's own search state and
// commands so replace edits the buffer through the normal dirty→Save flow.

import {
  buildHighlightRegex,
  buildSearchRegex,
  matchInfo,
  formatMatchCount,
  type FindFlags,
} from "./findwidget";

/** Minimal contract the overlay depends on. Keeping it this small is what makes
 *  the CM6-vs-textarea decision a one-line swap. */
export interface EditorWidget {
  /** The root element to place in the layout. */
  readonly el: HTMLElement;
  /** Current buffer text. */
  getValue(): string;
  /** Replace the whole buffer, optionally re-picking the language from a
   *  filename (extension-driven). Resets undo history — it's a new document. */
  setValue(doc: string, filename?: string): void;
  /** Register the change callback (fired on every edit). One listener is enough
   *  for the dirty-dot tracking the view does. */
  onChange(cb: () => void): void;
  /** Make the editor read-only (e.g. while a save is in flight) or editable. */
  setReadOnly(ro: boolean): void;
  /** Move the caret to (1-based) line/col and scroll it into view — used to jump
   *  to a search hit. Clamped to the document; a no-op if out of range. */
  reveal(line: number, col?: number): void;
  focus(): void;
  /** Open the in-editor find/replace UI, if the implementation has one. */
  openFind(): void;
  /** Highlight every occurrence of `query` in the open document (used to mirror
   *  the project search's matches inside the editor). Empty query clears the
   *  highlight. `caseInsensitive`/`wholeWord` mirror the project-search options. */
  setHighlightQuery(query: string, caseInsensitive: boolean, wholeWord: boolean): void;
  dispose(): void;
}

// ---------- textarea fallback ----------

/** A dependency-free editor: a styled `<textarea>`. No line numbers or syntax
 *  highlighting, but full editing + native browser find. Always available. */
class TextareaEditor implements EditorWidget {
  readonly el: HTMLElement;
  private ta: HTMLTextAreaElement;
  private changeCb: () => void = () => {};

  constructor(doc: string) {
    this.el = document.createElement("div");
    this.el.className = "fileedit-ta-wrap";
    this.ta = document.createElement("textarea");
    this.ta.className = "fileedit-textarea";
    this.ta.spellcheck = false;
    this.ta.wrap = "off";
    this.ta.value = doc;
    // Keep keystrokes from bubbling into the terminal / app shortcuts.
    this.ta.addEventListener("keydown", (e) => e.stopPropagation());
    this.ta.addEventListener("input", () => this.changeCb());
    this.el.appendChild(this.ta);
  }

  getValue(): string {
    return this.ta.value;
  }
  setValue(doc: string): void {
    this.ta.value = doc;
  }
  onChange(cb: () => void): void {
    this.changeCb = cb;
  }
  setReadOnly(ro: boolean): void {
    this.ta.readOnly = ro;
  }
  reveal(line: number, col = 1): void {
    const lines = this.ta.value.split("\n");
    const target = Math.max(1, Math.min(line, lines.length));
    let offset = 0;
    for (let i = 0; i < target - 1; i++) offset += lines[i].length + 1;
    offset += Math.max(0, col - 1);
    this.ta.focus();
    this.ta.setSelectionRange(offset, offset);
    // Best-effort scroll-to-caret for the fallback: approximate from line height.
    const lh = parseFloat(getComputedStyle(this.ta).lineHeight) || 16;
    this.ta.scrollTop = Math.max(0, (target - 1) * lh - this.ta.clientHeight / 2);
  }
  focus(): void {
    this.ta.focus();
  }
  openFind(): void {
    // A plain <textarea> has no match-highlighting/find widget. Degradation: the
    // project search box (which highlights hit files + jumps to the line) is the
    // find affordance; native browser find also works. Intentionally a no-op.
  }
  setHighlightQuery(): void {
    // No in-textarea occurrence highlighting is possible without a rich editor;
    // the fallback relies on the project search + jump-to-line instead.
  }
  dispose(): void {
    this.el.remove();
  }
}

// ---------- CodeMirror 6 ----------

/** Map a filename to a lazily-imported CodeMirror language extension, or null
 *  for plain text. Each import is its own chunk, so only the languages actually
 *  opened are ever fetched. */
async function languageFor(filename: string): Promise<import("@codemirror/state").Extension | null> {
  const ext = filename.toLowerCase().split(".").pop() ?? "";
  switch (ext) {
    case "js": case "mjs": case "cjs": case "jsx":
      return (await import("@codemirror/lang-javascript")).javascript({ jsx: true });
    case "ts": case "mts": case "cts":
      return (await import("@codemirror/lang-javascript")).javascript({ typescript: true });
    case "tsx":
      return (await import("@codemirror/lang-javascript")).javascript({ typescript: true, jsx: true });
    case "json": case "jsonc":
      return (await import("@codemirror/lang-json")).json();
    case "py": case "pyi":
      return (await import("@codemirror/lang-python")).python();
    case "rs":
      return (await import("@codemirror/lang-rust")).rust();
    case "html": case "htm": case "vue":
      return (await import("@codemirror/lang-html")).html();
    case "css": case "scss": case "sass": case "less":
      return (await import("@codemirror/lang-css")).css();
    case "md": case "markdown": case "mdx":
      return (await import("@codemirror/lang-markdown")).markdown();
    default:
      return null;
  }
}

/** A CodeMirror-backed editor. Built by `createEditor`; the constructor takes the
 *  already-imported modules so all the dynamic `import()`s live in one place. */
class CodeMirrorEditor implements EditorWidget {
  readonly el: HTMLElement;
  private changeCb: () => void = () => {};
  // Loosely typed to avoid leaking CM types across the interface; the concrete
  // objects come from the dynamic import in `createEditor`.
  private readonly view: import("@codemirror/view").EditorView;
  private readonly cm: CmModules;
  private readonly langCompartment: import("@codemirror/state").Compartment;
  private readonly roCompartment: import("@codemirror/state").Compartment;
  /** Holds the always-on workspace-query highlighter (a ViewPlugin), or [] when
   *  no query is active. Reconfigured imperatively by `setHighlightQuery`. */
  private readonly wsHighlight: import("@codemirror/state").Compartment;

  constructor(
    parent: HTMLElement,
    doc: string,
    lang: import("@codemirror/state").Extension | null,
    cm: CmModules
  ) {
    this.el = parent;
    this.cm = cm;
    this.langCompartment = new cm.state.Compartment();
    this.roCompartment = new cm.state.Compartment();
    this.wsHighlight = new cm.state.Compartment();
    const state = cm.state.EditorState.create({
      doc,
      extensions: [
        cm.view.lineNumbers(),
        cm.view.highlightActiveLineGutter(),
        cm.view.highlightActiveLine(),
        cm.view.drawSelection(),
        cm.view.EditorView.lineWrapping,
        cm.commands.history(),
        cm.language.indentOnInput(),
        cm.language.bracketMatching(),
        cm.search.highlightSelectionMatches(),
        // Float the find widget at the top (VS-Code-like) with our own compact
        // two-row panel (createPanel); positioned as an overlay in styles.css.
        cm.search.search({ top: true, createPanel: (view) => this.buildFindPanel(view) }),
        cm.view.keymap.of([
          cm.commands.indentWithTab,
          ...cm.commands.defaultKeymap,
          ...cm.commands.historyKeymap,
          ...cm.search.searchKeymap,
        ]),
        this.langCompartment.of(lang ?? []),
        this.roCompartment.of(cm.state.EditorState.readOnly.of(false)),
        this.wsHighlight.of([]),
        cm.view.EditorView.updateListener.of((u: import("@codemirror/view").ViewUpdate) => {
          if (u.docChanged) this.changeCb();
        }),
        // Standard, widely-recognized syntax colours (One Dark), then our own
        // font + sizing on top (One Dark doesn't set a font family).
        cm.oneDark.oneDark,
        editorChrome(cm),
      ],
    });
    this.view = new cm.view.EditorView({ state, parent });
  }

  getValue(): string {
    return this.view.state.doc.toString();
  }

  setValue(doc: string, filename?: string): void {
    this.view.dispatch({
      changes: { from: 0, to: this.view.state.doc.length, insert: doc },
    });
    if (filename !== undefined) {
      void languageFor(filename).then((lang) => {
        this.view.dispatch({
          effects: this.langCompartment.reconfigure(lang ?? []),
        });
      });
    }
  }

  onChange(cb: () => void): void {
    this.changeCb = cb;
  }

  setReadOnly(ro: boolean): void {
    this.view.dispatch({
      effects: this.roCompartment.reconfigure(this.cm.state.EditorState.readOnly.of(ro)),
    });
  }

  reveal(line: number, col = 1): void {
    const doc = this.view.state.doc;
    const l = Math.max(1, Math.min(line, doc.lines));
    const lineObj = doc.line(l);
    const pos = Math.min(lineObj.from + Math.max(0, col - 1), lineObj.to);
    this.view.dispatch({
      selection: { anchor: pos },
      effects: this.cm.view.EditorView.scrollIntoView(pos, { y: "center" }),
    });
  }

  focus(): void {
    this.view.focus();
  }

  openFind(): void {
    this.cm.search.openSearchPanel(this.view);
  }

  setHighlightQuery(query: string, caseInsensitive: boolean, wholeWord: boolean): void {
    // Why not `setSearchQuery`: CM's search extension only paints match
    // decorations while its find PANEL is open, so with the panel closed the
    // workspace query lit up nothing (the reported bug). Instead install our own
    // always-on ViewPlugin whose decorations come from a MatchDecorator — that
    // decorations facet renders whenever the plugin is in the config, panel or
    // not, and is viewport-bounded (MatchDecorator only scans the visible range).
    const re = buildHighlightRegex(query, caseInsensitive, wholeWord);
    this.view.dispatch({
      effects: [
        this.wsHighlight.reconfigure(re ? this.highlightPlugin(re) : []),
        // Also seed CM6's own search state so the in-file Find widget opens
        // pre-filled with the workspace query. This does NOT drive the visible
        // highlight (the ViewPlugin above does) — it only feeds the panel.
        this.cm.search.setSearchQuery.of(
          new this.cm.search.SearchQuery({
            search: query,
            caseSensitive: !caseInsensitive,
            wholeWord,
            literal: true,
          })
        ),
      ],
    });
  }

  /** A ViewPlugin that decorates every match of `re` in the viewport with the
   *  `.cm-wsMatch` class, kept current on edits and scrolls. */
  private highlightPlugin(re: RegExp): import("@codemirror/state").Extension {
    const cm = this.cm;
    const matcher = new cm.view.MatchDecorator({
      regexp: re,
      decoration: cm.view.Decoration.mark({ class: "cm-wsMatch" }),
    });
    return cm.view.ViewPlugin.fromClass(
      class {
        decorations: import("@codemirror/view").DecorationSet;
        constructor(view: import("@codemirror/view").EditorView) {
          this.decorations = matcher.createDeco(view);
        }
        update(u: import("@codemirror/view").ViewUpdate) {
          this.decorations = matcher.updateDeco(u, this.decorations);
        }
      },
      { decorations: (v) => v.decorations }
    );
  }

  /** Custom CodeMirror search panel: a compact two-row inline-icon find/replace
   *  widget (VS-Code-inspired shape, our own styling + glyphs). It drives CM6's
   *  native search state/commands, so in-file replace edits the buffer through
   *  the normal dirty→Save path; the pure `findwidget` module owns the regex +
   *  "n of m" logic. Returned to `search({createPanel})`. */
  private buildFindPanel(view: import("@codemirror/view").EditorView): import("@codemirror/view").Panel {
    const cm = this.cm;
    const mk = (tag: string, cls: string, text?: string): HTMLElement => {
      const e = document.createElement(tag);
      e.className = cls;
      if (text !== undefined) e.textContent = text;
      return e;
    };
    const dom = mk("div", "fileedit-find-widget");
    // Keep the widget's own typing/shortcuts out of the terminal + app handlers.
    dom.addEventListener("keydown", (e) => e.stopPropagation());

    // ---- row 1: find ----
    const row1 = mk("div", "fileedit-find-row");
    const findField = document.createElement("input");
    findField.className = "fileedit-find-input";
    findField.placeholder = "Find";
    findField.spellcheck = false;
    const caseTog = mk("button", "fileedit-find-toggle", "Aa") as HTMLButtonElement;
    caseTog.title = "Match case";
    const wordTog = mk("button", "fileedit-find-toggle", "W") as HTMLButtonElement;
    wordTog.title = "Whole word";
    const reTog = mk("button", "fileedit-find-toggle", ".*") as HTMLButtonElement;
    reTog.title = "Regular expression";
    const countEl = mk("span", "fileedit-find-count", "");
    const prevBtn = mk("button", "fileedit-find-icon", "↑") as HTMLButtonElement;
    prevBtn.title = "Previous match (Shift+Enter)";
    const nextBtn = mk("button", "fileedit-find-icon", "↓") as HTMLButtonElement;
    nextBtn.title = "Next match (Enter)";
    const closeBtn = mk("button", "fileedit-find-icon", "✕") as HTMLButtonElement;
    closeBtn.title = "Close (Esc)";
    row1.append(findField, caseTog, wordTog, reTog, countEl, prevBtn, nextBtn, closeBtn);

    // ---- row 2: replace ----
    const row2 = mk("div", "fileedit-find-row");
    const replaceField = document.createElement("input");
    replaceField.className = "fileedit-find-input";
    replaceField.placeholder = "Replace";
    replaceField.spellcheck = false;
    const replBtn = mk("button", "fileedit-find-icon", "⇥") as HTMLButtonElement;
    replBtn.title = "Replace";
    const replAllBtn = mk("button", "fileedit-find-icon", "⇟") as HTMLButtonElement;
    replAllBtn.title = "Replace all";
    row2.append(replaceField, replBtn, replAllBtn);

    dom.append(row1, row2);

    const flags = (): FindFlags => ({
      caseSensitive: caseTog.classList.contains("on"),
      wholeWord: wordTog.classList.contains("on"),
      regexp: reTog.classList.contains("on"),
    });
    // Push the widget's query into CM6's search state so its commands act on it.
    const commit = (): void => {
      const f = flags();
      view.dispatch({
        effects: cm.search.setSearchQuery.of(
          new cm.search.SearchQuery({
            search: findField.value,
            replace: replaceField.value,
            caseSensitive: f.caseSensitive,
            wholeWord: f.wholeWord,
            regexp: f.regexp,
            literal: true,
          })
        ),
      });
      refreshCount();
    };
    const refreshCount = (): void => {
      const re = buildSearchRegex(findField.value, flags());
      const info = matchInfo(view.state.doc.toString(), re, view.state.selection.main.from);
      countEl.textContent = formatMatchCount(findField.value, info);
    };

    findField.addEventListener("input", commit);
    replaceField.addEventListener("input", commit);
    for (const t of [caseTog, wordTog, reTog]) {
      t.addEventListener("click", () => {
        t.classList.toggle("on");
        commit();
        findField.focus();
      });
    }
    findField.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (e.shiftKey) cm.search.findPrevious(view);
        else cm.search.findNext(view);
        refreshCount();
      } else if (e.key === "Escape") {
        e.preventDefault();
        cm.search.closeSearchPanel(view);
      }
    });
    replaceField.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        cm.search.replaceNext(view);
        refreshCount();
      } else if (e.key === "Escape") {
        e.preventDefault();
        cm.search.closeSearchPanel(view);
      }
    });
    prevBtn.addEventListener("click", () => { cm.search.findPrevious(view); refreshCount(); });
    nextBtn.addEventListener("click", () => { cm.search.findNext(view); refreshCount(); });
    closeBtn.addEventListener("click", () => cm.search.closeSearchPanel(view));
    replBtn.addEventListener("click", () => { cm.search.replaceNext(view); refreshCount(); });
    replAllBtn.addEventListener("click", () => { cm.search.replaceAll(view); refreshCount(); });

    return {
      dom,
      top: true,
      mount: () => {
        // Prefill from the active query (seeded on open-from-search), so the
        // widget shows the workspace query and its toggles.
        const q = cm.search.getSearchQuery(view.state);
        if (q.search) {
          findField.value = q.search;
          caseTog.classList.toggle("on", q.caseSensitive);
          wordTog.classList.toggle("on", q.wholeWord);
          reTog.classList.toggle("on", q.regexp);
        }
        findField.focus();
        findField.select();
        refreshCount();
      },
      update: (u: import("@codemirror/view").ViewUpdate) => {
        if (
          u.docChanged ||
          u.selectionSet ||
          u.transactions.some((t) => t.effects.some((e) => e.is(cm.search.setSearchQuery)))
        ) {
          refreshCount();
        }
      },
    };
  }

  dispose(): void {
    this.view.destroy();
  }
}

/** The bag of CodeMirror modules the editor needs, resolved once by dynamic
 *  import so the rest of the class stays synchronous. */
interface CmModules {
  state: typeof import("@codemirror/state");
  view: typeof import("@codemirror/view");
  commands: typeof import("@codemirror/commands");
  language: typeof import("@codemirror/language");
  search: typeof import("@codemirror/search");
  oneDark: typeof import("@codemirror/theme-one-dark");
}

/** A modern IDE monospace stack, tried in order. No bundled font files — these
 *  are the fonts developers already have installed (or the OS ships). */
const EDITOR_FONT =
  '"Cascadia Code", "JetBrains Mono", "Fira Code", "Cascadia Mono", "SF Mono", Menlo, Consolas, ui-monospace, monospace';

/** Font + sizing layered over One Dark (which sets the colours but no font). */
function editorChrome(cm: CmModules): import("@codemirror/state").Extension {
  return cm.view.EditorView.theme({
    "&": { height: "100%" },
    ".cm-scroller": { fontFamily: EDITOR_FONT, fontSize: "13px", lineHeight: "1.55" },
  });
}

/** Build the best available editor into `parent`. Tries CodeMirror 6; if any of
 *  its chunks fail to load, transparently falls back to the textarea editor so
 *  the overlay always works. */
export async function createEditor(
  parent: HTMLElement,
  doc: string,
  filename: string
): Promise<EditorWidget> {
  try {
    const [state, view, commands, language, search, oneDark, lang] = await Promise.all([
      import("@codemirror/state"),
      import("@codemirror/view"),
      import("@codemirror/commands"),
      import("@codemirror/language"),
      import("@codemirror/search"),
      import("@codemirror/theme-one-dark"),
      languageFor(filename),
    ]);
    const cm: CmModules = { state, view, commands, language, search, oneDark };
    return new CodeMirrorEditor(parent, doc, lang, cm);
  } catch (err) {
    // CM6 unavailable (chunk load failure): degrade, don't break the feature.
    console.warn("fileedit: CodeMirror failed to load, using textarea fallback", err);
    const ta = new TextareaEditor(doc);
    parent.appendChild(ta.el);
    // Adapt: the textarea manages its own inner element, so hand back a widget
    // whose `el` is the parent we were given (the view treats it uniformly).
    return {
      el: parent,
      getValue: () => ta.getValue(),
      setValue: (d) => ta.setValue(d),
      onChange: (cb) => ta.onChange(cb),
      setReadOnly: (ro) => ta.setReadOnly(ro),
      reveal: (line, col) => ta.reveal(line, col),
      focus: () => ta.focus(),
      openFind: () => ta.openFind(),
      setHighlightQuery: () => ta.setHighlightQuery(),
      dispose: () => ta.dispose(),
    };
  }
}
