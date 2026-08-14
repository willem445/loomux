//! Composed-screen reconstruction for `get_output` (#520).
//!
//! ## Why this exists
//!
//! `get_output` used to hand the orchestrator `strip_ansi(raw_ring)` — the raw
//! write stream with escape sequences deleted — and then collapse *consecutive
//! identical* lines (`orchestration::collapse_repeated_frames`). That works for a CLI
//! whose animation redraws a whole line per tick. It does nothing at all for a
//! modern TUI, and Claude Code v2.1.x is the worst case (#520, observed live):
//!
//! - it repaints **partial lines** by cursor address, so after stripping the
//!   escapes the surviving text of two consecutive frames concatenates into
//!   ever-different strings — no two lines are ever byte-identical and a
//!   line-identity dedup never fires;
//! - the spinner **verb cycles** (`Shenaniganing…` → `Roosting…`) and a token
//!   counter ticks, so even a full-line repaint is never a repeat;
//! - the input box holding the (long) delivered prompt is repainted every
//!   frame, so the prompt text multiplies 5-10x across the capture.
//!
//! Two 30-line calls cost the orchestrator ~24k tokens.
//!
//! ## What this does instead
//!
//! Deleting escape sequences throws away the exact information that makes the
//! stream readable: *where each write landed*. A redraw is only noise because
//! it overwrites something — replay the writes onto a screen and the overwrite
//! happens, exactly as it does in the human's terminal. So this module is a
//! small, dependency-free VT replay: feed it the raw ring, get back the
//! composed grid (scrolled-off history rows + the final on-screen rows). Three
//! hundred spinner frames painted over each other collapse to the one frame
//! that is actually on screen, because that is genuinely all that is there.
//!
//! This is the issue's preferred fix shape, and it is CLI-generic in the
//! strongest sense (CLAUDE.md constraint 8): it knows nothing about spinners,
//! verbs, or any CLI's vocabulary — only about ECMA-48. A CLI loomux has never
//! seen gets the same treatment.
//!
//! ## Deliberate limits
//!
//! - **Not a terminal emulator.** SGR/colour, character sets, wide-char widths,
//!   and mouse/bracketed-paste modes are parsed only far enough to be skipped;
//!   we render text placement, not appearance. A double-width CJK glyph
//!   occupies one cell here and two in the real pane, which can shift a row's
//!   trailing text — cosmetic in a monitoring read.
//! - **Starts blind.** The ring is a 256 KB *tail*, so replay begins mid-stream
//!   against a blank grid with the cursor home. Absolute-addressed paints land
//!   correctly; a relative move off the top edge clamps instead of reaching
//!   content that scrolled out of the ring long ago.
//! - **`get_output` reads [`render_screen`]; the question guard reads
//!   [`render_visible`].** `orchestration::strip_ansi` is unchanged and still serves
//!   every other caller (`box_holds_paste`, the compact/menu detectors). #530
//!   changed only what the orchestrator *reads*; #534 is the first place a
//!   composed screen also informs what loomux *decides*, and it does so through
//!   [`render_visible`] — see that function's doc for why the two must not be
//!   confused.

use std::collections::VecDeque;

/// Fallback geometry when the live pty size can't be read. 80x24 is the
/// ANSI default a CLI assumes before it learns better; a wrong width only
/// wraps long prose a column early, it never loses content.
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;

/// Ceiling on retained scrolled-off rows. The input is a bounded ring
/// (256 KB), so this can only bind on a pathological stream of very short
/// lines — where the byte cap in `format_output_tail` is the real answer
/// anyway. Present so replay cost stays O(ring), not O(ring) memory *and* an
/// unbounded Vec of empty strings.
const MAX_HISTORY_ROWS: usize = 4096;

/// Hard ceiling on the grid we will allocate, whatever geometry we are handed.
/// A pty resize races this read; a nonsense size must degrade, not allocate.
const MAX_COLS: usize = 1000;
const MAX_ROWS: usize = 300;

fn blank_row(cols: usize) -> Vec<char> {
    vec![' '; cols]
}

fn row_text(row: &[char]) -> String {
    let s: String = row.iter().collect();
    s.trim_end().to_string()
}

