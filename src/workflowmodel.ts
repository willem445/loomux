// Pure model for `.loomux/workflow.yml` — the user-defined agent workflow (#222).
// DOM-free and I/O-free: parse, validate, derive the graph, serialize. The pane
// (workflowview.ts) is a VIEW over this; the FILE is the source of truth (the
// Kestra pattern — a form edit rewrites the YAML, it does not become a second,
// competing copy of it).
//
// Three rules this module exists to keep, each earned from a documented failure of
// some other workflow tool (see the #222 investigation, §1a-v and §4):
//
//  1. `id` is the identity; `name` is display only. n8n keys its graph by the node's
//     DISPLAY NAME, so a rename silently breaks every edge and expression pointing at
//     it. Here an edge/gate names an `id`, ids are immutable once created, and a rename
//     touches nothing else.
//  2. No coordinates, ever. Dify/ComfyUI/Langflow all embed x/y in the semantic file,
//     so nudging a node churns the logic diff. Layout (if the view ever draws any) goes
//     in `.loomux/workflow.layout.json`; this file is the workflow.
//  3. Validate BEFORE a run, not during one. Flowise, Langflow and Dify discover a
//     dangling reference at runtime; Dify will happily *publish* a workflow whose node
//     isn't installed. `validateWorkflow` is the whole pre-run pass, and it is pure
//     precisely so it is unit-tested without a DOM (test/workflowmodel.test.ts).
//
// A fourth rule is about how we FAIL: an unknown kind, an unknown CLI, a dangling edge
// — none of them stop the file from opening. They become findings, the block renders as
// a stub, and the human can fix it in the pane. Refusing to open a file you can't fully
// understand is ComfyUI's #1 import-failure class, and it is the one behavior guaranteed
// to leave someone stranded with no way to repair the thing that is broken.
//
// The YAML subset is hand-rolled rather than pulled from npm: the schema is small and
// CLOSED (block mappings, block sequences, flow seqs/maps, block scalars, comments,
// quoted scalars), and the alternative is a dependency in a project that has spent real
// effort keeping its dependency list short. Anything the subset can't read is a parse
// finding on a line number — the raw-text view still opens, so the file is still fixable.

// ---------- the closed enums ----------

/** The capability classes. CLOSED, deliberately (#222 §2c): a workflow file may define
 *  unlimited personas but may never invent a capability — `kind` picks one of these four
 *  and inherits its structural guarantees (a planner gets no worktree and no write tools;
 *  a reviewer may `gh pr review` but can never push). A repo file that could grant itself
 *  write access would be a footgun with `auto_ops` on and nobody watching. */
export const BLOCK_KINDS = ["orchestrator", "worker", "reviewer", "planner"] as const;
export type BlockKind = (typeof BLOCK_KINDS)[number];

export function isBlockKind(v: string): v is BlockKind {
  return (BLOCK_KINDS as readonly string[]).includes(v);
}

/** The agent CLIs a block may run. Mirrors the backend's `SUPPORTED_CLIS`
 *  (orchestration/mod.rs) — a block naming anything else is a finding, not a spawn. */
export const WORKFLOW_CLIS = ["claude", "copilot"] as const;
export type WorkflowCli = (typeof WORKFLOW_CLIS)[number];

export function isWorkflowCli(v: string): v is WorkflowCli {
  return (WORKFLOW_CLIS as readonly string[]).includes(v);
}

/** The schema version this build reads and writes. */
export const WORKFLOW_VERSION = 1;

/** Where the workflow lives, relative to the repo root. */
export const WORKFLOW_FILE = ".loomux/workflow.yml";

/** What a `merge` gate can require of its reviewers. `all-pass` = every named reviewer
 *  recorded PASS; `threshold` = at least N of them did. */
export const GATE_REQUIRES = ["all-pass", "threshold"] as const;
export type GateRequire = (typeof GATE_REQUIRES)[number];

/** A legal block id: lowercase-ish, human-meaningful, safe as a filename fragment and as
 *  a shell-adjacent token. Deliberately strict — the id ends up in agent ids, pane names
 *  and (via the backend) command lines, and `sanitize_model` (mod.rs) is the precedent
 *  for keeping repo-authored strings out of a shell line. */
const BLOCK_ID_RE = /^[a-z][a-z0-9]*(?:[-_][a-z0-9]+)*$/;

export function isValidBlockId(id: string): boolean {
  return BLOCK_ID_RE.test(id);
}

// ---------- the schema ----------

/** Any value the YAML subset can hold. Blocks keep the keys they don't understand
 *  (`extra`) so a file written by a NEWER loomux survives a round-trip through an older
 *  pane instead of being silently stripped by it. */
export type YamlValue = string | number | boolean | null | YamlValue[] | { [k: string]: YamlValue };

/** One agent block: a persona (prompt or profile), a model, a CLI — and a `kind` that
 *  says which capability class it runs as.
 *
 *  `kind` and `cli` are typed as raw strings, not as the enums: a file naming
 *  `kind: superuser` must still LOAD (as a stub, with a finding) so the human can fix it
 *  in the pane. Narrowing them at the type level would force the parser to drop the very
 *  block the user needs to see. */
export interface WorkflowBlock {
  /** Immutable identity. Edges and gates reference THIS (never `name`). */
  id: string;
  /** Display label. Renaming it must never break a reference — that is its whole point. */
  name: string;
  /** One of BLOCK_KINDS; anything else is a finding + a stub. */
  kind: string;
  /** One of WORKFLOW_CLIS; anything else is a finding + a stub. */
  cli: string;
  /** Model to pin, or "" for the CLI's default. */
  model: string;
  /** Inline persona — compiled to `claude --agents '<json>'` (native, no file needed). */
  prompt?: string;
  /** Persona file — compiled to `copilot --agent <name>` against `.github/agents/`.
   *  Mutually exclusive with `prompt` (a block with both is a finding). */
  profile?: string;
  /** Keys this build doesn't know, preserved verbatim across a round-trip. */
  extra?: Record<string, YamlValue>;
}

/** One ADVISORY edge (#222 §2g): the declared happy path. The orchestrator still decides
 *  when to spawn what — a static DAG would replace its mergeability/parallelism judgment
 *  with something dumber. Edges document intent and drive the graph view; gates are the
 *  half that is actually enforced. */
export interface WorkflowEdge {
  from: string;
  to: string;
}

/** The ENFORCED half: a merge gate. The backend refuses `gh pr merge` (via the existing
 *  PATH shim) until the named reviewers' verdicts are recorded PASS — which is what makes
 *  multi-reviewer more than theatre, and closes the loomux side of #197. */
export interface MergeGate {
  require: string;
  /** Only meaningful when `require: threshold`. */
  threshold?: number;
  reviewers: string[];
  /** Extra conditions (`ci-green`, …) — passed through; the backend owns their meaning. */
  also: string[];
}

export interface WorkflowGates {
  merge?: MergeGate;
  extra?: Record<string, YamlValue>;
}

export interface Workflow {
  version: number;
  name: string;
  blocks: WorkflowBlock[];
  edges: WorkflowEdge[];
  gates: WorkflowGates;
  extra?: Record<string, YamlValue>;
}

// ---------- findings ----------

export type FindingSeverity = "error" | "warning";

export type FindingCode =
  | "yaml-syntax"
  | "not-a-mapping"
  | "version-missing"
  | "version-unsupported"
  | "no-blocks"
  | "block-not-a-mapping"
  | "block-id-missing"
  | "block-id-invalid"
  | "block-id-duplicate"
  | "unknown-kind"
  | "unknown-cli"
  | "prompt-and-profile"
  | "edge-not-a-mapping"
  | "edge-unknown-block"
  | "edge-self"
  | "gate-unknown-require"
  | "gate-no-reviewers"
  | "gate-unknown-reviewer"
  | "gate-not-a-reviewer"
  | "gate-bad-threshold"
  | "isolated-block"
  | "unreachable-block"
  | "no-entry-block";

