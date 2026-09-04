// The set of CLIs a `listSessions()` row can name — one definition, in a module
// that imports nothing, so every consumer can reach it without an import cycle.
//
// WHY IT IS ITS OWN FILE (#2126 P2, review round 1 finding 2). This union has two
// consumers that sit on opposite sides of an existing dependency edge:
// `pty.ts` declares the wire row (`SessionInfo["source"]`), and
// `sessionreconcile.ts` matches panes against it (`Cli`). Spelling it out twice
// is the failure #722 had to catch by hand — a scanner added on the Rust side
// widens one and the other silently stops adopting that CLI's panes — but making
// the reconciler import the type from `pty.ts` closes that hole by opening
// another: it is a new edge in the repo's TypeScript import graph, and
// `scripts/code-metrics.cjs` reports the resulting cycle on every PR. That census
// is how the repo watches its own module graph, so quietening it by teaching it
// to skip type-only edges would trade a real signal for this one convenience.
//
// A leaf has neither problem. `import type` from here erases at runtime and adds
// no cycle at build time, because this module has nothing to point back with.
//
// **Mirrors `SessionInfo.source` in `src-tauri/src/sessions.rs`, and nothing
// checks the two against each other** — the row crosses IPC as a plain string.
// A source added there without a widening here is a row the frontend
// mis-handles rather than rejects, which is why both halves land in one PR.
export type SessionSource = "claude" | "copilot" | "opencode" | "pi";
