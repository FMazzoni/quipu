Two concerns live here: the storage of the two remote urls, and the environment
quipu hands to every git child it spawns. They share a module because the second
is meaningless without the first.

## Storage: `meta`, not a config file

The urls are rows in the `meta` table:

```text
meta(key='remote.authority', value=<url>)
meta(key='remote.ledger',    value=<url>)
```

`meta` already holds `project_uuid`, `schema_version` and `display_prefix`, so
this adds a convention rather than a mechanism. A TOML file was rejected for the
reasons the open `qp config` decision records: one source of truth, no toml
dependency, and no config-file precedence tiers to reason about.

**Reconciliation with the pending `qp config` command.** `qp remote` is a typed
façade over two `meta` keys, not a second config system. When `qp config` lands
it reads and lists these keys like any other; what it must *not* do is offer a
generic `qp config set remote.authority <url>`, because the typed command is
where url validation and the authority/ledger distinction are explained. A
single well-named verb for a setting with real semantics, and the generic
lister for everything, is the same split `display_prefix` already has between
`qp init --prefix` and a read-only `config` view.

**No event is recorded.** Attaching a store is a property *of* the store, like
its prefix, not a transition in any task's history — and the event log is
defined as the content of the store, which the replayer rebuilds. Adding a
store-scoped `remote_set` event would put a row in the ledger that rebuilds
nothing. The audit trail for an attachment is the attachment.

## ssh multiplexing, and the burst hazard

Measured, on real hardware: a claim against a hosted remote costs **2424 ms**
without ssh multiplexing and **16 ms** with it; against a LAN host, 552 ms
versus 21 ms. That is a 150x difference, and it is far too large to leave to
whether a user happened to edit `~/.ssh/config`. So quipu sets
`GIT_SSH_COMMAND` on the git children it spawns. Those are the claim authority's
children and nothing else: the ledger is object storage over HTTPS and spawns no
process at all.

```text
ssh -o ControlMaster=auto -o ControlPath=<store-dir>/ssh/m-<url>-<lane> -o ControlPersist=60
```

Per-invocation, on the child process only. Nothing global changes and the
user's ssh config is untouched. The socket lives under quipu's own directory
rather than `~/.ssh`, so a stale socket is quipu's to leave and quipu's to
clean up.

### The hazard, stated plainly

**Multiplexing helps sequential claims and breaks bursts.** Measured: ten
concurrent pushes through *one* ControlMaster connection all failed — sshd's
`MaxSessions` (default 10) counts the master alongside its channels. The same
ten agents on *independent* connections worked correctly, with exactly one
winner. A wave that fans out through a shared master fails in a way that looks
like anything but ssh.

### The resolution: the socket is keyed by lane

A `ControlPath` shared by every quipu process is what creates the burst; a
`ControlPath` unique per process would fix the burst and destroy the benefit,
since each `qp claim` is its own short-lived process and would never find a
master to reuse. Neither extreme works.

So the socket is keyed by **lane** — the acting agent's id. One agent claiming
ticket after ticket reuses its own master and gets the 16 ms path. Ten agents
fanning out in a wave have ten distinct ids, so they get ten independent
connections: exactly the configuration that was measured to work. The key is
the same thing quipu already uses to say who is acting, which is why it lands
in the right place without a new concept.

The url is hashed into the socket name too, so two remotes never share a
master, and both hashes are truncated to eight hex characters to keep the path
clear of the 108-byte `sun_path` limit. If the path would still be too long,
multiplexing is dropped rather than handed to ssh to truncate.

### Overriding it

- A `GIT_SSH_COMMAND` (or `GIT_SSH`) already in the environment wins outright —
  quipu never overwrites one.
- `QUIPU_SSH_MULTIPLEX=0` (or `off`/`false`/`no`) disables the injection.
- Non-ssh urls — `https://`, `git://`, a filesystem path — get no injection,
  because no ssh process is spawned for them.

The knob is an environment variable rather than a `meta` key on purpose: it is a
per-machine, per-run concurrency property, not a per-project preference, and the
`qp config` decision's filter excludes concurrency knobs from project config for
exactly this reason.

## Reachability

`probe` is the only function here that touches the network, and only `--check`
calls it. `git ls-remote` exits 0 against a reachable remote — including a
freshly created, entirely empty bare repository, verified — and 128 when it
cannot get there. Children also run with `GIT_TERMINAL_PROMPT=0`, so a missing
credential fails instead of hanging on a prompt no agent will ever answer.
