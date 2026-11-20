//! Subcommands: the operations of `lode-ops`, plus the ones only the command line has.

pub use lode_ops::cmd::{
    admin, branch, changelist, checkout, checks, commit, conflict, diff, files, history, import,
    init, locks, login, merge, proxy, revert, shelve, status, update, view,
};

pub mod mcp;
pub mod self_update;
