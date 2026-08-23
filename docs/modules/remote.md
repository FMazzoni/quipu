The synopsis below is fenced deliberately: outside a code block, rustdoc parses
`<url>` and friends as unknown HTML tags and silently deletes them, losing every
argument placeholder.

```text
qp remote set --authority <url> [--ledger <url>] [--check] [--json]
qp remote set --ledger <url>    [--check] [--json]
qp remote show   [--check] [--json]
qp remote clear  [--authority] [--ledger] [--all] [--json]
```

## Two urls, independently settable, either omittable

- **authority** — where claims are refereed. A claim is a git ref push that the
  far end either accepts or rejects; that rejection is the mutual exclusion.
  Reachability matters. Ticket content never crosses this url.
- **ledger** — where the exported event log syncs. Content crosses it, so only
  machines you own need to reach it. Nothing about it referees anything.

They are separate because the two jobs are separate. Authority-only is a
referee over content that stays local. Ledger-only is backup and sync with no
referee. Both are coherent, both are supported, and neither requires the other:
`qp remote set --ledger <url>` on a store with no authority is a complete,
correct configuration.

## Local-only is the default and a first-class state

With neither url set, quipu does exactly what it always did — no network, no
daemon, no subprocess, no external service. `qp remote show` on a fresh store
prints `local-only — no authority, no ledger` and reads one row out of `meta`.

`qp remote clear` is load-bearing rather than a convenience. A configured but
unreachable authority makes `qp claim` fail, by design, and `clear` is the
deliberate, recorded act that gets you unblocked. That is why bare
`qp remote clear` means "clear everything": when it is the thing you reach for,
it is because something is already wrong, and the shortest form should be the
one that works.

## Nothing here touches the network unless you ask

`set` writes config and stops. A remote that is down right now is still a
remote you are allowed to configure, so a failed probe never fails the write.
`--check` adds an opt-in `git ls-remote` and reports the result alongside the
config; the exit code reflects the write, not the probe.

`show` is likewise local unless `--check` is passed, which keeps the governing
rule intact: exactly one command talks to the network synchronously, and it is
`claim`.

## Where the config lives

Both urls are rows in the `meta` table — the same place as `project_uuid`,
`schema_version` and `display_prefix`. See `remote_config` for the keys and for
why `qp remote` is a typed façade over them rather than a parallel mechanism.
