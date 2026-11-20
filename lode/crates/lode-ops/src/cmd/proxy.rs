//! `lode proxy`: the caching proxy (T154) used for one server's file content.

use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::{OpError, OpResult};

/// `lode proxy` subcommands. The server is the workspace's (or `--server`, `LODE_SERVER`,
/// the one from `lode login`).
#[derive(Debug, clap::Subcommand)]
pub enum ProxyCmd {
    /// Download the server's file content through this caching proxy, such as
    /// https://lode-office.example.com. Everything else still goes to the server.
    Set {
        /// The proxy's URL.
        url: String,
    },
    /// Download file content from the server directly again.
    Clear,
    /// Show the proxy in use for the server and whether it answers.
    Show,
}

/// Runs a `lode proxy` subcommand.
pub async fn proxy(ctx: &Ctx, cmd: ProxyCmd) -> OpResult<Value> {
    let ws = ctx.workspace().ok();
    let server = ctx.server_url(ws.as_ref())?;
    match cmd {
        ProxyCmd::Set { url } => {
            let parsed = ctx.parse_server(&url)?;
            match ctx.anonymous_client(&parsed)?.info().await {
                Ok(info) if info.proxy.is_none() => {
                    return Err(OpError::usage(format!(
                        "{parsed} is a Lode server, not a caching proxy"
                    )))
                }
                Ok(_) => {}
                Err(e) => ctx
                    .out
                    .warn(format!("the proxy does not answer yet ({e}); saved anyway")),
            }
            let mut settings = ctx.settings()?;
            settings
                .proxies
                .insert(server.to_string(), parsed.to_string());
            ctx.save_settings(&settings)?;
            ctx.out.line(format!(
                "File content from {server} now comes through {parsed}."
            ));
            Ok(json!({ "server": server.as_str(), "proxy": parsed.as_str() }))
        }
        ProxyCmd::Clear => {
            let mut settings = ctx.settings()?;
            let removed = settings.proxies.remove(server.as_str()).is_some();
            ctx.save_settings(&settings)?;
            ctx.out
                .line(format!("File content comes from {server} directly."));
            if std::env::var_os("LODE_PROXY").is_some_and(|v| !v.is_empty()) {
                ctx.out.warn("LODE_PROXY is set and still names a proxy");
            }
            Ok(json!({ "server": server.as_str(), "removed": removed }))
        }
        ProxyCmd::Show => {
            let Some((url, source)) = ctx.proxy_for(&server)? else {
                ctx.out.line(format!("No caching proxy for {server}."));
                return Ok(json!({ "server": server.as_str(), "proxy": null }));
            };
            let parsed = ctx.parse_server(&url)?;
            let (reachable, upstream) = match ctx.anonymous_client(&parsed)?.info().await {
                Ok(info) => (true, info.proxy.map(|p| p.upstream)),
                Err(_) => (false, None),
            };
            let from = if source == "settings" {
                "lode proxy set"
            } else {
                source
            };
            ctx.out.line(format!(
                "{server} → {parsed} (from {from}){}",
                if reachable { "" } else { "; not answering" }
            ));
            Ok(json!({
                "server": server.as_str(),
                "proxy": parsed.as_str(),
                "source": source,
                "reachable": reachable,
                "upstream": upstream,
            }))
        }
    }
}
