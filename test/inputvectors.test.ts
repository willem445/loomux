// The `markFirstInput()` enumeration, executable (#570; #518/#528 residual).
//
// THE INVARIANT. The human-origin bit that reaches the backend (`write_pty`'s
// `human`) is only ever as good as the list of frontend paths that mark it.
// #440 B2-R and #518 both established that list the same way — by hand, by
// grepping — and the list is not written down anywhere a compiler or a test
// can check. A future input path that reaches the PTY without calling
// `markFirstInput()`/`markHumanInput()` degrades the bit silently, and the
// degradation is not the harmless direction: `onData` reads the latch
// synchronously, so an unmarked human keystroke leaves as `human: false`, and
// the backend's keystroke-recency clock — the thing that stops loomux pasting
// over what somebody is typing — never learns a human was there. That is the
// clobber every guard in `humanorigin.ts` exists to prevent.
//
// WHY A SOURCE SCAN AND NOT A RUNTIME TEST. The marks live in `pane.ts`'s DOM
// wiring, which this repo deliberately validates by hand rather than against a
// simulated DOM (see CLAUDE.md). So the property "no input path skips the
// mark" cannot be asserted by calling anything; it is a property of the call
// GRAPH, and the cheapest honest way to pin a call graph this small is to read
// the source. `hiddenrule.test.ts` is the precedent — a stylesheet invariant
// its own subject's unit tests could never see, pinned by parsing the file.
//
// WHAT THIS IS NOT. It is a tripwire, not a proof. It knows the shapes the
// input paths have TODAY; a genuinely novel vector (a new xterm API, a direct
// `invoke("write_pty")`) is not in its table and would not fire. That is why
// the last test asserts every rule still matches real code: a rule that has
// silently stopped matching anything is worse than no rule, because it reports
// green about a file it is no longer reading.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";

// ---------- the scanner ----------

/** A source file as the scanner sees it. */
interface Source {
  path: string;
  text: string;
}

interface Rule {
  id: string;
  /** What breaks in production if this rule fires. Printed with the finding,
   *  because a tripwire whose failure message doesn't say what is now unsafe
   *  gets silenced rather than fixed. */
  why: string;
  /** The call shape that puts bytes on a path to the PTY. */
  vector: RegExp;
  /** What must appear in the window ENDING at the vector's own line. */
  requires: RegExp;
  /** Lines ABOVE the vector line that count as its window (0 = same line). */
  windowBack: number;
  /** Matches this rule's `vector` that are provably not input paths. Each
   *  entry must be hit — a stale exemption is itself a finding (below). */
  allow: { file: string; note: string }[];
}

interface Finding {
  rule: string;
  path: string;
  line: number;
  text: string;
  why: string;
}

/**
 * Blank out everything that is not code — comments and string/template/regex
 * literals — replacing each character with a space so LINE AND COLUMN NUMBERS
 * SURVIVE EXACTLY. Without this the scan is worse than useless here: 7 of the
 * 9 `.paste(` occurrences in `src/` are prose ABOUT the paste sites, including
 * `humanorigin.ts`'s whole design argument, and `ptywrite.ts`'s header quotes
 * `writePty(id, data).catch()` as the shape that CAUSED #65.
 *
 * The regex-literal heuristic is the standard one (a `/` starts a literal only
 * where a value may begin), extended with the keywords this repo actually uses
 * before one. Getting it wrong is not silently fine — a mis-read regex could
 * swallow a real call site — so `a regex literal does not eat the code after it`
 * pins it.
 */
