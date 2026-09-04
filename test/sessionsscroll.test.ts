// #2334: the left panel's session browser has exactly ONE scroll box, and everything it LISTS is
// inside it.
//
// This is a test about CSS and DOM wiring, which is unusual here (the repo validates DOM wiring by
// hand) and deliberate, for the same reason `hiddenrule.test.ts` is: the defect lived in the one
// place this view's unit tests could never look. `sessionfilter.ts`, `orchlist.ts` and
// `sessionmeta.ts` were all correct and all tested; `sessions.ts` rendered exactly the rows they
// decided on. The Orchestration tab still could not be scrolled, because `.orch-list` was a
// `flex: none` SIBLING of the one element carrying `overflow-y` (#1563 slice B), inside a
// fixed-height flex column. A `flex: none` item does not shrink, so the orchestrations group took
// its whole content height out of the column and the rows above the fold had nowhere to scroll to.
// Mine looked fine only because it renders no orchestration rows at all, so that sibling was empty
// there.
//
// WHAT IS PINNED, AND WHY IN THIS SHAPE. The property is structural, not cosmetic: a list mounted
// as a sibling of the scroll box is unreachable content, whatever it is called. So both halves
// default-deny rather than enumerate:
//
//   * The panel BODY takes a closed set of children - the fixed-height chrome, plus one scroll
//     box. A new list appended straight into the body fails here, and the only way to pass is to
//     mount it in the scroll box or to argue a new chrome row onto the allow-list below. That is
//     the direction that matters: the scroll box itself may take anything.
//   * Across every class this view mounts, exactly ONE declares a scrolling overflow. A second
//     inner scroll box - the other way to split this list in two - fails there.
//
// Neither half reads a binding's NAME to decide anything (CLAUDE.md's source-scanning convention):
// the class literal is resolved from the source and the verdict is taken from the class, so a
// rename of `orchEl` or `scrollEl` moves nothing. The allow-list carries a reason per row and is
// required to be exact, so a chrome row that is deleted or renamed fails loudly rather than
// silently watching nothing.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const SRC = readFileSync(new URL("../src/sessions.ts", import.meta.url), "utf8");
// Comments out first: this file's own prose quotes selectors and declarations, and so does
// `sessions.ts`. A parser that read them would be reading the argument instead of the code.
const CSS = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8").replace(
  /\/\*[\s\S]*?\*\//g,
  ""
);

/** Every class literal this view puts on an element it creates. The vocabulary of the whole
 *  browser, not just the panel body's children - the overflow half below default-denies over
 *  all of it. */
function mountedClasses(src: string): Set<string> {
  const out = new Set<string>();
  for (const m of src.matchAll(/\.className\s*=\s*"([^"$]+)"/g)) {
    for (const c of m[1].split(/\s+/)) if (c) out.add(c);
  }
  return out;
}

/** The class an identifier is carrying at source position `before` - the nearest PRECEDING
 *  `x.className = "..."`, which is how a reader resolves it. `sessions.ts` reuses the local name
 *  `head` for two different elements, so "nearest preceding" is the only correct rule here; the
 *  first or the last match would each be wrong for one of them. */
function classAt(src: string, ident: string, before: number): string {
  const re = new RegExp(`\\b${ident.replace(".", "\\.")}\\.className\\s*=\\s*"([^"$]+)"`, "g");
  let found = "";
  for (const m of src.matchAll(re)) {
    if (m.index === undefined || m.index >= before) break;
    found = m[1];
  }
  assert.notEqual(found, "", `no className assignment found for \`${ident}\``);
  return found;
}

/** The classes appended to `receiver` by its `.append(...)` call, in order. */
function appendedTo(src: string, receiver: string): string[] {
  const re = new RegExp(`${receiver.replace(".", "\\.")}\\.append\\(([^)]*)\\)`, "g");
  const all = [...src.matchAll(re)];
  assert.equal(all.length, 1, `expected exactly one \`${receiver}.append(...)\` in sessions.ts`);
  const call = all[0];
  return call[1]
    .split(",")
    .map((a) => a.trim())
    .filter(Boolean)
    .map((a) => classAt(src, a, call.index ?? src.length));
}

/** The panel body's permitted children, other than the scroll box. Each is FIXED-HEIGHT chrome:
 *  it renders the same box however much the browser has to show, so it cannot hide content by
 *  sitting outside the scroller. A row here is a claim about that, not a note about what exists
 *  today - adding one means making that claim for the new element. */
const FIXED_CHROME: Record<string, string> = {
  "sessions-head": "the heading and its refresh button - one row, always",
  "sessions-search": "the filter input - one line, always",
  "sessions-mode": "the Mine/Orchestration tablist - two tabs, always",
};