/** One thing wrong with the workflow. `blockId` lets the pane render the finding INLINE
 *  next to the block it is about (the whole reason the validation pass is worth having is
 *  that it tells you WHERE); `line` does the same for the raw-text view. */
export interface Finding {
  severity: FindingSeverity;
  code: FindingCode;
  message: string;
  blockId?: string;
  /** 1-based source line, when the finding came from reading the text. */
  line?: number;
}

export function hasErrors(findings: readonly Finding[]): boolean {
  return findings.some((f) => f.severity === "error");
}

// ---------- YAML subset: reading ----------

interface RawLine {
  /** 0-based index into the source lines. */
  i: number;
  /** Leading-space count. */
  indent: number;
  /** The line with its indent and any trailing comment removed. */
  text: string;
}

/** Strip a `#` comment, ignoring one inside a quoted scalar. A `#` that is not preceded
 *  by whitespace is NOT a comment in YAML (`a#b` is the scalar `a#b`), which matters here
 *  because a model or a branch can legitimately contain one. */
function stripComment(line: string): string {
  let quote: '"' | "'" | null = null;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (quote) {
      if (c === "\\" && quote === '"') i++;
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'") {
      quote = c;
      continue;
    }
    if (c === "#" && (i === 0 || /\s/.test(line[i - 1]!))) return line.slice(0, i);
  }
  return line;
}

const indentOf = (line: string): number => line.length - line.trimStart().length;

class YamlReader {
  /** Cursor into `raw` — the next line not yet consumed. */
  private i = 0;
  readonly findings: Finding[] = [];
  private readonly raw: string[];

  constructor(raw: string[]) {
    this.raw = raw;
  }

  private err(line: number, message: string): void {
    this.findings.push({ severity: "error", code: "yaml-syntax", message, line: line + 1 });
  }

  /** The next SIGNIFICANT line (blank and comment-only lines skipped), without consuming
   *  it. Callers consume by setting the cursor past the line they took.
   *
   *  TABS. YAML forbids a tab in indentation, and this pane must say so — because the
   *  backend validator (a real parser) will refuse the same file, and a pane that reports
   *  `valid` on a file the spawn then rejects is worse than one that reports nothing.
   *  The line is skipped with a finding rather than aborting the read: the rest of the
   *  file still opens, which is this module's whole contract.
   *
   *  (A tab INSIDE a block scalar is content, not indentation, and stays that way —
   *  `blockScalar` reads `this.raw` directly and never comes through here.) */
  private peek(): RawLine | null {
    for (let j = this.i; j < this.raw.length; j++) {
      const raw = this.raw[j]!;
      // Test the RAW leading whitespace. The previous form — `raw.trimStart().startsWith("\t")`
      // — could never fire, because trimStart() strips the very tab it was looking for
      // (rev-5 F2): the guard was dead, and a fully tab-indented file validated clean.
      if (/^[ ]*\t/.test(raw)) {
        if (!this.tabLines.has(j)) {
          this.tabLines.add(j); // peek() is called repeatedly; the finding is reported once
          this.err(j, "tabs cannot be used for indentation in YAML — use spaces");
        }
        continue;
      }
      const text = stripComment(raw).trimEnd();
      if (!text.trim()) continue;
      return { i: j, indent: indentOf(text), text: text.trim() };
    }
    return null;
  }

  /** Lines already reported as tab-indented, so a re-peek doesn't report them twice. */
  private readonly tabLines = new Set<number>();

  /** Read the whole document as a mapping. */
  document(): YamlValue {
    const p = this.peek();
    if (!p) return {};
    if (p.indent !== 0) {
      this.err(p.i, "the document must start at column 0");
      return {};
    }
    if (p.text.startsWith("-")) {
      this.err(p.i, "a workflow file is a mapping (version:, blocks:, …), not a list");
      return {};
    }
    return this.mapping(0);
  }

  private mapping(indent: number): Record<string, YamlValue> {
    const obj: Record<string, YamlValue> = {};
    for (;;) {
      const p = this.peek();
      if (!p || p.indent < indent) break;
      if (p.indent > indent) {
        // Nothing above claimed this line — it is over-indented. Skip it rather than
        // spin, and say so: an unconsumed line with no error is a silently dropped key.
        this.err(p.i, `unexpected indentation — "${p.text}" is indented further than its siblings`);
        this.i = p.i + 1;
        continue;
      }
      if (p.text.startsWith("-")) break; // a sequence at this level ends the mapping
      const split = splitKey(p.text);
      if (!split) {
        this.err(p.i, `expected "key: value" but found "${p.text}"`);
        this.i = p.i + 1;
        continue;
      }
      this.i = p.i + 1;
      obj[split.key] = this.afterKey(split.rest, indent, p.i);
    }
    return obj;
  }

  /** The value that follows `key:` on line `at`, whatever form it takes. */
  private afterKey(rest: string, indent: number, at: number): YamlValue {
    if (rest === "") {
      const p = this.peek();
      // A nested block is indented further — EXCEPT a sequence, which YAML allows to sit
      // at the parent key's own indent. Both are common in the wild; accept both.
      if (p && (p.indent > indent || (p.indent === indent && p.text.startsWith("-")))) {
        return p.text.startsWith("-") ? this.sequence(p.indent) : this.mapping(p.indent);
      }
      return null;
    }
    // `|`, `>`, with an optional INDENTATION INDICATOR and/or chomping marker, in either
    // order (`|2`, `|-`, `|2-`, `|-2` are all legal YAML).
    if (/^[|>](?:\d[-+]?|[-+]?\d?)$/.test(rest)) return this.blockScalar(rest, indent);
    return this.flowOrScalar(rest, at);
  }

  private sequence(indent: number): YamlValue[] {
    const items: YamlValue[] = [];
    for (;;) {
      const p = this.peek();
      if (!p || p.indent !== indent) break;
      if (p.text !== "-" && !p.text.startsWith("- ")) break;
      this.i = p.i + 1;
      if (p.text === "-") {
        // The item's content is the block indented under the dash.
        const q = this.peek();
        if (q && q.indent > indent) {
          items.push(q.text.startsWith("-") ? this.sequence(q.indent) : this.mapping(q.indent));
        } else {
          items.push(null);
        }
        continue;
      }
      const rest = p.text.slice(1).trimStart();
      // The column the item's keys live at: where `rest` actually starts on the line.
      // `- id: x` puts them at dash+2, but `-   id: x` is legal too, and getting this
      // wrong silently drops every key after the first.
      const keyIndent = indent + (p.text.length - rest.length);
      const split = rest.startsWith("{") || rest.startsWith("[") ? null : splitKey(rest);
      if (split) {
        const first: Record<string, YamlValue> = {};
        first[split.key] = this.afterKey(split.rest, keyIndent, p.i);
        items.push({ ...first, ...this.mapping(keyIndent) });
      } else {
        items.push(this.flowOrScalar(rest, p.i));
      }
    }
    return items;
  }

  /** A `|` / `>` block scalar: every line indented past the key, dedented by the content's
   *  indent — which the header STATES (`|2`) when it can, and which is otherwise inferred
   *  from the first content line. The explicit form is what we emit, and it is the only one
   *  that survives a prompt whose own first line is indented (rev-5 F3): inferring the
   *  dedent from content that is itself indented eats exactly that indentation.
   *
   *  Comments are NOT stripped here — inside a block scalar a `#` is content, and a prompt
   *  that says "# Review checklist" must survive. Tabs likewise: in here they are text. */
  private blockScalar(header: string, parentIndent: number): string {
    const folded = header.startsWith(">");
    const chomp = header.includes("-") ? "strip" : header.includes("+") ? "keep" : "clip";
    const indicator = /\d/.exec(header);
    const body: string[] = [];
    // -1 = "infer from the first content line". An explicit indicator is RELATIVE to the
    // parent node's indentation, which is what makes it independent of the content.
    let contentIndent = indicator ? parentIndent + Number(indicator[0]) : -1;
    while (this.i < this.raw.length) {
      const raw = this.raw[this.i]!;
      if (!raw.trim()) {
        body.push("");
        this.i++;
        continue;
      }
      const ind = indentOf(raw);
      if (ind <= parentIndent) break;
      if (contentIndent < 0) contentIndent = ind;
      body.push(raw.slice(Math.min(ind, contentIndent)).trimEnd());
      this.i++;
    }
    while (body.length && body[body.length - 1] === "") body.pop();
    if (!body.length) return "";
    const text = folded ? foldLines(body) : body.join("\n");
    return chomp === "strip" ? text : text + "\n";
  }

