//! `lode login`, `lode logout`, `lode whoami`.

use lode_proto::auth::LoginRequest;
use serde_json::{json, Value};

use crate::context::Ctx;
use crate::error::{OpError, OpResult};
use crate::output::Ui;

/// `lode login` options.
#[derive(Debug, Default, clap::Args)]
pub struct LoginArgs {
    /// Server URL, such as https://lode.example.com.
    pub url: Option<String>,
    /// Login name (prompted for if missing).
    #[arg(long)]
    pub name: Option<String>,
    /// Read a token from stdin instead of signing in with a password.
    #[arg(long)]
    pub token_stdin: bool,
    /// Read the password from stdin instead of prompting.
    #[arg(long)]
    pub password_stdin: bool,
    /// The password, for programs that already asked for it (not a command-line option).
    #[arg(skip)]
    pub password: Option<String>,
}

fn read_stdin_line(ui: &dyn Ui, what: &str) -> OpResult<String> {
    let line = ui.read_line()?.trim().to_owned();
    if line.is_empty() {
        return Err(OpError::usage(format!("no {what} on stdin")));
    }
    Ok(line)
}

fn prompt(ui: &dyn Ui, text: &str) -> OpResult<String> {
    if !ui.interactive() {
        return Err(OpError::usage(format!(
            "{} needed but stdin is not a terminal; pass it as an option",
            text.trim_end_matches(": ")
        )));
    }
    ui.prompt(text, false)
}

/// Signs in and stores the token for the server.
pub async fn login(ctx: &Ctx, args: LoginArgs) -> OpResult<Value> {
    let server = match &args.url {
        Some(u) => ctx.parse_server(u)?,
        None => ctx.server_url(None)?,
    };
    let anon = ctx.anonymous_client(&server)?;
    let ui = ctx.out.ui();
    let token = if args.token_stdin {
        read_stdin_line(ui, "token")?
    } else {
        if ctx.g.json && (args.name.is_none() || !(args.password_stdin || args.password.is_some()))
        {
            return Err(OpError::usage(
                "with --json, give --name and --password-stdin (or --token-stdin)",
            ));
        }
        let name = match args.name {
            Some(n) => n,
            None => prompt(ui, "Name: ")?,
        };
        let password = if let Some(p) = args.password {
            p
        } else if args.password_stdin {
            read_stdin_line(ui, "password")?
        } else {
            if !ui.interactive() {
                return Err(OpError::usage(
                    "password needed but stdin is not a terminal; use --password-stdin",
                ));
            }
            ui.prompt("Password: ", true)?
        };
        let host = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "this machine".into());
        let resp = anon
            .login(&LoginRequest {
                name,
                password,
                token_name: Some(format!("lode on {host}")),
            })
            .await?;
        resp.token
    };
    // Check the token before keeping it.
    let me = anon.with_token(token.clone()).whoami().await?;
    let file = ctx.credentials().save(&server, &token)?;
    let mut settings = ctx.settings()?;
    settings.server = Some(server.to_string());
    ctx.save_settings(&settings)?;
    ctx.out
        .line(format!("Signed in to {server} as {}.", me.principal.name));
    Ok(json!({
        "server": server.as_str(),
        "name": me.principal.name,
        "kind": me.principal.kind,
        "scopes": me.scopes,
        "token_file": file,
    }))
}

/// Revokes the stored token on the server (best effort) and forgets it.
pub async fn logout(ctx: &Ctx) -> OpResult<Value> {
    let server = ctx.server_url(None)?;
    let store = ctx.credentials();
    let Some(token) = store.load(&server)? else {
        ctx.out.line(format!("Not signed in to {server}."));
        return Ok(json!({ "server": server.as_str(), "removed": false }));
    };
    let client = ctx.anonymous_client(&server)?.with_token(token);
    let revoked = match client.whoami().await {
        Ok(me) => client.delete_token(me.token_id).await.is_ok(),
        Err(_) => false,
    };
    store.remove(&server)?;
    if !revoked {
        ctx.out
            .warn("the token could not be revoked on the server; it was only forgotten here");
    }
    ctx.out.line(format!("Signed out of {server}."));
    Ok(json!({ "server": server.as_str(), "removed": true, "revoked": revoked }))
}

/// Shows who the current token belongs to.
pub async fn whoami(ctx: &Ctx) -> OpResult<Value> {
    let ws = ctx.workspace().ok();
    let server = ctx.server_url(ws.as_ref())?;
    let me = ctx.client(&server)?.whoami().await?;
    let kind = match me.principal.kind {
        lode_proto::PrincipalKind::User => "user",
        lode_proto::PrincipalKind::Agent => "agent",
    };
    ctx.out.line(format!(
        "{} ({kind}) on {server}; scopes: {}",
        me.principal.name,
        me.scopes.join(", ")
    ));
    Ok(json!({
        "server": server.as_str(),
        "name": me.principal.name,
        "kind": kind,
        "owner": me.principal.owner,
        "scopes": me.scopes,
    }))
}