struct Screen {
    cols: usize,
    rows: usize,
    grid: Vec<Vec<char>>,
    /// Rows that scrolled off the top of the primary screen, oldest first.
    /// A deque, not a `Vec`: eviction happens at the OLDEST end on a hot path
    /// (once per scrolled row), and only `pop_front` makes that O(1).
    history: VecDeque<String>,
    row: usize,
    col: usize,
    /// ECMA-48 deferred wrap: writing the last column leaves the cursor
    /// *on* it with a pending wrap, so a line that exactly fills the width
    /// doesn't emit a spurious blank row.
    pending_wrap: bool,
    /// DECSTBM margins, inclusive row indices.
    scroll_top: usize,
    scroll_bot: usize,
    saved_cursor: Option<(usize, usize)>,
    /// Primary screen, parked while an alternate screen is active.
    parked: Option<Vec<Vec<char>>>,
    /// Whether rows scrolling off the top are retained (#534). A
    /// [`render_visible`] replay sets this false: it answers "what is on
    /// screen NOW", so a scrolled-off row is not merely uninteresting to it,
    /// it is the thing the caller must not see. Dropping the row at the
    /// source rather than filtering later means no reader can reach it.
    keep_history: bool,
}

impl Screen {
    fn new(cols: u16, rows: u16, keep_history: bool) -> Self {
        let cols = (cols as usize).clamp(2, MAX_COLS);
        let rows = (rows as usize).clamp(2, MAX_ROWS);
        Screen {
            cols,
            rows,
            grid: (0..rows).map(|_| blank_row(cols)).collect(),
            history: VecDeque::new(),
            row: 0,
            col: 0,
            pending_wrap: false,
            scroll_top: 0,
            scroll_bot: rows - 1,
            saved_cursor: None,
            parked: None,
            keep_history,
        }
    }

    fn push_history(&mut self, line: String) {
        if !self.keep_history {
            return;
        }
        self.history.push_back(line);
        while self.history.len() > MAX_HISTORY_ROWS {
            // O(1). A `Vec` front-drain costs O(cap) *per scroll* once the cap
            // is reached, and a pane spamming bare newlines scrolls once per
            // byte of the ring — 256 K scrolls x 4096 rows shifted is work an
            // adversarial (or merely chatty) pane should never be able to buy
            // with a single `get_output` call (#530 review, finding 1).
            self.history.pop_front();
        }
    }

    /// Scroll the active region up `n` rows. Rows leaving the *top of the
    /// screen* become scrollback; rows leaving an app-defined region (a status
    /// bar pinned above/below) are the app's own churn and are discarded, which
    /// is what the human sees too.
    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n.min(self.rows) {
            let gone = self.grid.remove(self.scroll_top);
            if self.scroll_top == 0 && self.parked.is_none() {
                let text = row_text(&gone);
                self.push_history(text);
            }
            self.grid.insert(self.scroll_bot, blank_row(self.cols));
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n.min(self.rows) {
            self.grid.remove(self.scroll_bot);
            self.grid.insert(self.scroll_top, blank_row(self.cols));
        }
    }

    /// LF / index: down one row, scrolling at the bottom margin.
    fn index(&mut self) {
        self.pending_wrap = false;
        if self.row == self.scroll_bot {
            self.scroll_up(1);
        } else if self.row + 1 < self.rows {
            self.row += 1;
        }
    }

    /// RI / reverse index: up one row, scrolling at the top margin.
    fn reverse_index(&mut self) {
        self.pending_wrap = false;
        if self.row == self.scroll_top {
            self.scroll_down(1);
        } else if self.row > 0 {
            self.row -= 1;
        }
    }

    fn carriage_return(&mut self) {
        self.pending_wrap = false;
        self.col = 0;
    }

    fn print(&mut self, ch: char) {
        if self.pending_wrap {
            self.carriage_return();
            self.index();
        }
        if self.col < self.cols {
            self.grid[self.row][self.col] = ch;
        }
        if self.col + 1 >= self.cols {
            self.pending_wrap = true;
        } else {
            self.col += 1;
        }
    }

    fn move_to(&mut self, row: usize, col: usize) {
        self.pending_wrap = false;
        self.row = row.min(self.rows - 1);
        self.col = col.min(self.cols - 1);
    }

    fn erase_in_line(&mut self, mode: u32) {
        let (from, to) = match mode {
            1 => (0, self.col.min(self.cols - 1)),
            2 => (0, self.cols - 1),
            _ => (self.col.min(self.cols - 1), self.cols - 1),
        };
        for c in from..=to {
            self.grid[self.row][c] = ' ';
        }
    }

