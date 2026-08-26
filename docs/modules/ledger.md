One shared ledger can hold many projects, and a store only ever syncs its own
one in. The full set therefore lives in exactly one place — the ledger — and
this command is the read over it.

## It must not write to the store

An import writes a projection into the store you are standing in. This deliberately
does not: each project's batches are replayed into a THROWAWAY sqlite file under
the system temp directory, read back, and deleted before the process exits. No
schema change, no rows, no `meta` key. Answering a question about other projects
must never leave a trace of them in a store that is not part of them — a store's
board is the set of tickets it is answerable for, and a query that quietly widened
it would be a bug nobody notices until they act on a ticket that was never theirs.

The scratch directory is under the system temp directory rather than beside the
database on purpose: a crash mid-replay must not leave something inside `.quipu/`
that the next reader mistakes for state.

## Why a command and not a flag

`qp list --global` would put the widest possible answer one character away from
the narrowest, printed in the same shape, and the difference between "my board"
and "every project on the ledger" is precisely what must never be misread. A
separate verb makes the provenance part of the call site, and the output says so
twice more: the human header names the ledger url and states that nothing local
was written, and the JSON carries `"source": "ledger"` next to the url, so a
consumer that flattens the projects still cannot lose the one fact that matters.

The reverse also holds — this is not a replacement for `qp list`. It shows the
ledger as of this fetch, so anything not yet synced is absent, and the header
says that too.

## Colliding display ids

Every project numbers from 1 and `display_prefix` is the same everywhere in
practice, so several projects each hold a `QP-1`. Three renderings were possible:

- **Prefix-qualify.** Disambiguates nothing while the prefixes are identical,
  which is the case that actually occurs.
- **Canonical origin identity** (`<store uuid>/<rowid>`). Unambiguous and
  unreadable, and it is not the id anyone's board shows.
- **Group by project.** Chosen. The project key heads each group, so a row's
  project is never in doubt, and inside a group `QP-7` means what it means on
  that project's own board.

The id shown is derived from `task_origin` — the store that minted the task and
the rowid it holds THERE — not from the throwaway projection's own `display_id`,
which is a number this command minted seconds ago and nobody has ever seen. Where
one project holds two stores that independently minted the same rowid, the id is
suffixed with the head of the origin store's uuid; the full origin pair is always
in the JSON.

## Reuse, not a second implementation

Fetching is `sync::materialise` (ensure_repo → fetch → integrate), so there is one
refspec, `refs/quipu/claims/*` stays out of reach, and this path cannot push.
Grouping is `project::batches_by_project`, which decides a batch's project through
`project::effective_key` — per file, never per directory, because a store stamped
with an identity after the fact has keyless batches belonging to a different
project sitting in the same directory. Replay is `import::import` with `--merge`,
which is the same merge order and the same legality guard a real sync runs.

Anything else would be a second answer to "what does this log mean", and the copy
that drifts is the one whose convergence nobody is testing.

The throwaway store is stamped with the project key of the group being replayed,
through the ordinary "join a project" path. A merge scopes itself to the TARGET
store's own key — that is what stops one ledger's projects from absorbing each
other — so a scratch store that minted a key of its own would match none of the
batches staged for it: the import would succeed, read nothing, write nothing, and
the project would render as an empty board. That is indistinguishable from a
ledger nobody has written to, which is why the count is checked rather than
trusted. Every staged batch belongs to the group by construction, so if the merge
skips one as another project's, the command fails loudly instead of reporting an
empty project. The scope is stated, never inferred from whichever database the
replay happens to land in.

`--force` is passed to that import. The anomaly guard exists to protect live data
from an unjustifiable history; the target here is a temp file deleted moments
later, and refusing would let one project's damaged history blank out every other
project in the view. Every anomaly is still reported on stderr.

## Cost

Measured: a store's whole event log exports to a small fraction of a megabyte, so
fetching everything and replaying it per query is a rounding error. There is
deliberately no partial fetch, no sparse checkout, no index and no cache — each
would be a second source of truth about the ledger's contents, bought with
nothing.
