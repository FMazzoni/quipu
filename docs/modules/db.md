Also holds the error types and the shared mutation utilities. Every state
mutation in the crate routes through `with_tx` + a guarded conditional
UPDATE — see `decisions/guarded-state-transitions.md` in the vault for the
contract.