    fn erase_in_display(&mut self, mode: u32) {
        match mode {
            1 => {
                for r in 0..self.row {
                    self.grid[r] = blank_row(self.cols);
                }
                self.erase_in_line(1);
            }
            2 | 3 => {
                for r in 0..self.rows {
                    self.grid[r] = blank_row(self.cols);
                }
            }
            _ => {
                self.erase_in_line(0);
                for r in (self.row + 1)..self.rows {
                    self.grid[r] = blank_row(self.cols);
                }
            }
        }
    }

    fn erase_chars(&mut self, n: usize) {
        let start = self.col.min(self.cols - 1);
        for c in start..(start + n).min(self.cols) {
            self.grid[self.row][c] = ' ';
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let start = self.col.min(self.cols - 1);
        for _ in 0..n.min(self.cols) {
            self.grid[self.row].remove(start);
            self.grid[self.row].push(' ');
        }
    }

    fn insert_chars(&mut self, n: usize) {
        let start = self.col.min(self.cols - 1);
        for _ in 0..n.min(self.cols) {
            self.grid[self.row].insert(start, ' ');
            self.grid[self.row].truncate(self.cols);
        }
    }

    fn insert_lines(&mut self, n: usize) {
        if self.row < self.scroll_top || self.row > self.scroll_bot {
            return;
        }
        for _ in 0..n.min(self.rows) {
            self.grid.remove(self.scroll_bot);
            self.grid.insert(self.row, blank_row(self.cols));
        }
    }

    fn delete_lines(&mut self, n: usize) {
        if self.row < self.scroll_top || self.row > self.scroll_bot {
            return;
        }
        for _ in 0..n.min(self.rows) {
            self.grid.remove(self.row);
            self.grid.insert(self.scroll_bot, blank_row(self.cols));
        }
    }

    /// Enter/leave the alternate screen. The primary grid is parked rather
    /// than cleared so a full-screen pager (`less`, a TUI menu) that exits
    /// leaves the pane reading as the human sees it: back to the shell
    /// output that was there before, not a blank screen.
    fn set_alt_screen(&mut self, on: bool) {
        if on {
            if self.parked.is_none() {
                let primary = std::mem::replace(
                    &mut self.grid,
                    (0..self.rows).map(|_| blank_row(self.cols)).collect(),
                );
                self.parked = Some(primary);
            }
        } else if let Some(primary) = self.parked.take() {
            self.grid = primary;
        }
        self.move_to(0, 0);
    }

    fn into_text(mut self) -> String {
        // A pager still on its alternate screen: the primary content is what
        // scrolled, and the alt screen is what is live — show both, in order.
        let mut rows: Vec<String> = self.history.into();
        if let Some(primary) = self.parked.take() {
            rows.extend(primary.iter().map(|r| row_text(r)));
        }
        rows.extend(self.grid.iter().map(|r| row_text(r)));
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        rows.join("\n")
    }

    /// The rows a human is looking at RIGHT NOW, and nothing else (#534).
    ///
    /// Three things are deliberately excluded, and each exclusion is the
    /// point rather than an optimization:
    ///
    /// - `history` — rows that scrolled off the top. Never populated at all
    ///   under `keep_history: false`, so this is belt and braces.
    /// - `parked` — the primary screen sitting behind an active alternate
    ///   screen. [`Screen::into_text`] shows it because a `get_output` reader
    ///   wants the context a pager is covering; it is by definition NOT
    ///   displayed, so a "is this still on screen" reading must not see it.
    /// - trailing blank rows, right-trimmed cells — cosmetic only, and
    ///   identical to `into_text`'s treatment so the two agree on content.
    fn into_visible(self) -> String {
        let mut rows: Vec<String> = self.grid.iter().map(|r| row_text(r)).collect();
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        rows.join("\n")
    }
}

/// One CSI sequence's numeric parameters, `;`-separated, defaults applied by
/// the caller. `0` and an empty slot both mean "absent" in ECMA-48's defaults.
fn param(params: &[u32], idx: usize, default: u32) -> u32 {
    match params.get(idx) {
        Some(0) | None => default,
        Some(v) => *v,
    }
}

/// Replay a raw pty byte stream onto a screen of `cols` x `rows` and return
/// the composed text: rows that scrolled off the top, then the rows currently
/// on screen, each right-trimmed, trailing blank rows dropped.
///
/// This is the whole of #520's fix: an overwrite in the stream becomes an
/// overwrite here, so redraw churn never reaches the caller in the first place.
///
/// **Not a "what is displayed" reading.** The history half makes this the
/// wrong input for any decision phrased as "is X still on screen" — see
/// [`render_visible`], which exists precisely because pointing a detector at
/// this output would reproduce the bug it was meant to fix (#534).
pub fn render_screen(bytes: &[u8], cols: u16, rows: u16) -> String {
    replay(bytes, cols, rows, true).into_text()
}

/// Replay `bytes` and return ONLY the rows currently on screen (#534) —
/// scrolled-off history is dropped as it scrolls, never retained and filtered.
///
/// ## Why this is a separate function and not a flag on the caller's side
///
/// [`render_screen`] returns *history rows followed by on-screen rows, joined
/// into one string*. Pointing `orchestration::prompt_wait_detected` at that output
/// would reproduce the exact bug #534 exists to fix: an answered question that
/// scrolled off is still in the history half, so the detector keeps matching
/// it — byte-ring behaviour with extra steps. The whole structural claim of
/// #534 is *"a question that is no longer RENDERED is answered"*, and only a
/// rendered-rows-only read can make it. Callers must not have to remember to
/// slice the history off; there is nothing here to slice.
///
/// ## What this can and cannot prove
///
/// It can prove a **negative about the composition**: replay these bytes at
/// this geometry and the text is not among the cells. It cannot prove a
/// negative about the human's screen, because the replay begins mid-stream
/// against a blank grid (see *Deliberate limits* at the top of this module) —
/// content painted before `bytes` begins and never repainted since is absent
/// here and present there. A caller turning "absent" into an action owes that
/// gap an argument; `orchestration::question_shown` is the one that does.
pub fn render_visible(bytes: &[u8], cols: u16, rows: u16) -> String {
    replay(bytes, cols, rows, false).into_visible()
}

/// The shared VT replay. `keep_history` decides only whether rows leaving the
/// top of the screen are retained — the composition itself is identical, so
/// the two public readings can never disagree about what is ON the grid.
fn replay(bytes: &[u8], cols: u16, rows: u16, keep_history: bool) -> Screen {
    let mut s = Screen::new(cols, rows, keep_history);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            0x1b => {
                i += 1;
                match bytes.get(i) {
                    // CSI: parameters/intermediates until a final byte.
                    Some(b'[') => {
                        i += 1;
                        let start = i;
                        while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                            i += 1;
                        }
                        let body = &bytes[start..i.min(bytes.len())];
                        let final_byte = bytes.get(i).copied();
                        i += 1;
                        if let Some(f) = final_byte {
                            apply_csi(&mut s, body, f);
                        }
                    }
                    // OSC (title, hyperlink, colour query): to BEL or ST.
                    Some(b']') => {
                        i += 1;
                        while i < bytes.len() {
                            if bytes[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                                i += 2;
                                break;
                            }
                            i += 1;
                        }
                    }
                    // DCS/SOS/PM/APC: to ST. Claude Code's boot-time colour
                    // probes (#179) come back through here.
                    Some(b'P') | Some(b'X') | Some(b'^') | Some(b'_') => {
                        i += 1;
                        while i < bytes.len() {
                            if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                                i += 2;
                                break;
                            }
                            if bytes[i] == 0x07 {
                                i += 1;
                                break;
                            }
                            i += 1;
                        }
                    }
                    Some(b'D') => {
                        s.index();
                        i += 1;
                    }
                    Some(b'E') => {
                        s.carriage_return();
                        s.index();
                        i += 1;
                    }
                    Some(b'M') => {
                        s.reverse_index();
                        i += 1;
                    }
                    Some(b'7') => {
                        s.saved_cursor = Some((s.row, s.col));
                        i += 1;
                    }
                    Some(b'8') => {
                        if let Some((r, c)) = s.saved_cursor {
                            s.move_to(r, c);
                        }
                        i += 1;
                    }
                    // Charset designators (`ESC ( B`) and friends: introducer
                    // plus one byte.
                    Some(b'(') | Some(b')') | Some(b'*') | Some(b'+') | Some(b'%') => i += 2,
                    Some(_) => i += 1,
                    None => {}
                }
            }
            b'\n' | 0x0b | 0x0c => {
                s.index();
                i += 1;
            }
            b'\r' => {
                s.carriage_return();
                i += 1;
            }
            b'\t' => {
                let next = ((s.col / 8) + 1) * 8;
                s.move_to(s.row, next.min(s.cols - 1));
                i += 1;
            }
            0x08 => {
                s.pending_wrap = false;
                s.col = s.col.saturating_sub(1);
                i += 1;
            }
            // Remaining C0 (BEL, SO/SI, ...) paint nothing.
            0x00..=0x1f | 0x7f => i += 1,
            _ => {
                let len = match b {
                    0x00..=0x7f => 1,
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => 1,
                };
                let end = (i + len).min(bytes.len());
                match std::str::from_utf8(&bytes[i..end]) {
                    Ok(text) => {
                        for ch in text.chars() {
                            s.print(ch);
                        }
                        i = end;
                    }
                    // Malformed or truncated: drop exactly ONE byte and
                    // resync, never the whole width the lead byte claimed.
                    // `0xe0` claims three bytes, so skipping the window eats
                    // the two valid characters behind a single bad byte — and
                    // since the ring is a byte TAIL that routinely begins
                    // mid-codepoint, that is the normal case at the head of a
                    // capture, not an exotic one (#530 review, finding 2).
                    Err(_) => i += 1,
                }
            }
        }
    }
    s
}