const SCROLL_BOX = "sessions-scroll";

test("the session browser mounts one scroll box, and nothing else that can grow", () => {
  const children = appendedTo(SRC, "this.el");
  // Positive control: the parse really read the append, and really resolved the classes. An empty
  // or short list would satisfy every set assertion below for the wrong reason.
  assert.ok(children.length >= 4, `resolved only ${children.length} children of the panel body`);
  assert.equal(new Set(children).size, children.length, "a class is mounted twice in the body");

  const allowed = new Set([...Object.keys(FIXED_CHROME), SCROLL_BOX]);
  const stray = children.filter((c) => !allowed.has(c));
  assert.deepEqual(
    stray,
    [],
    `mounted directly in the panel body, outside the scroll box: ${stray.join(", ")}. ` +
      "The body is a fixed-height flex column, so a list put here grows with its content and is " +
      "then clipped, exactly as .orch-list was before #2334. Append it to the scroll box, or " +
      "argue it onto FIXED_CHROME as fixed-height chrome."
  );
  // Exact, not a subset: a chrome row that was deleted or renamed must fail here rather than sit
  // in the allow-list watching nothing.
  const missing = [...allowed].filter((c) => !children.includes(c));
  assert.deepEqual(missing, [], `allow-listed but no longer mounted: ${missing.join(", ")}`);
});

test("every list the browser renders is inside that scroll box", () => {
  const inScroller = appendedTo(SRC, "this.scrollEl");
  // The three things this browser LISTS. Each grows without bound: as many orchestration groups,
  // as many sessions, and a footnote that appears under them. The set is required, not merely
  // permitted - moving one out is the #2334 defect, and it must fail here as well as above.
  for (const c of ["orch-list", "sessions-list", "sessions-delegate-toggle"]) {
    assert.ok(inScroller.includes(c), `.${c} is not inside .${SCROLL_BOX}`);
  }
});

/** Every rule in the stylesheet that makes its element a scroll container on the block axis.
 *  `overflow: hidden` and `text-overflow` are not that and are not matched; `scroll` is, since it
 *  splits a list in two exactly as `auto` does. */
function scrollingSelectors(css: string): string[] {
  const out: string[] = [];
  for (const block of css.matchAll(/([^{}]*)\{([^{}]*)\}/g)) {
    for (const decl of block[2].split(";")) {
      const m = /^\s*overflow(-y)?\s*:\s*([a-z]+)/.exec(decl);
      if (m && (m[2] === "auto" || m[2] === "scroll")) out.push(block[1].trim());
    }
  }
  return out;
}

test("exactly one class this view mounts is a scroll container", () => {
  const mounted = mountedClasses(SRC);
  // Positive control on both instruments before the absence assertion below: the source parse
  // found this view's vocabulary, and the CSS parse found scroll containers SOMEWHERE (the app
  // has several - .agents-list, #hintbar), so a zero here would be a broken parser rather than a
  // clean stylesheet.
  assert.ok(mounted.size >= 10, `only ${mounted.size} classes parsed out of sessions.ts`);
  const scrolling = scrollingSelectors(CSS);
  assert.ok(scrolling.length >= 3, `only ${scrolling.length} scrolling rules parsed out of the CSS`);

  // A selector is "this view's" when its rightmost compound names a class the view mounts. That
  // is name-independent: it decides from the mounted set, not from a spelling this test knows.
  const ours = scrolling.filter((sel) =>
    sel
      .split(",")
      .some((s) =>
        [...mounted].some((c) => new RegExp(`\\.${c}(?![\\w-])[^\\s>+~]*$`).test(s.trim()))
      )
  );
  assert.deepEqual(
    [...new Set(ours)].sort(),
    [`.${SCROLL_BOX}`],
    "the session browser must have exactly one scroll box: a second one splits the panel into " +
      "two independently scrolling halves, which is the #2334 defect in its other shape"
  );
});

test("the scroll box actually fills the column it scrolls in", () => {
  const rule = new RegExp(`\\.${SCROLL_BOX}\\s*\\{([^}]*)\\}`).exec(CSS);
  assert.ok(rule, `.${SCROLL_BOX} has no rule in styles.css`);
  const decls = rule[1];
  // Without a grow of at least 1 the box is content-height inside `.leftpanel-body`, and then it
  // overflows the column instead of scrolling in it - green on every assertion above, and the
  // bug still on screen.
  assert.match(decls, /flex\s*:\s*1\b/, `.${SCROLL_BOX} must grow to fill the panel body`);
  assert.match(decls, /overflow-y\s*:\s*auto/, `.${SCROLL_BOX} must be the scroll container`);
});
