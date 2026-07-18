//! Canonical queries over the qp schema.
//!
//! Layering (see `$QUIPU_VAULT/plans/2026-07-18-185716-audit-remediation.md`):
//!   `db.rs`    — connection, transactions, migrations, guarded-transition helpers
//!   `store.rs` — canonical read queries + the row types they return
//!   `cmd/*.rs` — argument parsing and rendering only, no SQL
//!
//! Why this module exists: the same queries were hand-written across many
//! command files in subtly divergent forms — the "latest agent" lookup existed
//! in 3 shapes across 11 sites, the unresolved-dep predicate in 9, the
//! event-tail SELECT in 3 column shapes across 6. Divergence is the risk, not
//! verbosity: adding a terminal state means updating every copy correctly, and
//! missing one is a silent logic bug.
//!
//! Scope discipline (deliberate, from the QP-68 research):
//!   - Read queries and their row types belong here.
//!   - Guarded-transition UPDATEs do NOT. They are not duplicated with each
//!     other — each has a distinct WHERE/SET — so moving them would relocate
//!     the highest-stakes code in the project for taxonomic tidiness alone.
//!   - Rendering helpers do NOT. `wrap_text`, `md_esc`, `html_esc`, `slugify`
//!     do no database work.

#![allow(dead_code)] // populated incrementally; some helpers land before their callers
