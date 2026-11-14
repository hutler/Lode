//! What a workspace points at. The command line shows this as part of `lode status`; programs
//! such as the desktop app ask for it on its own.

use lode_workspace::db::keys;
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::OpResult;
use crate::local;

/// The workspace containing the context's directory: `{"root","server","depot","branch",
/// "rev"}`, each null when the workspace does not record it.
pub async fn info(ctx: &Ctx) -> OpResult<Value> {
    let ws = local::open(ctx)?;
    let db = ws.db();
    Ok(json!({
        "root": ws.root().display().to_string(),
        "server": db.config(keys::SERVER_URL)?,
        "depot": db.config(keys::DEPOT)?,
        "branch": db.config(keys::BRANCH)?,
        "rev": db.config_u64(keys::SYNCED_REV)?,
    }))
}