  private flowOrScalar(text: string, at: number): YamlValue {
    if (text.startsWith("[") || text.startsWith("{")) {
      const flow = new FlowReader(text);
      try {
        const v = flow.parse();
        if (!flow.atEnd()) this.err(at, `trailing text after "${text.slice(0, flow.pos)}"`);
        return v;
      } catch (e) {
        this.err(at, e instanceof Error ? e.message : String(e));
        return null;
      }
    }
    return plainScalar(text);
  }
}

/** Fold a `>` scalar: consecutive non-blank lines join with a space, a blank line is a
 *  paragraph break. (Supported for completeness — `|` is what a prompt actually wants,
 *  because folding a prompt's line breaks changes what the agent reads.) */
function foldLines(lines: string[]): string {
  const out: string[] = [];
  let para: string[] = [];
  const flush = (): void => {
    if (para.length) out.push(para.join(" "));
    para = [];
  };
  for (const l of lines) {
    if (!l.trim()) {
      flush();
      out.push("");
    } else para.push(l.trim());
  }
  flush();
  return out.join("\n");
}

/** Split `key: value` at the first top-level `: ` (or a trailing `:`). Returns null when
 *  the line is not a mapping entry at all. */
function splitKey(text: string): { key: string; rest: string } | null {
  let quote: '"' | "'" | null = null;
  let depth = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text[i]!;
    if (quote) {
      if (c === "\\" && quote === '"') i++;
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'") quote = c;
    else if (c === "[" || c === "{") depth++;
    else if (c === "]" || c === "}") depth--;
    else if (c === ":" && depth === 0) {
      const next = text[i + 1];
      if (next === undefined || next === " ") {
        const key = text.slice(0, i).trim();
        if (!key) return null;
        return { key: unquote(key), rest: text.slice(i + 1).trim() };
      }
    }
  }
  return null;
}

/** Escape codes a double-quoted scalar can carry. `default: the character itself` covers
 *  `\"` and `\\`, which is the whole point of an escape. */
const ESCAPES: Record<string, string> = { n: "\n", t: "\t", r: "\r" };

function unquote(s: string): string {
  if (s.length >= 2 && s[0] === '"' && s.endsWith('"')) {
    // ONE PASS, left to right (rev-6 F8). Chained `.replace()`s unescape in the wrong order:
    // `\\n` (an escaped backslash followed by the letter n) had its `\n` expanded to a
    // NEWLINE by the first replace, before the later one could collapse `\\` to a single
    // backslash — so `"C:\\new"` read back as `C:` + newline + `ew`. A single pass consumes
    // each backslash with the character it actually escapes, so an escaped backslash can
    // never be re-read as the start of another escape.
    return s.slice(1, -1).replace(/\\(.)/g, (_, c: string) => ESCAPES[c] ?? c);
  }
  if (s.length >= 2 && s[0] === "'" && s.endsWith("'")) return s.slice(1, -1).replace(/''/g, "'");
  return s;
}

function plainScalar(text: string): YamlValue {
  if (text[0] === '"' || text[0] === "'") return unquote(text);
  if (text === "null" || text === "~") return null;
  if (text === "true") return true;
  if (text === "false") return false;
  if (/^-?\d+$/.test(text)) return Number(text);
  if (/^-?\d+\.\d+$/.test(text)) return Number(text);
  return text;
}

/** A one-line flow collection: `[a, b]`, `{ from: x, to: [a, b] }`. */
class FlowReader {
  pos = 0;
  private readonly s: string;

  constructor(s: string) {
    this.s = s;
  }

  atEnd(): boolean {
    this.ws();
    return this.pos >= this.s.length;
  }

  parse(): YamlValue {
    this.ws();
    const c = this.s[this.pos];
    if (c === "[") return this.seq();
    if (c === "{") return this.map();
    return this.scalar();
  }

  private ws(): void {
    while (this.pos < this.s.length && /\s/.test(this.s[this.pos]!)) this.pos++;
  }

  private seq(): YamlValue[] {
    this.pos++; // [
    const out: YamlValue[] = [];
    for (;;) {
      this.ws();
      if (this.s[this.pos] === "]") {
        this.pos++;
        return out;
      }
      if (this.pos >= this.s.length) throw new Error("unterminated [ … ] list");
      out.push(this.parse());
      this.ws();
      if (this.s[this.pos] === ",") this.pos++;
      else if (this.s[this.pos] !== "]") throw new Error(`expected "," or "]" in list`);
    }
  }

  private map(): Record<string, YamlValue> {
    this.pos++; // {
    const out: Record<string, YamlValue> = {};
    for (;;) {
      this.ws();
      if (this.s[this.pos] === "}") {
        this.pos++;
        return out;
      }
      if (this.pos >= this.s.length) throw new Error("unterminated { … } mapping");
      const key = this.scalarText();
      this.ws();
      if (this.s[this.pos] !== ":") throw new Error(`expected ":" after "${key}"`);
      this.pos++;
      out[unquote(key)] = this.parse();
      this.ws();
      if (this.s[this.pos] === ",") this.pos++;
      else if (this.s[this.pos] !== "}") throw new Error(`expected "," or "}" in mapping`);
    }
  }

  private scalar(): YamlValue {
    return plainScalar(this.scalarText());
  }

  /** A scalar token up to the next structural character, quotes respected. */
  private scalarText(): string {
    this.ws();
    const c = this.s[this.pos];
    if (c === '"' || c === "'") {
      const start = this.pos;
      this.pos++;
      while (this.pos < this.s.length) {
        const ch = this.s[this.pos]!;
        if (ch === "\\" && c === '"') this.pos += 2;
        else if (ch === c) {
          this.pos++;
          return this.s.slice(start, this.pos);
        } else this.pos++;
      }
      throw new Error("unterminated quoted string");
    }
    const start = this.pos;
    while (this.pos < this.s.length && !",:[]{}".includes(this.s[this.pos]!)) this.pos++;
    const text = this.s.slice(start, this.pos).trim();
    if (!text) throw new Error("expected a value");
    return text;
  }
}

// ---------- YAML subset: writing (the canonical formatter) ----------
//
// One shape, always, so `git diff` shows what CHANGED and not how it was written: fixed
// key order (the order a human reads a block in — who it is, what it runs as, what it
// runs on, then the persona body last because it is the long one), edges grouped by
// their source and ordered by the blocks they connect, gate lists ordered the same way.
//
// Blocks themselves keep their AUTHORED order. That is the one place a "stable sort"
// would do harm: the roster reads top-to-bottom, and re-sorting it alphabetically on
// every save would churn the diff of the file it is supposed to keep legible.

/** Quote a scalar when leaving it bare would change what it means (or fail to parse).
 *
 *  `,` `[` `]` `{` `}` are in the list for a reason worth stating, because leaving them out
 *  was a silent file-corrupting bug (rev-5 F1): this ONE emitter serves both contexts —
 *  block (`name: …`) and FLOW (`reviewers: [a, b]`, `also: […]`, an unknown key's array or
 *  map). In flow context those five characters are STRUCTURAL, so an unquoted
 *  `Bash(gh pr view --json title,body)` re-reads as two list entries and an unquoted
 *  `fmt{x}` closes the collection early and takes the whole value down with it — and both
 *  happen on an ordinary form edit, because every form edit re-serializes the file.
 *
 *  Rather than keep two emitters and a rule about which context is which (the rule you
 *  forget at exactly one of the six call sites), the ONE emitter quotes for the strictest
 *  context. A quote is always SAFE in block context — it just isn't always necessary — and
 *  "sometimes unnecessary" is a far cheaper failure than "sometimes destroys the value". */
