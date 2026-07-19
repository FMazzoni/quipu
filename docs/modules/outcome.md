Every mutation models its result once as a small `Outcome` struct
(`Serialize` for `--json`, `human()` for the prose line printed today),
then renders it through `emit`. This generalizes the pattern already used
by `qp add`'s `Created` struct.