export function stripNonCode(src: string): string {
  const REGEX_MAY_START = /(^|[(,=:[!&|?{};+\-*%~^<>]|\b(?:return|typeof|case|in|of|delete|void|instanceof|new|do|else|yield|await))\s*$/;
  let out = "";
  let i = 0;
  let mode: "code" | "line" | "block" | "sq" | "dq" | "tpl" | "re" = "code";
  const keep = (c: string): void => {
    out += c === "\n" ? "\n" : " ";
  };
  while (i < src.length) {
    const c = src[i];
    const d = src[i + 1] ?? "";
    if (mode === "code") {
      if (c === "/" && d === "/") {
        mode = "line";
        out += "  ";
        i += 2;
        continue;
      }
      if (c === "/" && d === "*") {
        mode = "block";
        out += "  ";
        i += 2;
        continue;
      }
      if (c === "'" || c === '"' || c === "`") {
        mode = c === "'" ? "sq" : c === '"' ? "dq" : "tpl";
        out += " ";
        i += 1;
        continue;
      }
      if (c === "/" && REGEX_MAY_START.test(out.slice(-32))) {
        mode = "re";
        out += " ";
        i += 1;
        continue;
      }
      out += c;
      i += 1;
      continue;
    }
    // Inside something we are blanking.
    if (c === "\n") {
      out += "\n";
      i += 1;
      // A line comment ends here; so does any single-line literal, which also
      // stops a mis-detected regex from swallowing the rest of the file.
      if (mode !== "block" && mode !== "tpl") mode = "code";
      continue;
    }
    if (c === "\\") {
      keep(c);
      if (i + 1 < src.length) keep(src[i + 1]);
      i += 2;
      continue;
    }
    if (mode === "block" && c === "*" && d === "/") {
      out += "  ";
      mode = "code";
      i += 2;
      continue;
    }
    if (
      (mode === "sq" && c === "'") ||
      (mode === "dq" && c === '"') ||
      (mode === "tpl" && c === "`") ||
      (mode === "re" && c === "/")
    ) {
      out += " ";
      mode = "code";
      i += 1;
      continue;
    }
    keep(c);
    i += 1;
  }
  return out;
}

/** Every rule, checked against every file: a finding per unexempted vector
 *  whose window does not satisfy the rule. Pure — takes sources, returns
 *  findings — so its own failure mode is testable without touching `src/`. */
export function scanInputVectors(sources: Source[], rules: Rule[]): Finding[] {
  const findings: Finding[] = [];
  for (const rule of rules) {
    for (const src of sources) {
      if (rule.allow.some((a) => a.file === src.path)) continue;
      const lines = stripNonCode(src.text).split("\n");
      const raw = src.text.split("\n");
      for (let i = 0; i < lines.length; i++) {
        if (!rule.vector.test(lines[i])) continue;
        const window = lines.slice(Math.max(0, i - rule.windowBack), i + 1).join("\n");
        if (rule.requires.test(window)) continue;
        findings.push({
          rule: rule.id,
          path: src.path,
          line: i + 1,
          text: (raw[i] ?? "").trim(),
          why: rule.why,
        });
      }
    }
  }
  return findings;
}

/** How many unexempted vectors each rule matched — the anti-vacuity reading. */
export function vectorCounts(sources: Source[], rules: Rule[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const rule of rules) {
    let n = 0;
    for (const src of sources) {
      if (rule.allow.some((a) => a.file === src.path)) continue;
      for (const line of stripNonCode(src.text).split("\n")) {
        if (rule.vector.test(line)) n++;
      }
    }
    counts.set(rule.id, n);
  }
  return counts;
}

// ---------- the table ----------
//
// Data-driven on purpose: adding a vector shape is one entry, and a finding
// names the rule, the file and the line, so the failure tells whoever broke it
// exactly which call site is now unmarked instead of "the invariant is
// violated somewhere".

const RULES: Rule[] = [
  {
    id: "paste-is-marked-human",
    why:
      "a paste that reaches the terminal without markFirstInput() leaves as human:false, so the " +
      "backend never learns a human put those bytes there and may paste a delivery over them " +
      "(#440 B2-R, #518)",
    vector: /\.paste\(/,
    requires: /markFirstInput\(\)/,
    // The two real sites mark on the line immediately above the paste; the
    // window is loose enough to survive a reformat and tight enough that a
    // mark belonging to some other method cannot satisfy it.
    windowBack: 12,
    allow: [],
  },
  {
    id: "keystrokes-are-marked-human",
    why:
      "xterm's onKey is the ONLY structural keyboard signal (onData also fires for terminal " +
      "auto-replies — #179); an onKey handler that does not mark leaves keystrokes classified " +
      "non-human",
    vector: /\.onKey\(/,
    requires: /markFirstInput\(\)/,
    windowBack: 2,
    allow: [
      {
        file: "src/modal.ts",
        note: "spec.onKey is the modal's own Escape callback prop — no terminal and no PTY behind it",
      },
    ],
  },
  {
    id: "pty-writes-go-through-the-ordered-writer",
    why:
      "a direct writePty bypasses createOrderedWriter, which is what keeps a bracketed-paste " +
      "terminator behind its body (#65) AND what carries the origin bit captured at enqueue " +
      "time (#518) — a bypassing write has neither",
    vector: /\bwritePty\(/,
    requires: /writer\.ready\(/,
    windowBack: 2,
    allow: [],
  },
  {
    id: "pty-writes-carry-an-origin",
    why:
      "writePty's `human` argument is optional and defaults to true; a two-argument call " +
      "silently asserts 'a human typed this' for every delivery loomux itself pastes",
    vector: /\bwritePty\(/,
    requires: /writePty\([^)]*,[^)]*,[^)]*\)/,
    windowBack: 0,
    allow: [],
  },
  {
    id: "terminal-data-carries-the-origin-latch",
    why:
      "onData is where the latch is read; a handler that writes without consulting humanOrigin " +
      "hands the backend a hardcoded origin, which is the bit failing open rather than being " +
      "read (#518)",
    vector: /\.onData\(/,
    requires: /humanOrigin/,
    windowBack: 0,
    allow: [],
  },
];

// ---------- the scanner's own tests, on synthetic sources ----------
//
// These are what make the real-source assertion below worth anything: they are
// the proof that the scanner FAILS when it should, which a green run over a
// correct tree cannot show.

test("an unmarked paste vector is found and named", () => {
  const bad: Source = {
    path: "src/fake.ts",
    text: [
      "class P {",
      "  dictate(text: string): void {",
      "    this.term.paste(text);",
      "  }",
      "}",
    ].join("\n"),
  };
  const findings = scanInputVectors([bad], RULES);
  assert.equal(findings.length, 1, `expected exactly one finding, got ${JSON.stringify(findings)}`);
  assert.equal(findings[0].rule, "paste-is-marked-human");
  assert.equal(findings[0].line, 3, "the finding must point at the offending line");
  assert.match(findings[0].text, /term\.paste/, "and quote it, so the failure is actionable");
  assert.match(findings[0].why, /human:false/, "and say what is now unsafe");

  // The same file, marked, is clean — or the rule is just 'never paste'.
  const good: Source = { path: "src/fake.ts", text: bad.text.replace("    this.term", "    this.markFirstInput();\n    this.term") };
  assert.deepEqual(scanInputVectors([good], RULES), []);
});

test("a writePty that skips the ordered writer, or drops the origin, is found", () => {
  const bypass: Source = {
    path: "src/fake.ts",
    text: "function send(id: number, data: string) {\n  writePty(id, data, true);\n}",
  };
  assert.deepEqual(
    scanInputVectors([bypass], RULES).map((f) => f.rule),
    ["pty-writes-go-through-the-ordered-writer"],
    "a raw writePty is a new input path with neither ordering nor an origin latch behind it"
  );

  const noOrigin: Source = {
    path: "src/fake.ts",
    text: "  this.writer.ready((data) => writePty(ptyId, data));",
  };
  assert.deepEqual(
    scanInputVectors([noOrigin], RULES).map((f) => f.rule),
    ["pty-writes-carry-an-origin"],
    "dropping the third argument re-asserts the pre-#518 default for every delivery"
  );
});

test("an onData handler that hardcodes the origin is found", () => {
  const hardcoded: Source = {
    path: "src/fake.ts",
    text: "    this.term.onData((data) => this.writer.write(data, true));",
  };
  assert.deepEqual(
    scanInputVectors([hardcoded], RULES).map((f) => f.rule),
    ["terminal-data-carries-the-origin-latch"]
  );
});

test("prose about an input path is not an input path", () => {
  // The failure mode that would make this whole test useless in the opposite
  // direction: `humanorigin.ts` and `pane.ts` discuss `term.paste()` and
  // `writePty(id, data)` at length in comments, and a scanner that read those
  // as code would report a permanent, unfixable finding — which is how a
  // tripwire gets deleted.
  const prose: Source = {
    path: "src/fake.ts",
    text: [
      "// It took the signal from `term.onKey` and the two `term.paste()` sites.",
      "/** Firing them concurrently — `writePty(id, data).catch()` — reorders. */",
      'const doc = "call this.term.paste(x) without a mark and it breaks";',
      "const re = /this\\.term\\.paste\\(/;",
      "const tpl = `writePty(id, data)`;",
    ].join("\n"),
  };
  assert.deepEqual(scanInputVectors([prose], RULES), [], "comments and literals are not call sites");
});

test("a regex literal does not eat the code after it", () => {
  // The one heuristic in `stripNonCode` that can be wrong in the dangerous
  // direction: mistake a division for a regex (or the reverse) and the rest of
  // a line — possibly a real call site — is blanked or mis-read.
  const src = [
    "const ok = /\\/\\//.test(s);",
    "const half = total / 2;",
    "this.term.paste(x);",
  ].join("\n");
  const stripped = stripNonCode(src).split("\n");
  assert.match(stripped[1], /total \/ 2/, "division is code, not the start of a literal");
  assert.match(stripped[2], /this\.term\.paste\(x\)/, "and the line after a regex is intact");
  assert.equal(stripped.length, 3, "line numbering must survive stripping exactly");
});

test("stripping preserves line numbers across multi-line comments and templates", () => {
  const src = ["/* one", "   two", "   three */", "this.term.paste(x);"].join("\n");
  const stripped = stripNonCode(src).split("\n");
  assert.equal(stripped.length, 4);
  assert.equal(stripped[1].trim(), "", "comment interiors are blanked");
  assert.match(stripped[3], /paste/, "and the code below keeps its line number");
});

// ---------- the real tree ----------

const SRC_DIR = new URL("../src/", import.meta.url);

function realSources(): Source[] {
  return readdirSync(SRC_DIR)
    .filter((f) => f.endsWith(".ts"))
    .sort()
    .map((f) => ({ path: `src/${f}`, text: readFileSync(new URL(f, SRC_DIR), "utf8") }));
}

test("no input path in src/ reaches the PTY without marking human origin", () => {
  const findings = scanInputVectors(realSources(), RULES);
  assert.deepEqual(
    findings.map((f) => `${f.path}:${f.line} [${f.rule}] ${f.text}\n    why: ${f.why}`),
    [],
    "a new input path skipped the human-origin mark — see the lines above"
  );
});

test("every rule still matches real code, and every exemption is still needed", () => {
  // Anti-vacuity, and the only thing standing between this file and a test
  // that passes because it stopped reading anything. `pane.ts` could be split,
  // xterm's API could be wrapped, a method could be renamed — each of which
  // makes a rule match nothing while the invariant it guards quietly stops
  // being checked. `.loomux/lessons.md`: enumerate in writing, including the
  // entries that are fine.
  const sources = realSources();
  const counts = vectorCounts(sources, RULES);
  for (const rule of RULES) {
    assert.ok(
      (counts.get(rule.id) ?? 0) > 0,
      `rule ${rule.id} matches nothing in src/ — its pattern has drifted away from the code it ` +
        `is supposed to guard, so it is now reporting green about a file it never reads`
    );
  }
  // A stale exemption is the same failure wearing the other hat: it turns off
  // a rule for a whole file on the strength of a match that is no longer there.
  for (const rule of RULES) {
    for (const a of rule.allow) {
      const file = sources.find((s) => s.path === a.file);
      assert.ok(file, `exemption ${rule.id} → ${a.file} names a file that no longer exists`);
      assert.ok(
        stripNonCode(file.text).split("\n").some((l) => rule.vector.test(l)),
        `exemption ${rule.id} → ${a.file} is stale (${a.note}); delete it rather than leaving ` +
          `the rule switched off for that whole file`
      );
    }
  }
});
