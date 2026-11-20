//! Operations, one module per `lode` command family. Each returns the JSON document its
//! command prints with `--json` (docs/CLI-JSON.md).

pub mod admin;
pub mod apply;
pub mod branch;
pub mod changelist;
pub mod checkout;
pub mod checks;
pub mod commit;
pub mod conflict;
pub mod diff;
pub mod files;
pub mod history;
pub mod import;
pub mod info;
pub mod init;
pub mod locks;
pub mod login;
pub mod merge;
pub mod mergetool;
pub mod proxy;
pub mod revert;
pub mod shelve;
pub mod status;
pub mod update;
pub mod view;