function emitScalar(v: string): string {
  if (v === "") return '""';
  if (
    /^[-?:,[\]{}#&*!|>'"%@`]/.test(v) ||
    /[,[\]{}]/.test(v) || // structural in a flow collection, anywhere in the string
    /:\s/.test(v) ||
    /\s#/.test(v) ||
    v !== v.trim() ||
    v === "true" ||
    v === "false" ||
    v === "null" ||
    v === "~" ||
    /^-?\d+(\.\d+)?$/.test(v) ||
    /[\n\t\r]/.test(v)
  ) {
    // Backslash FIRST, so the escapes introduced below aren't themselves re-escaped — the
    // mirror image of the reader's single pass (see `unquote`), and the two must stay
    // symmetric or a value stops surviving the round-trip it just survived.
    return `"${v
      .replace(/\\/g, "\\\\")
      .replace(/"/g, '\\"')
      .replace(/\n/g, "\\n")
      .replace(/\t/g, "\\t")
      .replace(/\r/g, "\\r")}"`;
  }
  return v;
}

function emitValue(v: YamlValue): string {
  if (v === null) return "null";
  if (typeof v === "boolean" || typeof v === "number") return String(v);
  if (typeof v === "string") return emitScalar(v);
  if (Array.isArray(v)) return `[${v.map(emitValue).join(", ")}]`;
  // The KEY goes through the emitter too (rev-6 F9). A key is a string in a flow mapping and
  // is every bit as capable of holding a `,` or a `}` as a value is — emitting it raw was the
  // value-side bug (F1) with the two halves of the pair swapped, and it survived F1's fix
  // only because nothing had put a structural character in a key yet.
  return `{ ${Object.keys(v)
    .sort()
    .map((k) => `${emitScalar(k)}: ${emitValue(v[k]!)}`)
    .join(", ")} }`;
}

/** A `|` block scalar, indented under its key. A prompt keeps its line breaks — folding
 *  them would change what the agent actually reads. */
function emitBlockScalar(key: string, text: string, indent: string): string[] {
  // A body that ends in a newline is `|` (clip); one that doesn't is `|-` (strip). That
  // is what makes prompt → YAML → prompt exact rather than approximately exact.
  const chomp = text.endsWith("\n") ? "" : "-";
  const body = text.replace(/\n$/, "").split("\n");
  // The INDENTATION INDICATOR (`|2`), and why it isn't optional (rev-5 F3): a plain `|` is
  // read back by dedenting to the FIRST CONTENT LINE's indent, so a prompt whose first line
  // is itself indented — a code snippet, an indented checklist, and it comes straight out of
  // the form's textarea — silently loses that indent on the next read. Same for a prompt
  // that opens with a blank line, where the "first content line" is the second one. Stating
  // the indent explicitly makes the reader's dedent independent of the content, which is the
  // only way this round-trips.
  const first = body[0] ?? "";
  const explicit = first === "" || /^\s/.test(first);
  const header = `|${explicit ? BLOCK_SCALAR_INDENT : ""}${chomp}`;
  const pad = " ".repeat(BLOCK_SCALAR_INDENT);
  return [`${indent}${key}: ${header}`, ...body.map((l) => (l ? `${indent}${pad}${l}` : ""))];
}

/** How far a block scalar's body is indented past its key. Both halves of the round-trip
 *  read it: the emitter pads by it, and the `|2` indicator it writes tells the reader to
 *  dedent by exactly it rather than by guessing from the content. */
const BLOCK_SCALAR_INDENT = 2;

function extraLines(extra: Record<string, YamlValue> | undefined, indent: string): string[] {
  if (!extra) return [];
  // The key goes through the emitter here too, for the same reason as in `emitValue` — an
  // unknown key is as arbitrary as an unknown value, and a key carrying a `: ` would
  // otherwise re-read as a different key with a different value. (`splitKey`/`unquote`
  // already read a quoted key; only the writing side was asymmetric.)
  return Object.keys(extra)
    .sort()
    .map((k) => `${indent}${emitScalar(k)}: ${emitValue(extra[k]!)}`);
}

/** Render the workflow in canonical form. `parseWorkflow(serializeWorkflow(w)).workflow`
 *  deep-equals `w`, and serializing twice is a no-op — the two properties the file's
 *  legibility rests on, both pinned in test/workflowmodel.test.ts. */
export function serializeWorkflow(w: Workflow): string {
  const order = blockOrder(w);
  const out: string[] = [];
  out.push(`version: ${w.version}`);
  if (w.name) out.push(`name: ${emitScalar(w.name)}`);
  out.push(...extraLines(w.extra, ""));

  // An EMPTY roster emits `blocks: []`, not a bare `blocks:` (rev-5 F4). A bare key is
  // YAML `null`, so the pane would re-read its own output as a malformed shape and report a
  // syntax-ish error against text it had just written itself — on top of the honest
  // `no-blocks`. Deleting the last block in the form is the ordinary way to get here.
  out.push("", w.blocks.length ? "blocks:" : "blocks: []");
  for (const b of w.blocks) {
    out.push(`  - id: ${emitScalar(b.id)}`);
    out.push(`    name: ${emitScalar(b.name)}`);
    out.push(`    kind: ${emitScalar(b.kind)}`);
    out.push(`    cli: ${emitScalar(b.cli)}`);
    if (b.model) out.push(`    model: ${emitScalar(b.model)}`);
    if (b.profile !== undefined) out.push(`    profile: ${emitScalar(b.profile)}`);
    out.push(...extraLines(b.extra, "    "));
    if (b.prompt !== undefined) out.push(...emitBlockScalar("prompt", b.prompt, "    "));
  }

  const groups = groupEdges(w.edges, order);
  if (groups.length) {
    out.push("", "edges:");
    for (const g of groups) {
      const to = g.to.length === 1 ? emitScalar(g.to[0]!) : `[${g.to.map(emitScalar).join(", ")}]`;
      out.push(`  - { from: ${emitScalar(g.from)}, to: ${to} }`);
    }
  }

  const gate = w.gates.merge;
  if (gate || w.gates.extra) {
    out.push("", "gates:");
    if (gate) {
      out.push("  merge:");
      out.push(`    require: ${emitScalar(gate.require)}`);
      if (gate.threshold !== undefined) out.push(`    threshold: ${gate.threshold}`);
      out.push(`    reviewers: [${sortByBlocks(gate.reviewers, order).map(emitScalar).join(", ")}]`);
      if (gate.also.length) out.push(`    also: [${gate.also.map(emitScalar).join(", ")}]`);
    }
    out.push(...extraLines(w.gates.extra, "  "));
  }

  return out.join("\n") + "\n";
}

/** Block id → its position in the roster. The sort key for everything that REFERENCES a
 *  block (edges, reviewer lists), so those lists read in graph order instead of
 *  alphabetical order — and so an unrelated rename can't reshuffle them. */
function blockOrder(w: Workflow): Map<string, number> {
  return new Map(w.blocks.map((b, i) => [b.id, i]));
}

function sortByBlocks(ids: readonly string[], order: Map<string, number>): string[] {
  // Dangling references (not in the roster) sort last, alphabetically: they are exactly
  // what the validation pass is about to complain about, so they belong where they are
  // easy to see rather than interleaved with the real ones.
  const seen = new Set<string>();
  const uniq = ids.filter((id) => (seen.has(id) ? false : (seen.add(id), true)));
  return uniq.sort((a, b) => {
    const ia = order.get(a),
      ib = order.get(b);
    if (ia !== undefined && ib !== undefined) return ia - ib;
    if (ia !== undefined) return -1;
    if (ib !== undefined) return 1;
    return a.localeCompare(b);
  });
}

/** Collapse the edge list into one entry per source (`{ from: worker, to: [a, b] }`),
 *  deduped and ordered by the roster. The fan-out form is how the schema sketch writes
 *  it and how a human reads it; the model keeps edges flat because every graph question
 *  (reachability, in-degree) is asked of pairs. */
function groupEdges(
  edges: readonly WorkflowEdge[],
  order: Map<string, number>
): { from: string; to: string[] }[] {
  const byFrom = new Map<string, string[]>();
  for (const e of edges) {
    const list = byFrom.get(e.from) ?? [];
    if (!list.includes(e.to)) list.push(e.to);
    byFrom.set(e.from, list);
  }
  return sortByBlocks([...byFrom.keys()], order).map((from) => ({
    from,
    to: sortByBlocks(byFrom.get(from)!, order),
  }));
}

// ---------- parse: text → model ----------

export interface ParseResult {
  workflow: Workflow;
  /** Syntax + shape findings. SEMANTIC findings (dangling edges, unknown kinds …) come
   *  from `validateWorkflow` — split because the pane re-validates a model the human is
   *  editing in the form, where there is no text to have a syntax error in. */
  findings: Finding[];
}

const asString = (v: YamlValue): string | null =>
  typeof v === "string" ? v : typeof v === "number" || typeof v === "boolean" ? String(v) : null;

const KNOWN_TOP = new Set(["version", "name", "blocks", "edges", "gates"]);
const KNOWN_BLOCK = new Set(["id", "name", "kind", "cli", "model", "prompt", "profile"]);
const KNOWN_GATE = new Set(["merge"]);

function collectExtra(
  obj: Record<string, YamlValue>,
  known: Set<string>
): Record<string, YamlValue> | undefined {
  const extra: Record<string, YamlValue> = {};
  for (const k of Object.keys(obj)) if (!known.has(k)) extra[k] = obj[k]!;
  return Object.keys(extra).length ? extra : undefined;
}

/** Read a workflow file. NEVER throws and NEVER refuses: a file it cannot fully
 *  understand still yields a workflow (with stub blocks) plus the findings that say why,
 *  because the pane's job is to let the human FIX the file — which it cannot do if the
 *  file won't open. */
export function parseWorkflow(text: string): ParseResult {
  // Strip a BOM. A workflow file written by a Windows editor (or by `Set-Content` without
  // `-Encoding utf8NoBOM`) starts with U+FEFF, and the reader would otherwise take it as part
  // of the first KEY — so `version: 1` arrived as a key named "﻿version", the version
  // read as missing, and the pane reported a file the human could see was right as broken.
  // It is invisible, so nothing about the error message could have led them to the cause.
  const reader = new YamlReader(text.replace(/^﻿/, "").split(/\r?\n/));
  const doc = reader.document();
  const findings = reader.findings;
  const w: Workflow = { version: WORKFLOW_VERSION, name: "", blocks: [], edges: [], gates: {} };

  if (doc === null || typeof doc !== "object" || Array.isArray(doc)) {
    if (text.trim()) {
      findings.push({
        severity: "error",
        code: "not-a-mapping",
        message: "A workflow file is a mapping with version:, blocks: and (optionally) edges: / gates:.",
      });
    }
    return { workflow: w, findings };
  }
  const root = doc as Record<string, YamlValue>;

  if (root.version === undefined) {
    findings.push({
      severity: "error",
      code: "version-missing",
      message: `No version: — this file should declare "version: ${WORKFLOW_VERSION}".`,
    });
  } else if (typeof root.version !== "number") {
    findings.push({
      severity: "error",
      code: "version-unsupported",
      message: `version: must be a number (found "${String(root.version)}").`,
    });
  } else {
    w.version = root.version;
    if (root.version !== WORKFLOW_VERSION) {
      findings.push({
        severity: "error",
        code: "version-unsupported",
        message: `version: ${root.version} is not supported by this build of loomux (it reads version ${WORKFLOW_VERSION}).`,
      });
    }
  }

  w.name = asString(root.name ?? "") ?? "";
  w.extra = collectExtra(root, KNOWN_TOP);

  // `blocks:` / `edges:` written with nothing after them are YAML null, and null here means
  // EMPTY — an empty roster, no edges. Only a value that is present and is not a list is a
  // shape error (rev-5 F4): reporting "must be a list" against an empty one would have the
  // pane complain about the file it just wrote itself when you delete the last block.
  const blocks = root.blocks;
  if (blocks !== undefined && blocks !== null && !Array.isArray(blocks)) {
    findings.push({
      severity: "error",
      code: "block-not-a-mapping",
      message: "blocks: must be a list of blocks.",
    });
  } else if (Array.isArray(blocks)) {
    blocks.forEach((raw, i) => w.blocks.push(readBlock(raw, i, findings)));
  }

  const edges = root.edges;
  if (edges !== undefined && edges !== null && !Array.isArray(edges)) {
    findings.push({
      severity: "error",
      code: "edge-not-a-mapping",
      message: "edges: must be a list of { from: …, to: … } entries.",
    });
  } else if (Array.isArray(edges)) {
    edges.forEach((raw, i) => w.edges.push(...readEdge(raw, i, findings)));
  }

  const gates = root.gates;
  if (gates !== undefined && (typeof gates !== "object" || gates === null || Array.isArray(gates))) {
    findings.push({
      severity: "error",
      code: "gate-unknown-require",
      message: "gates: must be a mapping (today the only gate is `merge`).",
    });
  } else if (gates && typeof gates === "object" && !Array.isArray(gates)) {
    const g = gates as Record<string, YamlValue>;
    if (g.merge !== undefined) w.gates.merge = readGate(g.merge, findings);
    w.gates.extra = collectExtra(g, KNOWN_GATE);
  }

  return { workflow: w, findings };
}

/** One block, ALWAYS — a malformed entry becomes a stub with the findings that explain
 *  it, never a dropped row. A block you cannot see is a block you cannot repair. */
function readBlock(raw: YamlValue, index: number, findings: Finding[]): WorkflowBlock {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    findings.push({
      severity: "error",
      code: "block-not-a-mapping",
      message: `blocks[${index}] is not a block mapping (expected id:, name:, kind:, cli: …).`,
    });
    return { id: "", name: `block ${index + 1}`, kind: "", cli: "", model: "" };
  }
  const r = raw as Record<string, YamlValue>;
  const id = asString(r.id ?? "") ?? "";
  const block: WorkflowBlock = {
    id,
    name: asString(r.name ?? "") ?? id,
    kind: asString(r.kind ?? "") ?? "",
    cli: asString(r.cli ?? "") ?? "",
    model: asString(r.model ?? "") ?? "",
    extra: collectExtra(r, KNOWN_BLOCK),
  };
  if (r.prompt !== undefined) block.prompt = asString(r.prompt) ?? "";
  if (r.profile !== undefined) block.profile = asString(r.profile) ?? "";
  return block;
}

