# Golden templates — the four role instruction files as they stood before #222

These are byte copies of `src/orchestration/templates/{orchestrator,worker,reviewer,planner}.md`
taken from the commit *before* the advanced-orchestrator toggle
(`4b93282`, the #222 integration branch with the block model and the workflow pane
on it, and nothing else).

`the_toggle_off_leaves_every_instruction_file_byte_for_byte_what_it_was` renders
**these** with the six pre-#222 template variables and asserts that a group launched
with the advanced orchestrator **off** gets exactly that text. They are the
*independent* side of that comparison, and that is their whole point: the first
version of the test built its expected value out of the live template, so both sides
moved together and unconditional prose added to a template sailed through the very
pin advertised to stop it (rev-11 F1).

## If this test fails

It is telling you that **the text every agent in every default group reads has
changed**. That is not automatically wrong — but it is never incidental, so it needs
a human, not a re-run.

- If you *meant* to edit the role templates, re-bless the fixture: copy the changed
  template over the file here, in its own commit, and say in the message what
  changed for the agents. The diff on this directory is then the review surface for
  "what did we just tell every worker to do differently?".
- If you did **not** mean to change what a default group reads — you were adding
  workflow-conditional prose — then the prose is in the wrong place. It belongs in
  `templates/workflow.md` or `templates/block.md`, behind `{{WORKFLOW}}` /
  `{{BLOCK_NOTE}}`, which resolve to the empty string for the built-in roster.

Line endings are normalized before comparison (there is no `.gitattributes`, so these
are CRLF on Windows and LF elsewhere) — the assertion is about the words, not about
the checkout.
