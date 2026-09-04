The collateral-of-a-fix body: `body-good.md` with every figure left at the value it had one
round ago, which is how 7 of the 23 fail rounds on the #2168 corpus were made. Nothing here
is invented — each defect reproduces a row of Part 1 at the value that row names.

Part of #900.

<!-- agent-layer -->
<details>
<summary>Agent context — evidence, receipts, instruments</summary>

*Everything in this fold is measured at `7396a0bd`, base `517073c4`.*

### Diffstat, measured at both ends

`git diff 517073c4..7396a0bd --numstat`: 4 files changed, 2673 insertions(+), 27 deletions(-)
— reproduces #2139 r2, where the head moved twice and the body's total stayed at the
round-1 figure.

### Banked green

Run **33791843349** at `7396a0bd`, all three platforms green. Run 33999999999 covered the
docs job.

### Append proof

The head blob of `src-tauri/tests/reviewdrive.rs` is `61855f9c` **324,776** bytes —
reproduces #2140 r3, the wave-1 figure left standing against the head blob.

`.orrerix/lessons.md` is 3361 bytes — reproduces #1764 r5.

`CLAUDE.md`:782 carries the source-scanning-guard bullet — reproduces #1764 r5's stale cite.

Scratch round cut from `deadbee1`, and the round-2 fixes are measured at `deadbee1`.

</details>
