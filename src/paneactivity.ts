// Per-pane output/input activity, reduced to the handful of facts the Agents
// tab (#2122) and the pane Notes rows (#2116) need to say what a pane is doing
// (#2122 slice A2). DOM-free and clock-injected — every entry point takes its
// `nowMs` from the caller, so `test/paneactivity.test.ts` drives the whole
// state machine in `node --test` without waiting a second per assertion
// (`framegate.ts` / `liveness.ts` are the precedent).
//
// WHAT THIS IS NOT. It reads no screen, opens no PTY probe and issues no IPC.
// Every input is a signal the pane already receives: the output chunks
// `acceptOutput` is already handed, the human-input mark `markFirstInput`
// already makes, the attention reason `setAttention` is already told, and the
// roster's own idleness reading off the tab-strip poll. Nothing here costs a
// byte that was not already arriving.
//
// THE `atPrompt` LATCH IS THE DESIGN DECISION. The backend's `waiting`
// attention reason already means "parked at a prompt": `attention_tick` /
// `plain_pane_attention` raise it when output has been quiet for
// `ATTENTION_QUIET_MS`, a prompt shape is on the masked tail, and no keystroke
// landed inside `ATTENTION_RECENT_INPUT_MS`. But that reason is ACKED ON FOCUS
// (`Pane.acknowledgeAttention` -> `attn_waiting_ack`), so reading it directly would
// flip a still-parked pane back to "working" the instant the human clicks it —
// the click is not evidence the agent resumed. So a `waiting` sighting LATCHES
// here, and the latch clears only on evidence that is independent of the
// signal that set it:
//
//   1. human input (`noteHumanInput`) — the human typed, so the turn is theirs;
//   2. at least `ACTIVITY_FLOOR_BYTES` of output inside one contiguous BURST —
//      the pane is painting something bigger than an idle repaint. A burst is
//      chunks arriving less than `ACTIVITY_WINDOW_MS` apart: they keep topping
//      up one count, and the first chunk after a longer gap starts over. It is
//      deliberately NOT a fixed window, so a pane repainting steadily at, say,
//      3.9 s intervals does eventually cross the floor — a terminal painting
//      without pause for a minute is not a parked one, and the direction that
//      reading fails in is `working`, never a false turn-done.
//
// Both clears are bounded and signal-independent, which is what
// `.orrerix/lessons.md` requires of any suppression driven by a fallible
// signal: neither depends on the attention scan noticing anything, so the
// latch can never be held on by the scan going quiet.
//
// NO RESET ON RESPAWN, deliberately. `lastHumanInputMs` is a PANE fact, not a
// process one — the same reasoning `humanOrigin` carries in `pane.ts` ("it
// belongs to the PANE, not to one process") — and the latch needs none either:
// a respawned agent CLI repaints far more than the floor on boot, which is
// clear (2) above, and a respawned plain shell that comes back sitting at a
// prompt is correctly still `atPrompt`. Nothing stays latched without the pane
// genuinely being parked.

/** Bytes of output inside one contiguous burst (see `ACTIVITY_WINDOW_MS`) that
 *  count as "this pane is doing work", rather than repainting an idle input box.
 *
 *  DUPLICATED from the backend's `DEFAULT_IDLE_ACTIVITY_FLOOR_BYTES`
 *  (`src-tauri/src/orchestration/mod.rs`), where the number was measured: a
 *  full idle Claude Code input-box repaint is ~164 bytes
 *  (`src-tauri/tests/fixtures/attention/idle-input-box.txt`), so 2048 clears it
 *  by an order of magnitude while still sitting far under any real turn's
 *  output. Duplicated rather than plumbed for the reason `DOCK_TERM_RESERVE_PX`
 *  is, and pinned against the Rust literal by `test/paneactivity.test.ts`,
 *  which reads `mod.rs` off disk so the two defaults cannot drift silently.
 *
 *  RESIDUAL, stated because the Rust side is NOT a bare const: the backend's
 *  floor is a live-tunable guardrail knob (`Guardrails.idle_activity_floor_bytes`
 *  / `set_idle_activity_floor`), so a group that raises its own floor is running
 *  a value this copy does not read. Nothing here plumbs it — that would be a
 *  wire read per pane per second for a number nobody has yet moved. The
 *  divergence is bounded and it fails in the safe direction: a frontend floor
 *  BELOW the backend's clears the latch earlier, so the pane reads `working`,
 *  which is the ladder's honest "no evidence of a prompt" — never a turn-done
 *  claim that is not true. */
