# `base-green` payload fixtures (#1174, #1181)

Real-shaped `GET /repos/{o}/{r}/commits/{ref}/check-runs` and `/status` payloads,
trimmed to the fields the reductions read (`total_count`, `check_runs[].status`,
`check_runs[].conclusion`; `statuses`, `state`).

They exist because the reductions in `workflow.rs` — `BASE_CHECK_RUNS_JQ` and
`BASE_STATUS_JQ` — ARE the `base-green` decision, and nothing used to execute
them: both the queue's `Fake` runner and the shim harness's fake `gh` returned
the already-reduced word, so every test agreed about a string neither had run.
That is how a suite green on three platforms coexisted with a truncated page
reading `green` (#1181 rev-lead, blocking).

`checkruns-truncated.json` is the regression: `total_count` exceeds the runs on
the page, and the failure is one of the omitted ones. Under the pre-#1181
expression it reduced to `green`.

## The shape fixtures (#1181 rev-lead NB5)

`checkruns-no-total-count.json` and the `status-no-*.json` pair drop a field the
reductions' own clauses rest on. Both endpoints document these as always
present; the point is that the reduction must not *assume* it, because jq fails
open on the absence rather than erroring:

- `null > N` is **false** (jq sorts null below every number), so a missing
  `total_count` skipped the truncation clause entirely and fell through to
  `green` — round one's defect in different clothing;
- `null | length` is **0**, not an error, so a missing `statuses` read as the
  definite claim "no legacy statuses exist";
- a missing `state` fell to the `else` and reported `red` — which refuses, but
  says something false about the base while doing it.

`checkruns-no-total-count-with-failure.json` is the negative control: a payload
can be shape-broken AND carry a visible failure, and `red` still wins, because a
failure loomux can actually see is the more actionable answer.
