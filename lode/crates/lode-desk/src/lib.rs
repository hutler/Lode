//! Lode Desk, the Lode desktop client.
//!
//! The app is a Slint window ([`app`]) over [`desk::Desk`], which runs `lode-ops`
//! operations and returns the same JSON documents `lode <command> --json` prints. Operations
//! open the workspace only while they run, so `lode` and editor plugins work next to the app.
//! The former Tauri window stays behind the `web` feature until M13 is complete.

// `deny`, not `forbid`: the code Slint generates for the screens allows itself `unsafe`.
#![deny(unsafe_code)]

pub mod app;
pub mod branches;
pub mod changes;
pub mod desk;
pub mod diffs;
pub mod history;
pub mod links;
pub mod merge;
pub mod team;
pub mod update;
pub mod views;
pub mod watch;
#[cfg(feature = "web")]
pub mod web;