/** `{ from: x, to: y }` or `{ from: x, to: [a, b] }` — the fan-out form expands into one
 *  flat edge per target, because that is what every graph question is asked of. */
function readEdge(raw: YamlValue, index: number, findings: Finding[]): WorkflowEdge[] {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    findings.push({
      severity: "error",
      code: "edge-not-a-mapping",
      message: `edges[${index}] is not a { from: …, to: … } mapping.`,
    });
    return [];
  }
  const r = raw as Record<string, YamlValue>;
  const from = asString(r.from ?? "") ?? "";
  const targets = Array.isArray(r.to) ? r.to : r.to === undefined ? [] : [r.to];
  if (!from || !targets.length) {
    findings.push({
      severity: "error",
      code: "edge-not-a-mapping",
      message: `edges[${index}] needs both a from: and a to:.`,
    });
    return [];
  }
  return targets.map((t) => ({ from, to: asString(t) ?? "" }));
}

function readGate(raw: YamlValue, findings: Finding[]): MergeGate {
  const gate: MergeGate = { require: "all-pass", reviewers: [], also: [] };
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    findings.push({
      severity: "error",
      code: "gate-unknown-require",
      message: "gates.merge must be a mapping (require:, reviewers:, …).",
    });
    return gate;
  }
  const r = raw as Record<string, YamlValue>;
  gate.require = asString(r.require ?? "all-pass") ?? "all-pass";
  if (typeof r.threshold === "number") gate.threshold = r.threshold;
  else if (r.threshold !== undefined) {
    findings.push({
      severity: "error",
      code: "gate-bad-threshold",
      message: `gates.merge.threshold must be a number (found "${String(r.threshold)}").`,
    });
  }
  const list = (v: YamlValue): string[] =>
    Array.isArray(v) ? v.map((x) => asString(x) ?? "").filter(Boolean) : [];
  gate.reviewers = list(r.reviewers ?? []);
  gate.also = list(r.also ?? []);
  return gate;
}

