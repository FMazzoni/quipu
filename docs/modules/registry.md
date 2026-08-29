The index exists because a name in a store is unreachable until you already
know which file to open — which is the one thing a caller naming a project does
not know. It closes that circle and does nothing else.

## Why the name is not here

The name lives in the store it names, so it survives a move and cannot drift
from the data it names. The registry is only an INDEX, and it must stay
rebuildable from the stores it points at: nothing may live here that is not also
in a store. That is what keeps a corrupted or deleted registry a nuisance rather
than data loss — `qp project register` in each store rebuilds it.

## Invariants

A registry file carries `registry_version`. A registry without a version field
is a schema change pretending not to be one, so an unversioned file, and a file
from a future version, are both refused rather than guessed at — an older binary
reading a newer file on a best-effort basis would drop the fields it did not
understand and write the file back without them.

A name maps to at most one store. Two stores claiming one name is a hard, named
error at registration, naming both paths, because the alternative is a lookup
that silently reaches the wrong project's tickets. The one case that is not a
collision is the same store arriving again, matched by `project_uuid` rather
than by path, so a store that moved re-points its entry instead of stranding it.

## Gotchas

Entries are a list, not a map, so a duplicate name introduced by a hand edit is
representable — and therefore reportable by the doctor. A map would make the
corrupt state unspellable and the check vacuous.

`registry_version` is `#[serde(default)]` so a file that omits it deserialises
to 0 and reaches the named refusal, rather than dying as a generic error about a
missing field. The absence of the version *is* the diagnosis.

## Boundaries

Registration is an explicit act, never a side effect of a read: nothing in this
module is called from the hot path, and no command but `qp project` writes the
file. Deciding what a name is belongs to `project`, not here.
