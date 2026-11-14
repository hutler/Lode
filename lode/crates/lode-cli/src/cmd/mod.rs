//! Subcommands: the operations of `lode-ops`, plus the ones only the command line has.

pub use lode_ops::cmd::{
    admin, branch, changelist, checkout, checks, commit, diff, files, history, import, init, locks,
    login, merge, revert, shelve, status, update, view,
};

pub mod mcp;
pub mod self_update;
