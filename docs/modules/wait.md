The wave barrier. `--cohort-done` waits for the tag-matched cohort to
reach zero non-terminal tasks; an empty cohort exits 4 rather than
succeeding, because "nothing matched" and "everything finished" must
not look alike to an orchestrator.