// ---------- validate: the pre-run pass ----------

/** Everything that is wrong with this workflow, before a single agent is spawned.
 *
 *  This is the pass every surveyed tool skipped (#222 §1a-v): Flowise, Langflow and Dify
 *  all discover a dangling reference at RUN time, and Dify will publish a workflow whose
 *  node isn't even installed. It is cheap, it is pure, and it is the difference between
 *  "your workflow failed after spawning two agents" and "block `rev-perf` doesn't exist —
 *  the merge gate names it". */
export function validateWorkflow(w: Workflow): Finding[] {
  const findings: Finding[] = [];
  const byId = new Map<string, WorkflowBlock>();

  if (!w.blocks.length) {
    findings.push({
      severity: "error",
      code: "no-blocks",
      message: "This workflow declares no blocks — add at least one agent block.",
    });
  }

  const seen = new Set<string>();
  for (const b of w.blocks) {
    const where = b.id || b.name;
    if (!b.id) {
      findings.push({
        severity: "error",
        code: "block-id-missing",
        message: `A block has no id. The id is the block's identity — edges and gates reference it.`,
        blockId: b.id,
      });
    } else if (!isValidBlockId(b.id)) {
      findings.push({
        severity: "error",
        code: "block-id-invalid",
        message: `"${b.id}" is not a valid block id — use lowercase letters, digits, - and _ (e.g. rev-security).`,
        blockId: b.id,
      });
    } else if (seen.has(b.id)) {
      findings.push({
        severity: "error",
        code: "block-id-duplicate",
        message: `Two blocks share the id "${b.id}" — an edge or a gate naming it would be ambiguous.`,
        blockId: b.id,
      });
    }
    if (b.id) {
      seen.add(b.id);
      if (!byId.has(b.id)) byId.set(b.id, b);
    }

    if (!isBlockKind(b.kind)) {
      findings.push({
        severity: "error",
        code: "unknown-kind",
        message: b.kind
          ? `Block "${where}" has kind "${b.kind}", which is not one of ${BLOCK_KINDS.join(", ")}. A workflow can define any persona, but never a new capability class.`
          : `Block "${where}" has no kind — pick one of ${BLOCK_KINDS.join(", ")}.`,
        blockId: b.id,
      });
    }
    if (!isWorkflowCli(b.cli)) {
      findings.push({
        severity: "error",
        code: "unknown-cli",
        message: b.cli
          ? `Block "${where}" runs cli "${b.cli}", which loomux cannot spawn (supported: ${WORKFLOW_CLIS.join(", ")}).`
          : `Block "${where}" has no cli — pick one of ${WORKFLOW_CLIS.join(", ")}.`,
        blockId: b.id,
      });
    }
    if (b.prompt !== undefined && b.profile !== undefined) {
      findings.push({
        severity: "error",
        code: "prompt-and-profile",
        message: `Block "${where}" declares both a prompt and a profile — pick one. (An inline prompt compiles to the CLI's native inline agent; a profile points at a file the CLI loads by name.)`,
        blockId: b.id,
      });
    }
  }

  for (const e of w.edges) {
    for (const [end, id] of [
      ["from", e.from],
      ["to", e.to],
    ] as const) {
      if (!byId.has(id)) {
        findings.push({
          severity: "error",
          code: "edge-unknown-block",
          message: `The edge ${e.from} → ${e.to} names a block that doesn't exist: "${id}" (${end}:).`,
        });
      }
    }
    if (e.from && e.from === e.to) {
      findings.push({
        severity: "error",
        code: "edge-self",
        message: `Block "${e.from}" has an edge to itself.`,
        blockId: e.from,
      });
    }
  }

  const gate = w.gates.merge;
  if (gate) {
    if (!(GATE_REQUIRES as readonly string[]).includes(gate.require)) {
      findings.push({
        severity: "error",
        code: "gate-unknown-require",
        message: `The merge gate requires "${gate.require}", which is not one of ${GATE_REQUIRES.join(", ")}.`,
      });
    }
    if (!gate.reviewers.length) {
      findings.push({
        severity: "error",
        code: "gate-no-reviewers",
        message: "The merge gate names no reviewers — a gate with nothing to wait for gates nothing.",
      });
    }
    for (const id of gate.reviewers) {
      const b = byId.get(id);
      if (!b) {
        findings.push({
          severity: "error",
          code: "gate-unknown-reviewer",
          message: `The merge gate requires a verdict from "${id}", but no block has that id — the gate could never open.`,
        });
      } else if (b.kind !== "reviewer") {
        findings.push({
          severity: "error",
          code: "gate-not-a-reviewer",
          message: `The merge gate names "${id}" as a reviewer, but that block's kind is "${b.kind || "(none)"}" — only a reviewer records a verdict.`,
          blockId: id,
        });
      }
    }
    if (gate.require === "threshold") {
      const t = gate.threshold;
      if (t === undefined || !Number.isInteger(t) || t < 1) {
        findings.push({
          severity: "error",
          code: "gate-bad-threshold",
          message: 'A "threshold" merge gate needs threshold: N with N ≥ 1.',
        });
      } else if (t > gate.reviewers.length) {
        findings.push({
          severity: "error",
          code: "gate-bad-threshold",
          message: `The merge gate needs ${t} passing reviews but names only ${gate.reviewers.length} reviewer(s) — it could never open.`,
        });
      }
    }
  }

  findings.push(...reachabilityFindings(w, byId));
  return findings;
}

/** The two structural warnings — a block nothing points at, and a block nothing can
 *  reach. Both are WARNINGS, not errors: edges are advisory (§2g), so an isolated block
 *  is a workflow the orchestrator can still run — it is just almost certainly a mistake
 *  (a fan-out you forgot to wire, a reviewer that will never be asked). */
