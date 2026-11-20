//! Subcommands: the operations of `lode-ops`, plus the ones only the command line has.

pub use lode_ops::cmd::{
    admin, branch, changelist, checkout, checks, commit, conflict, diff, doctor, events, files,
    history, import, init, link, locks, login, merge, mergetool, proxy, revert, shelve, status,
    update, validate, view,
};

pub mod mcp;
pub mod self_update;
