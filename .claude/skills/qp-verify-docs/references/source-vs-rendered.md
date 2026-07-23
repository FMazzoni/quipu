# Source or rendered page, and the tooling to read each

## Which one you read

Both get read, for different jobs. Confusing them produces either a false PASS
or a reverted skill, so the split is stated before anything else:

- **Mechanical tests read SOURCE, always.** `tests/docs.rs` checks `//!` header
  shape, summary length, the blank `//!` before every pointer, and `///` first
  paragraphs — by regex, over `src/**/*.rs`. Its own header explains why it must
  never read HTML: rustdoc injects `<wbr>` into long module names, so
  `install_skills` renders as `install_<wbr>skills` and a naive name regex over
  the HTML gives a false PASS on exactly the module most likely to drift. That
  warning is correct and stays. Do not "fix" `tests/docs.rs` to read rendered
  output.

- **Reading sweeps read the RENDERED PAGE.** A judgement about redundancy,
  ordering, or whether a copied command still runs is a judgement about the
  artifact a reader sees. No regex is involved, so the `<wbr>` hazard does not
  apply — an agent reading `install_skills` on the page reads it correctly.

These do not conflict. The rule is about method: pattern-matching needs source
because rendering perturbs the bytes; reading needs the page because
composition is where the defects live. Do not revert this skill to per-file
sweeping on the strength of the `tests/docs.rs` warning — it warns about
regexes, not about eyes.

**Why the page, concretely.** A module's rendered page is three sources merged:
its `//!` header, plus the `include_str!`'d `docs/modules/<name>.md`, plus an
item table built from the first paragraph of each `///`. The crate front page is
`main.rs`'s header plus all of `docs/architecture.md`. Per-file review sees one
third at a time, so every sentence can be true in isolation while the page is
redundant, misordered, or uncopyable. Two real misses prove it: a five-agent
per-file sweep passed a duplicated exit-code table (`main.rs`'s header restates
architecture.md's `## Exit codes` section, both on `qp/index.html`) and passed
rustdoc turning an unbackticked `--cohort-done` into `–cohort-done`, which does
not run when pasted.

## Building and dumping the docs

Build first:

```
just docs          # → target/doc/
just docs-dump     # every module page dumped as text, for skimming
```

**Dump pages; do not read raw HTML.** `w3m -dump -cols 100 <page>` is the tool.
It is installed and renders rustdoc cleanly at roughly half the bytes of the
HTML — the front page goes 44629 → 21131 — which is the difference between the
sweep fitting in context and not. `lynx`, `pandoc` and `html2text` are **not
installed**; do not reach for them.

A targeted run names a source file, but the page is still where you look for
anything beyond the single claim: `just docs`, then
`w3m -dump -cols 100 target/doc/qp/<module>/index.html`.

## Enumerating pages: the `find` gotchas

Enumerate the pages and count them:

```
find target/doc/qp -name 'index.html' | sort
```

That is the crate front page plus one page per module — 32 at this writing. Two
traps in the wider tree: `find target/doc -name '*.html'` picks up
`target/doc/src/`, the syntax-highlighted source mirror, which is not
documentation; and `find target/doc/qp -name '*.html'` stays out of that mirror
but still returns every per-item page (`struct.*.html`, `fn.*.html`), five times
the real count. Neither is the denominator. Restrict to `index.html`.

## Counting with `ripgrep`

**If you reach for `ripgrep`, three things.** `rg` is installed and is the
better tool over `src/`, but: `rg -c` and `grep -c` count *lines*, while
`rg -o <pat> | wc -l` counts *occurrences* — mixing them produced a wrong count
in a real audit. `rg -U` matches across line breaks, which is what you want on
wrapped doc prose (`tr '\n' ' '` does not help; it leaves the `///` prefixes
inline). And the trap: **`rg` obeys `.gitignore`, which ignores `/target`**, so a
bare `rg <pat>` from the repo root sweeps the sources and silently skips every
rendered page — a clean exit that looks like "no hits". Naming the path (`rg
<pat> target/doc/qp`) or `--no-ignore` restores them. The `just docs-dump` +
`grep` path above is unaffected and correct as written; do not "modernise" it.

## The smart-typography trap

rustdoc's markdown applies smart typography, so an unbackticked `--flag` becomes
`–flag` and fails silently when pasted into a shell. Grep the dump for `–`
adjacent to a word character to catch it; numeric ranges (`2–5`, `1–3 ms`) are
correct typography, not findings. Check too that `<placeholder>` did not vanish
as an unknown HTML tag — same class, different mechanism, both invisible in
source.