function reachabilityFindings(w: Workflow, byId: Map<string, WorkflowBlock>): Finding[] {
  const out: Finding[] = [];
  if (!w.edges.length || w.blocks.length < 2) return out;

  const ids = [...byId.keys()];
  // Nothing here has an ID, so there is no graph to reason about — every edge is dangling
  // and `edge-unknown-block` has already said so. Without this, `entries` came out empty
  // and we announced that "every block is pointed at by another", which is neither true nor
  // useful about a file whose blocks have no identities yet (rev-5 F6).
  if (!ids.length) return out;
  const inDeg = new Map(ids.map((id) => [id, 0]));
  const outAdj = new Map<string, string[]>(ids.map((id) => [id, []]));
  for (const e of w.edges) {
    if (!byId.has(e.from) || !byId.has(e.to) || e.from === e.to) continue;
    inDeg.set(e.to, (inDeg.get(e.to) ?? 0) + 1);
    outAdj.get(e.from)!.push(e.to);
  }

  for (const id of ids) {
    if (inDeg.get(id) === 0 && outAdj.get(id)!.length === 0) {
      out.push({
        severity: "warning",
        code: "isolated-block",
        message: `Block "${id}" has no edges — nothing declares when it runs.`,
        blockId: id,
      });
    }
  }

  // Entries are the blocks nothing points at. A workflow with none is all cycles — the
  // rework loop (worker ⇄ reviewer) is a legitimate cycle, so a cycle is not itself a
  // finding; having NOWHERE TO START is.
  const entries = ids.filter((id) => inDeg.get(id) === 0);
  if (!entries.length) {
    out.push({
      severity: "warning",
      code: "no-entry-block",
      message: "Every block is pointed at by another — the declared path has no starting point.",
    });
    return out;
  }

  const reached = new Set<string>(entries);
  const queue = [...entries];
  while (queue.length) {
    const id = queue.shift()!;
    for (const next of outAdj.get(id)!) {
      if (!reached.has(next)) {
        reached.add(next);
        queue.push(next);
      }
    }
  }
  for (const id of ids) {
    if (!reached.has(id)) {
      out.push({
        severity: "warning",
        code: "unreachable-block",
        message: `Block "${id}" is unreachable — no path leads to it from a starting block.`,
        blockId: id,
      });
    }
  }
  return out;
}

// ---------- the derived graph (read-only) ----------

export interface GraphNode {
  block: WorkflowBlock;
  /** The block's INDEX in the roster — its identity in the picture. Not its id: the blocks
   *  that most need drawing are the broken ones, and two id-less stubs (or a duplicate-id
   *  pair) share an id while being two different rows. Keying the graph by id drew them on
   *  top of each other, so a file with two stubs showed one (rev-5 F5) — in the very view
   *  whose job is to let you SEE the file. The roster already keys by index for exactly
   *  this reason; now the graph agrees with it. */
  index: number;
  /** False when the block's kind isn't a capability class — the view draws it as a stub. */
  known: boolean;
  /** Column in the layered layout: distance from the nearest entry block. */
  layer: number;
}

export interface GraphEdge extends WorkflowEdge {
  /** False when either end names a block that doesn't exist (the view draws it dangling). */
  resolved: boolean;
}

export interface GraphGate {
  /** The gate's name — today, always "merge". */
  name: string;
  require: string;
  threshold?: number;
  reviewers: string[];
}

export interface WorkflowGraph {
  nodes: GraphNode[];
  edges: GraphEdge[];
  gates: GraphGate[];
  /** Block INDICES grouped by layer, left to right (see GraphNode.index). */
  layers: number[][];
}

/** Derive the picture: the blocks, the advisory edges between them, and the enforced
 *  gates hanging off the reviewers they name. READ-ONLY by design (#222 Q6) — the graph
 *  is a view over the file, like GitLab's CI "Visualize" tab, not an editable canvas that
 *  can corrupt it. Layering is longest-path from the entry blocks, with cycles (the
 *  worker ⇄ reviewer rework loop) resolved by leaving the back-edge's target where its
 *  forward path put it. */
export function deriveGraph(w: Workflow): WorkflowGraph {
  const byId = new Map<string, WorkflowBlock>();
  for (const b of w.blocks) if (b.id && !byId.has(b.id)) byId.set(b.id, b);

  const edges: GraphEdge[] = w.edges.map((e) => ({
    ...e,
    resolved: byId.has(e.from) && byId.has(e.to),
  }));

  // Layering is computed over IDS — an edge names ids, so that is what a column can be
  // derived from — and then handed to the NODES, which are rows. The two are different
  // things, and conflating them is what stacked the broken blocks on one another.
  const layer = new Map<string, number>();
  for (const b of w.blocks) if (b.id) layer.set(b.id, 0);

  // Relax forward edges |blocks| times: a node sits one column right of its deepest
  // predecessor. Bounded, so a cycle terminates instead of spinning.
  for (let pass = 0; pass < w.blocks.length; pass++) {
    let moved = false;
    for (const e of edges) {
      if (!e.resolved || e.from === e.to) continue;
      const want = (layer.get(e.from) ?? 0) + 1;
      if (want > (layer.get(e.to) ?? 0)) {
        layer.set(e.to, want);
        moved = true;
      }
    }
    if (!moved) break;
  }

  // An id-less block has no column of its own to compute (nothing can point at it), so it
  // sits in the first one — visible, drawn as the stub it is, next to the finding that says
  // to give it an id.
  const nodes: GraphNode[] = w.blocks.map((b, index) => ({
    block: b,
    index,
    known: isBlockKind(b.kind),
    layer: (b.id && layer.get(b.id)) || 0,
  }));

  const depth = nodes.reduce((m, n) => Math.max(m, n.layer), 0);
  const layers: number[][] = Array.from({ length: depth + 1 }, () => []);
  for (const n of nodes) layers[n.layer]!.push(n.index);

  const gates: GraphGate[] = w.gates.merge
    ? [
        {
          name: "merge",
          require: w.gates.merge.require,
          threshold: w.gates.merge.threshold,
          reviewers: w.gates.merge.reviewers,
        },
      ]
    : [];

  return { nodes, edges, gates, layers };
}

// ---------- editing helpers (used by the form; pure, so they are tested) ----------

/** A fresh block id derived from `base`, unique within `w`. Ids are IMMUTABLE once a
 *  block exists (rule 1 at the top of this file), so this runs exactly once per block —
 *  at creation — and never again. */
export function nextBlockId(w: Workflow, base: string): string {
  const slug =
    base
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .replace(/^([^a-z])/, "b$1") || "block";
  const taken = new Set(w.blocks.map((b) => b.id));
  if (!taken.has(slug)) return slug;
  for (let n = 2; ; n++) {
    const candidate = `${slug}-${n}`;
    if (!taken.has(candidate)) return candidate;
  }
}

// ---------- graph EDIT operations (#222 v2: the canvas edits the file) ----------
//
// The canvas is now bidirectional — you can draw an edge, add a block, delete either — and
// every one of those goes through a function here, in the pure module, rather than through
// the DOM layer poking at the model. That is what makes "draw an edge, serialize, re-read,
// get the same workflow" a unit test instead of a thing you check by hand with a mouse.
//
// They all return a NEW workflow, and none of them is allowed to invent an identity: a block
// gets its id from the human (immutable, human-meaningful — §4), and an edge is a pair of ids
// that already exist.

/** Why a proposed edge can't be drawn, or null when it can. Checked BEFORE the edge is
 *  created rather than reported after — an editable canvas that lets you draw an edge and
 *  then tells you it was invalid has wasted the gesture and left you to undo it. */
export function connectionError(w: Workflow, from: string, to: string): string | null {
  if (!from || !to) return "A block needs an id before an edge can name it.";
  if (from === to) return "A block can't run after itself.";
  if (!w.blocks.some((b) => b.id === from) || !w.blocks.some((b) => b.id === to)) {
    return "That block doesn't exist.";
  }
  if (w.edges.some((e) => e.from === from && e.to === to)) return "That edge already exists.";
  return null;
}

/** Draw an advisory edge. A duplicate or illegal edge is a no-op rather than a throw — the
 *  canvas has already refused the gesture (`connectionError`), and this is the second line of
 *  defence, not the first. */
export function connectBlocks(w: Workflow, from: string, to: string): Workflow {
  if (connectionError(w, from, to)) return w;
  return { ...w, edges: [...w.edges, { from, to }] };
}

/** Erase an edge. Only that edge: the blocks it joined are untouched, which is the whole
 *  difference between deleting a connection and deleting the work. */
export function disconnectBlocks(w: Workflow, from: string, to: string): Workflow {
  return { ...w, edges: w.edges.filter((e) => !(e.from === from && e.to === to)) };
}

/** Add a block. The caller supplies the ID — the canvas asks the human for it, because §4's
 *  first commitment is that an id is human-meaningful and immutable, and a canvas that mints
 *  `node_1720794829558` (Dify's actual behaviour) makes every edge in the file unreadable
 *  and every id a lie about what the block is. */
