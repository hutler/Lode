//! `lode://` links. The reader and writer live in `lode_ops::links`, shared with `lode link`
//! and the CLI's JSON output, so every program hands out and accepts the same links.

pub use lode_ops::links::*;
