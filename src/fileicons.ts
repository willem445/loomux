// Pure filename → icon mapping for the file tree (issue #174). Two halves, both
// DOM-free and node:test-covered: `iconCategory(filename)` classifies a name
// into one of a dozen buckets, and `iconSvg(category)` returns an inline 16×16
// SVG string. The SVGs use `stroke`/`fill="currentColor"` so they inherit the
// pane's text colour and theme for free — no icon font, sprite sheet, or icon
// package (keeps the feature "lightweight", and matches the FOLDER_ICON pattern
// already in pane.ts). Classification never throws: an unknown name always
// resolves to the generic "file" bucket.

export type IconCategory =
  | "folder"
  | "folder-open"
  | "code"
  | "rust"
  | "python"
  | "json"
  | "markdown"
  | "web"
  | "style"
  | "shell"
  | "image"
  | "config"
  | "lock"
  | "text"
  | "file";

/** Extension (lower-cased, no dot) → category. */
const EXT_CATEGORY: Record<string, IconCategory> = {
  js: "code", mjs: "code", cjs: "code", jsx: "code",
  ts: "code", mts: "code", cts: "code", tsx: "code",
  rs: "rust",
  py: "python", pyi: "python",
  json: "json", jsonc: "json",
  md: "markdown", markdown: "markdown", mdx: "markdown",
  html: "web", htm: "web", xml: "web", svg: "web", vue: "web",
  css: "style", scss: "style", sass: "style", less: "style",
  sh: "shell", bash: "shell", zsh: "shell", fish: "shell",
  ps1: "shell", psm1: "shell", bat: "shell", cmd: "shell",
  png: "image", jpg: "image", jpeg: "image", gif: "image",
  webp: "image", bmp: "image", ico: "image", avif: "image",
  toml: "config", yaml: "config", yml: "config", ini: "config",
  cfg: "config", conf: "config", env: "config",
  lock: "lock",
  txt: "text", log: "text", csv: "text", rst: "text",
};

/** Whole-filename (lower-cased) → category, for extensionless or special files
 *  where the base name carries the meaning. */
const NAME_CATEGORY: Record<string, IconCategory> = {
  dockerfile: "config",
  makefile: "config",
  ".gitignore": "config",
  ".gitattributes": "config",
  ".editorconfig": "config",
  ".npmrc": "config",
  ".env": "config",
  "cargo.lock": "lock",
  "package-lock.json": "lock",
  "yarn.lock": "lock",
  "pnpm-lock.yaml": "lock",
  "license": "text",
  "readme": "markdown",
};

/** Classify a filename. Directories are handled by the caller (pass the dir's
 *  open/closed state to `iconSvg` directly); this is for files. Robust to
 *  uppercase, multi-dot (`a.test.ts` → its final `ts`), dotfiles (`.gitignore`),
 *  and no extension — always returns a category, never throws. */
export function iconCategory(filename: string): IconCategory {
  const lower = filename.toLowerCase();
  if (NAME_CATEGORY[lower]) return NAME_CATEGORY[lower];
  // Strip a trailing "readme"/"license" with any extension (README.md handled
  // by ext; README with none handled above; README.txt → markdown-ish is fine
  // as text via ext). Fall through to extension logic.
  const dot = lower.lastIndexOf(".");
  // No dot, or a leading-dot dotfile with no further extension (".gitignore"
  // was caught above; an unknown dotfile like ".foorc" has dot at 0) → treat the
  // segment after the dot as the ext.
  if (dot <= 0) {
    // ".foorc" → ext "foorc" (unknown → file); "Makefile" (dot < 0) → file.
    if (dot === 0) {
      const ext = lower.slice(1);
      return EXT_CATEGORY[ext] ?? "file";
    }
    return "file";
  }
  const ext = lower.slice(dot + 1);
  return EXT_CATEGORY[ext] ?? "file";
}

// ---------- inline SVGs ----------
//
// Each is a 16×16 glyph. Kept deliberately simple and monochrome; distinct
// enough to scan a tree at a glance. currentColor means they follow the theme.

const svg = (inner: string): string =>
  `<svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round">${inner}</svg>`;

