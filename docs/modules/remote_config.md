Where the ledger url lives, and what a url has to look like before it is written.

## Storage: `meta`, not a config file

The url is a row in the `meta` table:

```text
meta(key='remote.ledger', value=<url>)
```

`meta` already holds `project_uuid`, `schema_version` and `display_prefix`, so
this adds a convention rather than a mechanism. A TOML file was rejected for the
reasons the open `qp config` decision records: one source of truth, no toml
dependency, and no config-file precedence tiers to reason about.

**Reconciliation with the pending `qp config` command.** `qp remote` is a typed
façade over a `meta` key, not a second config system. When `qp config` lands it
reads and lists that key like any other; what it must *not* do is offer a
generic `qp config set remote.ledger <url>`, because the typed command is where
url validation lives and where a url's *kind* is explained. A single well-named
verb for a setting with real semantics, and the generic lister for everything,
is the same split `display_prefix` already has between `qp init --prefix` and a
read-only `config` view.

**No event is recorded.** Attaching a store is a property *of* the store, like
its prefix, not a transition in any task's history — and the event log is
defined as the content of the store, which the replayer rebuilds. Adding a
store-scoped `remote_set` event would put a row in the ledger that rebuilds
nothing. The audit trail for an attachment is the attachment.

## Validation is two layers, and the split is deliberate

`validate_url` is the shallow half: it rejects what would misbehave whatever the
url turns out to be — empty, padded with whitespace, carrying a control
character, or starting with `-` and so read as a flag by whatever is eventually
handed it. It is scheme-blind on purpose.

`validate_ledger_url` is the shape half: shallow check first, then
`s3::parse`. A kind of url that is not the ledger's adds its own second layer
beside that one rather than loosening this one — the refusal has to be able to
name the form the caller should have typed, and a single merged check cannot.

## Reachability

`probe` is the only function here that touches the network, and only `--check`
calls it. It LISTs the ledger's own prefix: the cheapest question that
exercises credentials, trust, the bucket and the policy in one round trip, and
one whose failure the object store *names*, so `Reach::Fault` can carry the
diagnosis instead of flattening everything into "could not reach it".

## What used to be here

A second url — the git claim authority — and the `GIT_SSH_COMMAND` quipu
injected into the git children a claim spawned, keyed by acting agent so a wave
fanning out got one ssh master each instead of bursting a single one past
sshd's `MaxSessions`. Both are gone: claims are arbitrated by one writer holding
`BEGIN IMMEDIATE`, so nothing here spawns a process any more. The measurements
and the reasoning are in the vault, not in the tree.
