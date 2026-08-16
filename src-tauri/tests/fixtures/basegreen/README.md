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
