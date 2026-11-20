//! Lode's client operations, shared by the `lode` command line and the Lode Desk app.
//!
//! Every operation takes a [`context::Ctx`] and its options and returns the JSON document
//! that `lode <command> --json` prints (`docs/CLI-JSON.md`), or an [`error::OpError`] with
//! the same `code`, message, details and exit class. Human output, progress and questions go
//! through the [`output::Ui`] the context was made with; nothing here reads standard input
//! or writes to the terminal.
//!
//! ```no_run
//! # async fn demo() -> Result<(), lode_ops::error::OpError> {
//! use std::sync::Arc;
//! use lode_ops::context::{Ctx, Globals};
//! use lode_ops::output::Silent;
//!
//! let ctx = Ctx::in_dir(Globals::default(), "/work/game".as_ref(), Arc::new(Silent))?;
//! let status = lode_ops::cmd::status::status(&ctx, Default::default()).await?;
//! println!("{}", status["branch"]);
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]

pub mod cmd;
pub mod context;
pub mod crash;
pub mod error;
pub mod links;
pub mod local;
pub mod materialize;
pub mod output;
pub mod templates;
