### Red-before-green: the wave

Local `cargo` is banned for agents, so there is no base-branch run to produce a red. Each
round is a throwaway scratch branch with its own `[scratch]` draft PR, one behaviour set
aside, everything else wired as it ships.

| # | behaviour set aside | scratch SHA | cut from | run | tests reddened | failure line (abridged) |
|---|---|---|---|---|---|---|
| 1 | session-id compare is canonicalized | `ce859688` | `7b36ae86` | 33928049423 | `a_session_id_is_compared_canonically_and_a_non_uuid_is_a_mismatch` | `assertion failed: session_ids_match(minted, "550E8400-E29B-41D4-A716-446655440000")` |
| 13 | a turn is opened lazily and ANNOUNCED | `f9afc965` | `7b36ae86` | 33928069681 | `a_compact_boundary_carries_its_trigger_and_its_pre_token_count`, `a_stream_event_delta_is_text_and_a_non_text_delta_is_not`, `an_unknown_message_type_is_ignored_as_an_event_and_kept_as_evidence`, `an_unrecognized_terminal_reason_is_carried_not_collapsed`, `init_booted_result_is_the_decoder_walking_one_turn`, `pump_publishes_events_and_logs_the_lines_it_could_not_decode` | `assertion 'left == right' failed` |

Every figure is read from that round's own log rather than adjusted by arithmetic.

- **Round 1 — one test** (593/1). The single behaviour the round sets aside.
- **Round 13 — six tests** (588/6). All six read `TurnStarted`.
