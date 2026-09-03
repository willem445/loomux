The review driver persists the `cap_starved_since_ms` anchor with the drive record, and
`note_cap_starvation` is the only writer. This body states every receipt correctly against
the facts in `facts.json`, and is the NEGATIVE CONTROL for the whole suite: an
implementation that refuses everything fails here rather than passing everywhere.

Part of #900.

<!-- agent-layer -->
<details>
<summary>Agent context — evidence, receipts, instruments</summary>

*Everything in this fold is measured at head `c7a3626a`, base `517073c4` (re-derived with
`git merge-base`, not inferred).*

### Diffstat, measured at both ends

`git diff 517073c4..c7a3626a --numstat`: 4 files changed, 773 insertions(+), 27 deletions(-)
— `crates/loomux-engine/src/reviewdrive.rs` 125, `doc/design/review-driver.md` 67,
`src-tauri/src/orchestration/rdtick.rs` 65, `src-tauri/tests/reviewdrive.rs` 516. Per-file
deltas sum to the total.

### Banked green

Run **33791843349** at `c7a3626a`, all three platforms green.

### Append proof

The base blob of `src-tauri/tests/reviewdrive.rs` is `728f7407`, **300,527** bytes, and its
head blob `61855f9c` is **325,375** bytes — a verbatim prefix, both by `git cat-file -s`.
(Both figures and the path sit on ONE line on purpose: that is what makes the blob-pairing
rule fail-able. Pairing each figure with the blob beside it settles both; without it the
base figure is measured against the file at head and 300,527 becomes a finding.)

`origin/main` moved during review, to `65ecd6ae` (#2124), which this branch does not carry.

</details>