// A plain page outline reused as the base for text-like glyphs.
const PAGE = `<path d="M4 1.6h5l3 3v9.8H4z"/><path d="M9 1.6v3h3"/>`;

const ICONS: Record<IconCategory, string> = {
  folder: svg(`<path d="M1.7 4c0-.6.5-1 1-1h3l1.3 1.4h6c.6 0 1 .4 1 1v6.2c0 .6-.4 1-1 1H2.7c-.5 0-1-.4-1-1z"/>`),
  "folder-open": svg(`<path d="M1.7 4c0-.6.5-1 1-1h3l1.3 1.4h6c.6 0 1 .4 1 1v1H1.7z"/><path d="M1.7 6.4h12.6l-1.2 6c-.1.5-.5.8-1 .8H2.9c-.5 0-.9-.3-1-.8z"/>`),
  code: svg(`${PAGE}<path d="M6.4 8.4 5 9.8l1.4 1.4"/><path d="M9.6 8.4 11 9.8l-1.4 1.4"/>`),
  rust: svg(`${PAGE}<circle cx="8" cy="9.6" r="2.2"/><path d="M8 7.4v-1M8 12.8v-1M5.8 9.6h-1M11.2 9.6h-1"/>`),
  python: svg(`${PAGE}<path d="M6.2 8.2h3.2c.5 0 .8.3.8.8v1c0 .5-.3.8-.8.8H7.8c-.5 0-.8.3-.8.8v1"/><circle cx="7" cy="7.6" r=".01"/>`),
  json: svg(`${PAGE}<path d="M7 7.4c-1 0-1 .8-1 1.2s0 1.2-1 1.2c1 0 1 .8 1 1.2s0 1.2 1 1.2"/><path d="M9 7.4c1 0 1 .8 1 1.2s0 1.2 1 1.2c-1 0-1 .8-1 1.2s0 1.2-1 1.2"/>`),
  markdown: svg(`${PAGE}<path d="M5.4 11.4V8.2l1.4 1.6 1.4-1.6v3.2"/><path d="M10.4 8.4v3M9.4 10.4l1 1 1-1"/>`),
  web: svg(`${PAGE}<circle cx="8" cy="9.6" r="2.4"/><path d="M5.6 9.6h4.8M8 7.2v4.8M6.2 8.1c.7.6 2.9.6 3.6 0M6.2 11.1c.7-.6 2.9-.6 3.6 0"/>`),
  style: svg(`${PAGE}<path d="M5.4 8h5.2M5.4 9.8h5.2M5.4 11.6h3"/>`),
  shell: svg(`<rect x="1.6" y="2.6" width="12.8" height="10.8" rx="1.2"/><path d="M4 6l2 2-2 2M7.8 10.4h4"/>`),
  image: svg(`<rect x="1.6" y="2.6" width="12.8" height="10.8" rx="1.2"/><circle cx="5.4" cy="6.4" r="1.2"/><path d="M2.4 12.4 6 9l2.4 2 2.2-2.6 3 3.6"/>`),
  config: svg(`<circle cx="8" cy="8" r="2.1"/><path d="M8 1.8v2M8 12.2v2M1.8 8h2M12.2 8h2M3.5 3.5l1.4 1.4M11.1 11.1l1.4 1.4M12.5 3.5l-1.4 1.4M4.9 11.1l-1.4 1.4"/>`),
  lock: svg(`<rect x="3.4" y="7" width="9.2" height="7" rx="1.1"/><path d="M5.4 7V5.2a2.6 2.6 0 0 1 5.2 0V7"/>`),
  text: svg(`${PAGE}<path d="M5.6 8h4.8M5.6 9.8h4.8M5.6 11.6h3"/>`),
  file: svg(PAGE),
};

/** Inline SVG string for a category. */
export function iconSvg(category: IconCategory): string {
  return ICONS[category];
}

/** Convenience: the SVG for a filename in one call. */
export function fileIconSvg(filename: string): string {
  return iconSvg(iconCategory(filename));
}

/** SVG for a directory row, picking the open or closed folder glyph. */
export function folderIconSvg(open: boolean): string {
  return open ? ICONS["folder-open"] : ICONS["folder"];
}
