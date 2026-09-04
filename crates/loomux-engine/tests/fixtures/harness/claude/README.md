# Claude Code stream-json fixtures

**These lines are SYNTHETIC. They were not recorded from a `claude` process.**

Every field in `one-turn.jsonl` is built from the official documentation, read
2026-09-03 — the message shapes from
[agent-sdk/typescript](https://code.claude.com/docs/en/agent-sdk/typescript)
(`SDKSystemMessage`, `SDKAssistantMessage`, `SDKUserMessage`,
`SDKPartialAssistantMessage`, `SDKResultMessage`, `ModelUsage`), the stream's
framing from [headless](https://code.claude.com/docs/en/headless). No agent CLI
was run to produce them: CLAUDE.md constraint 3 forbids it, and it would have
spent the user's credits.

## What that means for what they prove

They prove the decoder against the **documented** contract. They cannot prove it
against the real CLI's bytes — a field the docs describe loosely, a key the docs
name but spell differently in practice, or a message the docs do not mention at
all would pass here and fail live.

`doc/design/harness-adapters.md` §8.1 contracts a human-recorded capture, and
replacing this file with one is a live-validation item in the same family as that
note's §9. Until then, treat a green decoder suite as "matches the docs", not as
"matches the CLI".

## Rules for editing

- **Agents never regenerate these.** A fixture regenerated from the code under
  test asserts nothing — it is a snapshot of today's behaviour wearing an
  expectation's clothes.
- A capture that replaces this file records the **CLI version** it came from, in
  this README, the way `doc/design/opencode.md` labels its source-read evidence.
- Keep the deliberate awkward cases. `one-turn.jsonl` carries, on purpose:
  a `thinking_delta` (which must NOT become transcript text), a **failing**
  `tool_result` (`is_error: true` — the branch an all-success capture never
  reaches), an `api_retry` (a real message that is not a pane event), a message
  type no build knows (which must be kept as evidence, not dropped), an unknown
  `capabilities` entry (an open set), and **two** models in `modelUsage` (so a
  reader that folds only the first is caught).

## Files

| file | what it is |
| --- | --- |
| `one-turn.jsonl` | one complete turn: init → streamed text → text + tool call → failing tool result → retry → unknown message → text → result |
