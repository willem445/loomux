### Red-before-green: the wave

The same body as `body-good.md`, with each figure left at the value it had one round ago —
the collateral-of-the-previous-fix class this checker exists for. Five independent stalings,
one per checkable figure: a head SHA the round was re-cut away from, a reddened set missing
the test a later fix added to it, a name that is no longer in the failing set at all, a
bullet count spelled as the wrong word, and a bullet split whose two halves both moved.

| # | behaviour set aside | scratch SHA | cut from | run | tests reddened | failure line (abridged) |
|---|---|---|---|---|---|---|
| 1 | session-id compare is canonicalized | `deadbeef` | `7b36ae86` | 33928049423 | `a_session_id_is_compared_canonically_and_a_non_uuid_is_a_mismatch` | `assertion failed: session_ids_match(minted, "550E8400-E29B-41D4-A716-446655440000")` |
| 13 | a turn is opened lazily and ANNOUNCED | `f9afc965` | `7b36ae86` | 33928069681 | `a_compact_boundary_carries_its_trigger_and_its_pre_token_count`, `a_stream_event_delta_is_text_and_a_non_text_delta_is_not`, `an_unknown_message_type_is_ignored_as_an_event_and_kept_as_evidence`, `an_unrecognized_terminal_reason_is_carried_not_collapsed`, `a_test_that_no_longer_exists_at_this_head` | `assertion 'left == right' failed` |

Every figure is read from that round's own log rather than adjusted by arithmetic.

- **Round 1 — one test** (593/1). The single behaviour the round sets aside.
- **Round 13 — five tests** (589/5). All five read `TurnStarted`.
