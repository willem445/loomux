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
  focus(): void;
  /** Open the in-editor find/replace UI, if the implementation has one. */
  openFind(): void;
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
  focus(): void {
    this.ta.focus();
  }
  openFind(): void {
    // No custom find panel; the WebView2 native find (or the project search
    // panel) covers it. Intentionally a no-op.
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
        cm.language.syntaxHighlighting(cm.language.defaultHighlightStyle, { fallback: true }),
        cm.search.highlightSelectionMatches(),
        cm.view.keymap.of([
          cm.commands.indentWithTab,
          ...cm.commands.defaultKeymap,
          ...cm.commands.historyKeymap,
          ...cm.search.searchKeymap,
        ]),
        this.langCompartment.of(lang ?? []),
        this.roCompartment.of(cm.state.EditorState.readOnly.of(false)),
        cm.view.EditorView.updateListener.of((u: import("@codemirror/view").ViewUpdate) => {
          if (u.docChanged) this.changeCb();
        }),
        theme(cm),
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

  focus(): void {
    this.view.focus();
  }

  openFind(): void {
    this.cm.search.openSearchPanel(this.view);
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
}

/** A compact dark theme aligned with loomux's Tokyo-Night-ish `:root` palette
 *  (accent #7aa2f7). Uses `transparent` backgrounds so the overlay's own
 *  surface colour shows through and the editor blends into the pane chrome. */
function theme(cm: CmModules): import("@codemirror/state").Extension {
  const MONO = '"Cascadia Mono", Consolas, ui-monospace, monospace';
  return cm.view.EditorView.theme(
    {
      "&": { color: "var(--text)", backgroundColor: "transparent", height: "100%" },
      ".cm-content": { fontFamily: MONO, caretColor: "var(--accent)" },
      ".cm-gutters": { backgroundColor: "transparent", color: "var(--text-dim)", border: "none" },
      ".cm-activeLine": { backgroundColor: "rgba(122,162,247,0.06)" },
      ".cm-activeLineGutter": { backgroundColor: "rgba(122,162,247,0.10)" },
      "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": {
        backgroundColor: "rgba(122,162,247,0.22)",
      },
      ".cm-cursor": { borderLeftColor: "var(--accent)" },
      ".cm-panels": { backgroundColor: "var(--panel-2)", color: "var(--text)" },
      ".cm-panels input, .cm-panels button": { fontFamily: MONO },
    },
    { dark: true }
  );
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
    const [state, view, commands, language, search, lang] = await Promise.all([
      import("@codemirror/state"),
      import("@codemirror/view"),
      import("@codemirror/commands"),
      import("@codemirror/language"),
      import("@codemirror/search"),
      languageFor(filename),
    ]);
    const cm: CmModules = { state, view, commands, language, search };
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
      focus: () => ta.focus(),
      openFind: () => ta.openFind(),
      dispose: () => ta.dispose(),
    };
  }
}