export const ACTIVITY_FLOOR_BYTES = 2048;

/** The longest gap between two chunks that still counts as ONE burst of output
 *  for the floor above. A pane quiet for longer than this has finished whatever
 *  it was painting, so the next chunk starts the count over rather than topping
 *  up a stale total — otherwise a pane dribbling 200 bytes a minute would
 *  eventually cross the floor and read as work.
 *
 *  This is a gap bound, not a fixed window: chunks arriving closer together than
 *  this keep accumulating for as long as they keep coming, so a pane repainting
 *  steadily just under the bound DOES cross the floor eventually. That is the
 *  intended reading — a terminal painting without pause is not parked — and it
 *  fails toward `working`, never toward a turn-done claim that is not true
 *  (#2195 review, rev-std finding 1, which is why this doc says so explicitly).
 *
 *  4000 ms is the backend's own definition of "this pane has gone quiet"
 *  (`ATTENTION_QUIET_MS`, `mod.rs`) — the same threshold that decides a
 *  `waiting` sighting is worth raising at all, so the gap this latch is cleared
 *  over is the gap it was set over. Pinned against that Rust literal too.
 *
 *  It is NOT the backend's own floor rule, which is a different instrument for a
 *  different job: `idle_output_is_activity` compares total growth between two
 *  scan ticks. Named here so a reader does not assume the two are one mechanism. */
export const ACTIVITY_WINDOW_MS = 4000;

/** The plain-data reading `PaneFacts.activity` carries — resolved AT `nowMs`,
 *  so every consumer reads the same numbers rather than each re-deriving the
 *  window arithmetic against its own clock. */
export interface ActivitySnapshot {
  /** When output last arrived, or null if none ever has. */
  readonly lastOutputMs: number | null;
  /** Output bytes accumulated in the CURRENT burst — chunks less than
   *  `ACTIVITY_WINDOW_MS` apart — and 0 once that burst has ended. Named
   *  `bytesInWindow` for the plan's field list; the bound is a gap between
   *  chunks, not a fixed window, and `ACTIVITY_WINDOW_MS` says why. */
  readonly bytesInWindow: number;
  /** When the human last typed/pasted into this pane, or null if they never
   *  have. Never set by program-generated data — see `noteHumanInput`. */
  readonly lastHumanInputMs: number | null;
  /** The latch: this pane is believed parked at a prompt waiting on a human. */
  readonly atPrompt: boolean;
  /** The roster's own reading for this pane's agent — "the reaper would call
   *  this idle / it holds no assignment". `null` for a pane the roster does
   *  not cover (every non-orchestration pane, and an orchestration pane before
   *  the first strip poll lands). Explicitly NOT "at a prompt" (#2089): it
   *  feeds the `idle` rung only, never `turn-done`. */
  readonly rosterIdle: boolean | null;
}

/** Are two output timestamps close enough to be the same burst?
 *
 *  THE CLOCK CAN GO BACKWARDS, and this is the one place that matters. `Pane`
 *  feeds this reducer `Date.now()` — wall-clock, so an NTP correction or a
 *  laptop resume moves it, unlike the `performance.now()` the render throttle
 *  uses. `Date.now()` is nonetheless the right source here, because
 *  `lastOutputMs` and `lastHumanInputMs` are reported to consumers and must be
 *  comparable with `firstInputMs`, which is wall-clock throughout `pane.ts`.
 *  What is NOT acceptable is letting a jump decide the burst: a plain
 *  `now - last > BOUND` reads a backward jump as a NEGATIVE gap, i.e. "still
 *  the same burst", so sub-floor repaints would accumulate across the jump and
 *  eventually clear a latch that should have held.
 *
 *  So a non-monotonic reading is treated as a burst BOUNDARY, not a
 *  continuation. That fails toward holding the latch — the pane keeps reading
 *  `turn-done`, which is the state the human is being asked about — rather than
 *  toward silently declaring work that never happened (#2195 review premortem).
 *  A forward jump needs no special case: it simply ends the burst, which is
 *  what a long gap means anyway. */