export function addBlock(w: Workflow, block: WorkflowBlock): Workflow {
  return { ...w, blocks: [...w.blocks, block] };
}

/** A new block, filled in with the defaults a reviewer usually wants — the caller overrides
 *  what it asked the human about. Kept here so "what a new block is" has one answer. */
export function newBlock(id: string, name: string, kind: BlockKind = "reviewer"): WorkflowBlock {
  return { id, name: name || id, kind, cli: "claude", model: "" };
}

/** Remove the block at `index`, AND every reference to it — edges at either end, and its
 *  seat on the merge gate. A delete that left the references behind would turn one click
 *  into three validation errors, which is exactly the "dangling reference" class this file
 *  exists to prevent (Dify ships it; we don't).
 *
 *  By INDEX, not by id, and both halves of that matter:
 *   - an id-LESS stub (a block the file got wrong) has no id to delete by, and deleting
 *     "every block whose id is empty" would take its siblings with it;
 *   - a DUPLICATE id survives its own deletion — the other block still answers to it — so
 *     the references are still meaningful and must NOT be stripped. Hence `gone`. */
export function removeBlockAt(w: Workflow, index: number): Workflow {
  const block = w.blocks[index];
  if (!block) return w;
  const blocks = w.blocks.filter((_, i) => i !== index);
  const id = block.id;
  const gone = !!id && !blocks.some((b) => b.id === id);
  const gate = w.gates.merge;
  return {
    ...w,
    blocks,
    edges: gone ? w.edges.filter((e) => e.from !== id && e.to !== id) : w.edges,
    gates: {
      ...w.gates,
      merge: gate
        ? { ...gate, reviewers: gone ? gate.reviewers.filter((r) => r !== id) : gate.reviewers }
        : undefined,
    },
  };
}

/** The file a repo with no workflow gets when the human asks for one: today's built-in
 *  pipeline, written out — plus the comments that say what each part is FOR.
 *
 *  Comments, and not just `serializeWorkflow(starterWorkflow())`, because this is the one
 *  moment the file is read by someone who has never seen the schema: it arrives in their
 *  editor, in their diff, in their teammate's `git pull`. A commented scaffold is how every
 *  config-as-code tool worth using introduces itself, and it costs one string.
 *
 *  (They are comments, so they do not survive a canonical re-serialize — the first form edit
 *  rewrites the file without them. That is the honest trade of having ONE canonical shape,
 *  it is stated in the design note, and it is why the scaffold is offered at CREATION rather
 *  than being something the formatter tries to preserve. What the human writes in the YAML
 *  tab and saves is kept verbatim; only an edit made through the form or the canvas
 *  re-serializes.)
 *
 *  `authoredWith` is stamped in the same one moment `starterWorkflow` stamps it. */
export function scaffoldWorkflowText(authoredWith?: string): string {
  const stamp = authoredWith ? `authored_with: ${authoredWith}\n` : "";
  return `# .loomux/workflow.yml — this repo's agent workflow (loomux #222).
# Committed on purpose: everyone who clones the repo gets the same roster.
# Loomux reads it only when "Advanced orchestrator" is ticked in the launcher.

version: 1
${stamp}name: default

# BLOCKS — the agents a run may use. \`kind\` is a capability class and the list is
# closed (orchestrator | worker | reviewer | planner): a workflow file can define any
# persona, but it can never grant a capability. A planner is read-only; a reviewer can
# review but never push; a worker gets a worktree.
blocks:
  - id: planner            # immutable, human-meaningful — edges and gates name THIS
    name: Planner          # display only; safe to rename at any time
    kind: planner
    cli: claude
    model: opus

  - id: worker
    name: Worker
    kind: worker
    cli: claude

  - id: reviewer
    name: Reviewer
    kind: reviewer
    cli: claude
    model: opus
    # A persona is optional: an inline \`prompt:\` (compiled to the CLI's native inline
    # agent) or a \`profile:\` path to a .github/agents/*.md file. Omit both and the
    # block runs loomux's built-in role instructions.
    #
    # prompt: |
    #   Review ONLY for security defects: injection, authz, secrets, path traversal.

# EDGES — ADVISORY. They declare the intended path; the orchestrator still decides when
# to spawn what. (Its judgment about what can run in parallel is the thing that makes it
# good — a static DAG would replace that with something dumber.)
edges:
  - { from: planner, to: worker }
  - { from: worker, to: reviewer }

# GATES — ENFORCED. Loomux refuses \`gh pr merge\` until every reviewer named here has
# recorded a PASS verdict. An agent cannot get around it: the refusal lives in the PATH
# shim, not in a prompt. Add a second reviewer to the list and it is a second reviewer
# that must actually pass — which is what makes multi-reviewer more than theatre.
gates:
  merge:
    require: all-pass      # or: threshold, with \`threshold: N\`
    reviewers: [reviewer]
`;
}

/** The optional top-level key recording which loomux WROTE this file — §4's "record the
 *  loomux version that authored it" (the Langflow `last_tested_version` lesson: when a file
 *  misbehaves, the first question is always which build produced it).
 *
 *  It is written EXACTLY ONCE, when the pane creates a new workflow, and never touched
 *  again: on an existing file it rides the unknown-key bag and round-trips verbatim. That
 *  is deliberate — stamping it on every save would mean every human who opens the pane and
 *  changes a model name also produces a one-line diff nobody asked for, in a file whose
 *  whole point is a legible history. It records who authored the workflow, not who last
 *  looked at it. (Deliberately NOT in KNOWN_TOP: the preservation path already handles it,
 *  and the backend — sub-PR 1 — owns whatever meaning it ever grows.) */
export const AUTHORED_WITH_KEY = "authored_with";

/** The workflow loomux runs today, as a file: plan → work → review, with the reviewer's
 *  verdict gating the merge. The starting point a repo with no `.loomux/workflow.yml`
 *  opens on, so the pane's empty state is a working example rather than a blank page.
 *
 *  `authoredWith` is the loomux version doing the creating; omit it and the key is simply
 *  not written (which is what the tests do, and what a caller with no version to hand
 *  should do — an `authored_with: unknown` would be worse than an absent key). */
export function starterWorkflow(authoredWith?: string): Workflow {
  return {
    version: WORKFLOW_VERSION,
    name: "default",
    ...(authoredWith ? { extra: { [AUTHORED_WITH_KEY]: authoredWith } } : {}),
    blocks: [
      { id: "planner", name: "Planner", kind: "planner", cli: "claude", model: "opus" },
      { id: "worker", name: "Worker", kind: "worker", cli: "claude", model: "" },
      { id: "reviewer", name: "Reviewer", kind: "reviewer", cli: "claude", model: "opus" },
    ],
    edges: [
      { from: "planner", to: "worker" },
      { from: "worker", to: "reviewer" },
    ],
    gates: { merge: { require: "all-pass", reviewers: ["reviewer"], also: [] } },
  };
}

// ---------- the one call the view makes ----------

export interface WorkflowAnalysis {
  workflow: Workflow;
  /** Parse findings and validation findings, in that order — syntax first, because a
   *  file that didn't parse will also fail half the semantic rules and leading with those
   *  would bury the line number that actually explains it. */
  findings: Finding[];
  graph: WorkflowGraph;
}

/** Text in, everything the pane renders out. */
export function analyzeWorkflow(text: string): WorkflowAnalysis {
  const { workflow, findings } = parseWorkflow(text);
  return {
    workflow,
    findings: [...findings, ...validateWorkflow(workflow)],
    graph: deriveGraph(workflow),
  };
}

/** The canonical formatter, as the pane's ✨ Format button uses it: read the file, write
 *  it back in the one canonical shape. Idempotent by construction. */
export function formatWorkflowText(text: string): string {
  return serializeWorkflow(parseWorkflow(text).workflow);
}
