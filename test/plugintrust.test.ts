// The pane-tab label / picker text is a real DOM surface, so — same as
// hiddenrule.test.ts for the `[hidden]` cascade invariant — this pins the
// invariant by reading the actual source rather than simulating a DOM
// (CLAUDE.md: "DOM wiring is validated by hand"). What's being pinned: a
// plugin manifest's `name` is UNTRUSTED third-party text (#360, flagged
// explicitly in the slice B/C reviews and this slice's own brief — "the
// pane tab label" named by name), and every surface that renders it —
// PluginPaneView's inline status, the welcome form's plugin picker, the
// pane tab label a plugin pane opens under — must treat it as DATA
// (`textContent`/`.value`) and never interpolate it into markup
// (`innerHTML`, a template string handed to `insertAdjacentHTML`, …).
//
// On base (before #360 Slice D), none of this exists — pluginpaneview.ts
// isn't a file, the plugin picker isn't in launcher.ts, and pane.ts has no
// plugin branch — so every test below fails for the reason the file/code
// is simply absent, not a false pass. `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (rel: string): string => readFileSync(new URL(`../src/${rel}`, import.meta.url), "utf8");

/** Every `.innerHTML` (assignment OR read) and `insertAdjacentHTML` call in a
 *  source file — the two ways this codebase's OWN modules build markup from a
 *  string (see pane.ts's icon constants, which assign literal SVG strings to
 *  `.innerHTML` — a real, legitimate use elsewhere in this file for TRUSTED,
 *  hardcoded content). Untrusted text must never reach either. */
function htmlSinks(src: string): string[] {
  const hits: string[] = [];
  for (const m of src.matchAll(/\.innerHTML\s*=|\.innerHTML(?!\s*=)|insertAdjacentHTML\s*\(/g)) {
    hits.push(m[0]);
  }
  return hits;
}

test("pluginpaneview.ts — the plugin pane's own surface — never touches innerHTML at all", () => {
  // The whole file's rendered content is the status/error line (the manifest's
  // untrusted `displayName`) and nothing else; the strongest, most legible
  // invariant is that the file contains NO html-sink call whatsoever, not "the
  // ones involving displayName specifically" — a future addition to this file
  // that reaches for innerHTML for ANY reason should fail here and be
  // reconsidered, not be waved through because it wasn't touching this string.
  const src = read("pluginpaneview.ts");
  assert.deepEqual(htmlSinks(src), [], "pluginpaneview.ts must never build markup from a string");
});

test("pluginpaneview.ts renders the untrusted manifest name via textContent, not string concatenation into markup", () => {
  const src = read("pluginpaneview.ts");
  assert.match(
    src,
    /statusEl\.textContent\s*=\s*`Opening \$\{this\.manifest\.displayName\}…`/,
    "the 'opening' status line must assign displayName through textContent"
  );
  assert.match(
    src,
    /statusEl\.textContent\s*=\s*`Couldn't open "\$\{this\.manifest\.displayName\}"/,
    "the error state must also assign displayName through textContent, never interpolate it into markup"
  );
});

test("launcher.ts's plugin picker renders each installed plugin's untrusted name via option.textContent", () => {
  const src = read("launcher.ts");
  assert.match(
    src,
    /opt\.textContent\s*=\s*m\.name;/,
    "the plugin <option> label must be assigned via textContent, exactly like every other <select> populated in this form (see the shared `select()` helper and ModelPicker.setOptions, which do the same)"
  );
  // And the html-sink check applies here too, scoped to the plugin picker's own
  // construction — a regression that started building the option's label as
  // markup (e.g. to bold the id) would have to walk past this.
  const pluginPickerSection = src.slice(src.indexOf("private paintPlugins"));
  assert.deepEqual(
    htmlSinks(pluginPickerSection),
    [],
    "the plugin picker must never build markup from a manifest's untrusted `name`"
  );
});

test("a plugin pane's tab label goes through Pane.setName → titleEl.textContent, same as every other kind", () => {
  // pane.ts doesn't special-case the plugin kind's title at all — startContent
  // calls `this.setName(opts.name)` unconditionally for every content kind, and
  // setName's own implementation is the ONE place a title ever reaches the DOM
  // (titleEl.textContent = name). Proving the plugin path routes through the
  // same call, rather than writing its own titleEl assignment, is what rules
  // out a second, unescaped title-rendering path existing alongside it.
  const src = read("pane.ts");
  assert.match(src, /titleEl\.textContent\s*=\s*name;/, "setName must assign the title via textContent");
  const pluginBranch = src.slice(src.indexOf('if (opts.kind === "plugin")'), src.indexOf('if (opts.kind === "workflow")'));
  assert.ok(pluginBranch.length > 0, "startContent's plugin branch must exist");
  assert.equal(
    htmlSinks(pluginBranch).length,
    0,
    "the plugin branch of buildContentView must not build its own markup for the title"
  );
});
