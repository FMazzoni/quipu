The synopsis is fenced deliberately: outside a code block, rustdoc parses
`<url>` and friends as unknown HTML tags and silently deletes them.

```text
qp remote doctor [--json]
```

## Why this exists

Every setup failure this project has met presented as something other than its
cause. A repository that is present and complete is reported as missing. A key
that is perfectly good is reported as rejected. A host that is up is reported as
down. In each case the mechanism was two commands once you knew which two — the
cost was DISCOVERY, and it was paid in hours.

So this command's product is not a pass/fail. It is a NAME for the failure, and
a remedy that acts on that name. A diagnosis nobody can act on is a slower error
message.

## Two transports, because there are two remotes

The ledger is object storage and the claim authority is git; they fail in
completely different ways and neither implies the other. A store may have both,
either, or neither, and `doctor` says which it found and checks what is there.
Neither configured is not a fault — local-only is the default and a supported
state.

## Stop at the first failure

Within one remote the checks run in order and the sequence stops when one fails;
the rest are recorded as skipped. This is the rule that makes the output worth
reading. A missing credential makes every later check answer a question nobody
asked, and a screen of red herrings is the experience this command exists to
replace. The two remotes are independent: a broken ledger never suppresses the
authority's checks.

The ORDER inside the ledger sequence is itself a diagnosis. The clock is checked
before the credential verdict, because a clock a quarter of an hour out makes the
store refuse a key that was never wrong — and "the credentials were rejected" is
precisely the answer that sends someone to rotate it.

## Where a check cannot tell two causes apart, it names both

Some pairs are genuinely indistinguishable from one request, and picking one
silently is how an afternoon gets spent on the wrong fix:

- **A missing bucket versus a key not scoped to it.** Both arrive on the same
  call and want opposite fixes — create the bucket, or change the policy. When
  the url carries a prefix, a second LIST at the bucket ROOT separates them: a
  root that lists proves the bucket exists, so the fault is the policy; a root
  that answers `NoSuchBucket` proves the opposite. When the root is denied too —
  which is what a store does for a bucket you may not see, whether or not it is
  there — the report says so and names both.
- **Nothing listening.** The service being down, an address left behind when a
  host moved onto a private mesh network, and this machine being locked out of
  that network produce byte-identical refusals. All three are named.
- **A repository the authority will not serve.** Ownership (git's
  dubious-ownership check refuses a repository owned by a different user than the
  process serving it, and tells the client it does not exist — `chmod` does
  nothing, because the check is on the owner and not the mode), a server whose
  git is not on the PATH of a non-interactive session, and a path that is
  genuinely wrong all print the same sentence.

## The transport is part of the diagnosis

The same sentence from git means different things depending on how it got there,
so the classifier takes the URL and not just the text. `access denied` means
"your key is not authorised" over ssh and something else entirely over `git://`,
which **has no authentication at all** — no user, no key, no secret anywhere in
the request. Reading it as a credential problem there sends someone to inspect an
ssh key that took no part in the failure, which is the exact shape of
misdirection this command exists to end. On `git://` that message means the
repository is absent, is not exported (no `git-daemon-export-ok`, and the daemon
was not started with `--export-all`), or is present and full but owned by a
different user than the daemon process. A filesystem path has no daemon, no login
session and no credential, so its list is shorter still.

One more ambiguity, measured rather than assumed: a `git://` daemon that accepts
a connection and dies before answering prints `Could not read from remote
repository` — character for character what it prints when it refuses a
repository, on git 2.54.0 and 2.55.0 alike. Where git names its reason the report
gives the three repository causes; where it falls back to that catch-all, the
daemon itself is named as a further one, because picking one of two
indistinguishable readings is the thing this command must never do.

## git's wording is not a foundation to build on

The messages this classifier reads belong to git, and git may reword them in any
release. A diagnosis that evaporates when it does is worse than one that never
existed, because it disappears at the moment someone is already lost.

So the classifier names the EVIDENCE first — did the name resolve, was a
connection made, did the far end answer, did git name a reason — and treats
verbatim strings as a refinement on top of that rather than the foundation. A new
spelling costs one pattern and changes nothing downstream. An unfamiliar spelling
degrades to a diagnosis that still names the causes the TRANSPORT admits, since
those do not depend on the wording, plus a plain statement that quipu did not
recognise the text and that git's own words are the authority. That last part is
load-bearing: the catch-all is not permitted to be a shrug.

Every pattern was produced and read, not recalled, and the tests record which git
each string came from. That matters more than it looks. A failure that reproduced
only in CI was first attributed to a version difference between git 2.54.0 and
2.55.0; measuring the two side by side refuted it — they emit character-identical
text for every connection-drop shape, and git's `protocol.c` and its
transport-policy enum are byte-identical between those releases. The real
wording-dependent gaps were elsewhere, and two were live: git's second spelling
for a dropped connection (`the remote end hung up upon initial contact`), and a
far end answering something that is not the git protocol at all (`protocol error:
bad line length character: …`), which had no name here whatsoever.

## Not everything that fails is the remote

Two causes name this machine rather than the far end, and both are invisible to a
classifier that thinks only in networks and repositories:

- **Local protocol policy.** `protocol.<name>.allow` and `GIT_ALLOW_PROTOCOL` can
  forbid a transport outright; git then refuses before contacting anything. The
  host may be perfectly healthy, so every network-shaped remedy is a wild goose
  chase.
- **Something else on the port.** A port that answers with anything that is not a
  pkt-line is neither unreachable nor unauthorised — it is a different service, a
  proxy answering first, or a TLS endpoint addressed as plaintext.

## The authority check that matters is the write

`git ls-remote` succeeds against a repository nobody may write. A claim is a ref
CREATION, so a store that passes a read check can still fail every claim it will
ever make. `doctor` creates a ref under `refs/quipu/doctor/`, deletes it, and
reports the leftover by name if the delete does not take — leaving something
behind on someone else's host is a fault, not a detail.

## The object-store probe writes, and does not delete

The write probe PUTs a fixed, flat key and reads it back. Fixed because quipu's
object client has no DELETE at all — nothing in the ledger is ever deleted, so
requiring a delete permission would fail stores whose key is correctly scoped
without one. A fixed key is overwritten by the next run instead of accumulating.
Flat — no `/` — because a key containing one would appear as a *project* to the
listing that reads `CommonPrefixes`.

PUT and GET are separate checks because a policy can grant one and not the other,
and sync needs both: it writes its own batches and reads everyone else's.

## Output and exit code

Human-readable by default, `--json` alongside. Every check carries a stable
`name` so a setup script can branch on one, and every failure carries a `remedy`.
The diagnosis goes to stdout in the requested format whatever it finds — the
command did what it was asked — and the exit code is the signal: `0` when nothing
failed, `5` when something did. A code of its own rather than the retryable
conflict code, so a skill that retries on that one does not loop forever against
a remote that will never come good.