function withinBurst(lastMs: number, nowMs: number): boolean {
  const gap = nowMs - lastMs;
  return gap >= 0 && gap <= ACTIVITY_WINDOW_MS;
}

/** One pane's activity state machine. Owned by `Pane`, fed from the sites
 *  named in the module header, read back through `Pane.facts()`. */
export class PaneActivity {
  private lastOutputMs: number | null = null;
  private bytesSinceWindowStart = 0;
  private lastHumanInputMs: number | null = null;
  private atPrompt = false;
  private rosterIdle: boolean | null = null;

  /** One arrived chunk of PTY output. Called per chunk from `acceptOutput` — a
   *  counter add and a comparison, on the hottest path in the app, which is why
   *  nothing here allocates or touches the DOM. */
  noteOutput(bytes: number, nowMs: number): void {
    if (bytes <= 0) return;
    // A gap longer than the bound means the previous burst is over, so this
    // chunk starts the count over rather than topping up a stale total.
    if (this.lastOutputMs === null || !withinBurst(this.lastOutputMs, nowMs)) {
      this.bytesSinceWindowStart = 0;
    }
    this.lastOutputMs = nowMs;
    this.bytesSinceWindowStart += bytes;
    // Clear (2): output big enough to be a real repaint, not an idle box.
    if (this.bytesSinceWindowStart >= ACTIVITY_FLOOR_BYTES) this.atPrompt = false;
  }

  /** The human typed or pasted into this pane. Wired ONLY from `pane.ts`'s
   *  `markFirstInput` / `markHumanInput` — the two functions that already own
   *  the single answer to "what counts as human input" — and NEVER from
   *  `term.onData`, which also fires for xterm's own DA/OSC auto-replies. That
   *  is #440 B2-R's structural guarantee, and it is exactly why copilot's
   *  boot-time colour queries (#179) cannot un-park a pane here. */
  noteHumanInput(nowMs: number): void {
    this.lastHumanInputMs = nowMs;
    // Clear (1): the turn is the human's now, whatever the scan last said.
    this.atPrompt = false;
  }

  /** The attention reason the backend last reported for this pane, or null.
   *
   *  Only `waiting` moves anything, and only in one direction. A `null` here
   *  is the FOCUS ACK — the human clicked the pane, which is not evidence the
   *  agent started working — so it deliberately does NOT clear the latch. Nor
   *  does any other reason: an urgent or question state outranks `turn-done`
   *  on the ladder anyway, so there is nothing to gain by forgetting a park
   *  that is still true underneath it. */
  noteAttention(reason: string | null): void {
    if (reason === "waiting") this.atPrompt = true;
  }

  /** The roster's idleness reading for this pane's agent, from the tab-strip
   *  poll (`StripViewPayload.groups[g].summary.agents[].idle_since_ms`). Pass
   *  `null` for a pane the roster does not cover. */
  noteRosterIdle(idle: boolean | null): void {
    this.rosterIdle = idle;
  }

  /** The reading at `nowMs`, with the window arithmetic already resolved. */
  snapshot(nowMs: number): ActivitySnapshot {
    // Same burst rule as `noteOutput`, through the same function — asking "is
    // this still one burst" by two expressions is how the two drift apart, and
    // a reader must not see a byte count the next chunk would have discarded.
    const ended = this.lastOutputMs === null || !withinBurst(this.lastOutputMs, nowMs);
    return {
      lastOutputMs: this.lastOutputMs,
      bytesInWindow: ended ? 0 : this.bytesSinceWindowStart,
      lastHumanInputMs: this.lastHumanInputMs,
      atPrompt: this.atPrompt,
      rosterIdle: this.rosterIdle,
    };
  }
}
