//! Subcommand implementations, one module per `qp` command.
//!
//! Mutators are edges in the task state machine; projections are read-only
//! views over the same data. See the crate docs for which is which.

pub mod abandon;
pub mod add;
pub mod assign;
pub mod block;
pub mod cancel;
pub mod claim;
pub mod complete;
pub mod decisions;
pub mod depends;
pub mod edit;
pub mod install_skills;
pub mod list;
pub mod log;
pub mod reclaim;
pub mod relation;
pub mod render;
pub mod report;
pub mod show;
pub mod status;
pub mod tag;
pub mod timeline;
pub mod tree;
pub mod wait;
pub mod watch;
pub mod wave;
