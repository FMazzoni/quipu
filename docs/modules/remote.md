The synopsis below is fenced deliberately: outside a code block, rustdoc parses
`<url>` and friends as unknown HTML tags and silently deletes them, losing every
argument placeholder.

```text
qp remote set --ledger s3://<host>/<bucket>[/<prefix>] [--check] [--json]
qp remote set --front-door https://<host>[:<port>]      [--check] [--json]
qp remote show   [--check] [--json]
qp remote clear  [--ledger] [--front-door] [--all] [--json]
qp remote doctor [--json]
```

## Two urls, and they are unrelated to each other

**ledger** — where the exported event log syncs. An object-store url, and only
an object-store url: a git ledger fetched every project onto every machine and
no transport option turned that off (see `docs/modules/sync.md`). Content
crosses it, so only machines you own need to reach it. It referees nothing —
claims are arbitrated locally, by one writer holding `BEGIN IMMEDIATE`.

**front door** — the `qp serve` process this store is reached through. An
http(s) url, with plaintext accepted only to a loopback literal, which is
`client::check_url`'s rule and not a second opinion about it.

The two share nothing but the `meta` table. A store may have both, either, or
neither, and each of the four is a supported shape: a ledger with no front door
is the ordinary synced store, a front door with no ledger is a store other
machines reach but that syncs with nobody, and neither is local-only.

The day this held one url, `Remotes` was already a struct rather than a bare
`Option` and `clear` already took `--ledger` and `--all`, on the grounds that
attachment had been a set of urls before and would be again. It is again, and
adding the second was a field and a `meta` key rather than a signature change
anywhere.

### Why the flag is `--front-door` and not `--server`

The global `--server` claims that long name in every subcommand, and clap
refuses a duplicate outright — with a `debug_assert`, so a release build gets a
silent mis-resolution instead of an error, which is how this was found.

The collision is a mercy rather than an obstacle, because the two flags mean
different things and the shared name would have hidden it. Global `--server`
routes THIS invocation through a front door and is not remembered. `remote set
--front-door` records the front door the store is reached through and sends
nothing anywhere. Nothing reads the recorded url as a default for the global
flag today — `qp remote doctor` is its first and only reader — and making it one
is a change to every verb's dispatch, not to this command.

## Local-only is the default and a first-class state

With no url set, quipu does exactly what it always did — no network, no daemon,
no subprocess, no external service. `qp remote show` on a fresh store prints
`local-only — no ledger, no server` and reads two rows out of `meta`. It prints this store's
`project_key` and `project_uuid` above that either way: this is the command that
answers "who am I to the other machines", and two stores of one project differ
in the second and agree in the first.

`qp remote clear` is the deliberate, recorded act of detaching. Bare
`qp remote clear` means "clear everything", because when it is the thing you
reach for it is usually because something is already wrong, and the shortest
form should be the one that works.

## Nothing here touches the network unless you ask

`set` writes config and stops. A remote that is down right now is still a
remote you are allowed to configure, so a failed probe never fails the write.
`--check` adds an opt-in probe and reports the result alongside the config; the
exit code reflects the write, not the probe.

A front-door probe is one `GET /healthz`, which is the only question that
endpoint answers: it reports what the process opened at startup, and the
comparison against this store's own project name happens here rather than
there. Asking the server "do you serve X" would be asking a liveness probe to
answer a question about user input, which the process contract rules out.

A ledger probe reports the object store's own diagnosis rather than a flat
"unreachable", and that distinction is the value of the probe: absent
credentials, rejected credentials, a missing bucket, a prefix outside your
policy, a TLS trust failure and a skewed clock all want different fixes, and they
are indistinguishable once flattened.

`--check` answers "is it there". `qp remote doctor` answers "why is it not",
which is a different and much larger question — see `doctor` for the checks, the
order they run in, and the failures they exist to name. It is the only command
here that writes anything to a remote, and what it writes it overwrites rather
than accumulating.

`show` is likewise local unless `--check` is passed, which keeps the governing
rule intact: no command reaches the network unless it was asked to, and `sync`
is the only one whose whole job is to.

## Where the config lives

The url is a row in the `meta` table — the same place as `project_uuid`,
`schema_version` and `display_prefix`. See `remote_config` for the key and for
why `qp remote` is a typed façade over it rather than a parallel mechanism.
