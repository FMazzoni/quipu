Every other command finds its store by walking up from the cwd. A process
holding several stores at once cannot: it is handed a NAME. This command is the
whole of that mapping — put a store in the index, take one out, list it, resolve
a name to a path.

```text
qp project register            index this store under the name it carries
qp project list                what is registered, dangling entries flagged
qp project lookup <NAME>       the store file that name resolves to
qp project unregister <NAME>   drop an entry (the remedy for a stale one)
```

## Invariants

Registration is explicit. Nothing here runs as a side effect of a read, and no
other command writes the registry: a tool that writes to a machine-wide file
while you are looking at a board is a surprise nobody can predict.

A lookup that does not resolve is an error, never a fallback to the store under
the cwd. Silently answering with *some* store is how a caller ends up reading a
real ticket from the wrong project, which looks exactly like success.

## Boundaries

The name itself is settled at `qp init` (see `project::assign_name`); this
command never invents one. The registry's format, versioning and collision rules
live in `registry`.