fn apply_csi(s: &mut Screen, body: &[u8], final_byte: u8) {
    // `?`/`>`/`<`/`=` mark private modes; `!`/`$`/`"`/`'`/space are
    // intermediates. Both are stripped before parsing numbers.
    let private = body.first().copied();
    let is_private = matches!(private, Some(b'?') | Some(b'>') | Some(b'<') | Some(b'='));
    let digits: Vec<u8> = body
        .iter()
        .copied()
        .filter(|c| c.is_ascii_digit() || *c == b';')
        .collect();
    let params: Vec<u32> = String::from_utf8_lossy(&digits)
        .split(';')
        .map(|p| p.parse::<u32>().unwrap_or(0))
        .collect();

    if is_private {
        // Private mode set/reset. Only the alternate-screen switches change
        // where text lands; cursor visibility, bracketed paste, mouse
        // reporting and focus events (#98) do not.
        if final_byte == b'h' || final_byte == b'l' {
            let on = final_byte == b'h';
            if params.iter().any(|p| matches!(*p, 47 | 1047 | 1049)) {
                s.set_alt_screen(on);
            }
        }
        return;
    }

    let n = |idx: usize| param(&params, idx, 1) as usize;
    match final_byte {
        b'A' => {
            let r = s.row.saturating_sub(n(0));
            s.move_to(r, s.col);
        }
        b'B' => {
            let r = s.row + n(0);
            s.move_to(r, s.col);
        }
        b'C' => {
            let c = s.col + n(0);
            s.move_to(s.row, c);
        }
        b'D' => {
            let c = s.col.saturating_sub(n(0));
            s.move_to(s.row, c);
        }
        b'E' => {
            let r = s.row + n(0);
            s.move_to(r, 0);
        }
        b'F' => {
            let r = s.row.saturating_sub(n(0));
            s.move_to(r, 0);
        }
        // CHA and its HPA alias (`` ` ``, written as its byte so the backtick
        // isn't load-bearing punctuation in the source).
        b'G' | b'\x60' => s.move_to(s.row, n(0).saturating_sub(1)),
        b'd' => s.move_to(n(0).saturating_sub(1), s.col),
        b'H' | b'f' => s.move_to(n(0).saturating_sub(1), n(1).saturating_sub(1)),
        b'J' => s.erase_in_display(param(&params, 0, 0)),
        b'K' => s.erase_in_line(param(&params, 0, 0)),
        b'L' => s.insert_lines(n(0)),
        b'M' => s.delete_lines(n(0)),
        b'P' => s.delete_chars(n(0)),
        b'@' => s.insert_chars(n(0)),
        b'X' => s.erase_chars(n(0)),
        b'S' => s.scroll_up(n(0)),
        b'T' => s.scroll_down(n(0)),
        b'r' => {
            let top = param(&params, 0, 1) as usize;
            let bot = match params.get(1) {
                Some(0) | None => s.rows,
                Some(v) => *v as usize,
            };
            let top = top.saturating_sub(1).min(s.rows - 1);
            let bot = bot.saturating_sub(1).min(s.rows - 1);
            if top < bot {
                s.scroll_top = top;
                s.scroll_bot = bot;
            }
            s.move_to(0, 0);
        }
        b's' => s.saved_cursor = Some((s.row, s.col)),
        b'u' => {
            if let Some((r, c)) = s.saved_cursor {
                s.move_to(r, c);
            }
        }
        // SGR (`m`), device queries (`c`, `n`), tab stops (`g`) and the rest
        // change appearance or ask questions; neither moves text.
        _ => {}
    }
}
