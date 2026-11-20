//! `lode login`, `lode logout`, `lode whoami`.

use lode_client::ServerUrl;
use lode_proto::auth::{ChangePasswordRequest, Info, LoginRequest, REASON_PASSWORD_CHANGE_REQUIRED};
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
    /// Sign in through the company's identity provider in a browser (single sign-on).
    #[arg(long, conflicts_with_all = ["name", "token_stdin", "password_stdin"])]
    pub sso: bool,
    /// With --sso: print the sign-in address instead of opening a browser, for a browser on
    /// another screen or computer that can still reach this one's loopback address.
    #[arg(long, requires = "sso")]
    pub no_browser: bool,
}

/// How long `--sso` waits for the browser.
const SSO_WAIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

fn token_label() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "this machine".into());
    format!("lode on {host}")
}

/// Signs in through the browser and returns the new token.
async fn sso_token(ctx: &Ctx, anon: &lode_client::Client, no_browser: bool) -> OpResult<String> {
    let info = anon.info().await?;
    let Some(sso) = info.sso else {
        return Err(OpError::usage(
            "this server does not offer single sign-on; sign in without --sso",
        ));
    };
    let pending = anon.begin_sso(Some(&token_label())).await?;
    let ui = ctx.out.ui();
    // Straight to the UI, not through `out`: with --json the address still has to reach
    // the person (stderr for the CLI).
    if !no_browser && ui.open_url(pending.url()) {
        ui.note(&format!(
            "Your browser opened to sign in with {}. If it did not, open this address:\n  {}",
            sso.label,
            pending.url()
        ));
    } else {
        ui.note(&format!(
            "Open this address in a browser on this computer to sign in with {}:\n  {}",
            sso.label,
            pending.url()
        ));
    }
    ui.note("Waiting for the browser (up to 5 minutes)...");
    Ok(pending.finish(SSO_WAIT).await?.token)
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
    // A wrong address (such as the web admin page's) gets a clear error here instead of
    // a refused sign-in.
    find_server(ctx, &server).await?;
    let anon = ctx.anonymous_client(&server)?;
    let ui = ctx.out.ui();
    let token = if args.sso {
        sso_token(ctx, &anon, args.no_browser).await?
    } else if args.token_stdin {
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
        let req = LoginRequest {
            name,
            password,
            token_name: Some(token_label()),
        };
        match anon.login(&req).await {
            Ok(resp) => resp.token,
            Err(lode_client::Error::Api(e))
                if e.details["reason"] == REASON_PASSWORD_CHANGE_REQUIRED
                    && ui.interactive()
                    && !ctx.g.json =>
            {
                // A first password: choose a new one now, then sign in with it.
                ui.note("This password must be changed before it can be used.");
                let new_password = ask_new_password(ui)?;
                anon.change_password(&ChangePasswordRequest {
                    name: req.name.clone(),
                    password: req.password,
                    new_password: new_password.clone(),
                })
                .await?;
                ui.note("Password changed.");
                anon.login(&LoginRequest {
                    password: new_password,
                    ..req
                })
                .await?
                .token
            }
            Err(e) => return Err(e.into()),
        }
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
        "sso": args.sso,
    }))
}

/// Asks for a new password twice, without echo, until both agree.
fn ask_new_password(ui: &dyn Ui) -> OpResult<String> {
    loop {
        let first = ui.prompt("New password: ", true)?;
        if first.chars().count() < MIN_PASSWORD_LEN {
            ui.note(&format!(
                "Too short; use at least {MIN_PASSWORD_LEN} characters."
            ));
            continue;
        }
        if ui.prompt("New password again: ", true)? == first {
            return Ok(first);
        }
        ui.note("The passwords differ.");
    }
}

/// Shortest password the server takes (it checks too).
const MIN_PASSWORD_LEN: usize = 8;

/// `lode passwd` options.
#[derive(Debug, Default, clap::Args)]
pub struct PasswdArgs {
    /// Server URL [default: the workspace's or the last one signed in to].
    pub url: Option<String>,
    /// Whose password [default: who you are signed in as].
    #[arg(long)]
    pub name: Option<String>,
    /// Read the current and the new password from stdin, one per line.
    #[arg(long)]
    pub password_stdin: bool,
}

/// Changes one's own password, given the current one. Also replaces a first password that
/// must be changed, before any sign-in.
pub async fn passwd(ctx: &Ctx, args: PasswdArgs) -> OpResult<Value> {
    let server = match &args.url {
        Some(u) => ctx.parse_server(u)?,
        None => ctx.server_url(ctx.workspace().ok().as_ref())?,
    };
    let anon = ctx.anonymous_client(&server)?;
    let ui = ctx.out.ui();
    let name = match args.name {
        Some(n) => n,
        None => match ctx.client(&server) {
            Ok(c) => match c.whoami().await {
                Ok(me) => me.principal.name,
                Err(_) => prompt(ui, "Name: ")?,
            },
            Err(_) => prompt(ui, "Name: ")?,
        },
    };
    let (password, new_password) = if args.password_stdin {
        (
            read_stdin_line(ui, "current password")?,
            read_stdin_line(ui, "new password")?,
        )
    } else {
        if !ui.interactive() {
            return Err(OpError::usage(
                "passwords needed but stdin is not a terminal; use --password-stdin",
            ));
        }
        (ui.prompt("Current password: ", true)?, ask_new_password(ui)?)
    };
    let resp = anon
        .change_password(&ChangePasswordRequest {
            name,
            password,
            new_password,
        })
        .await?;
    ctx.out
        .line(format!("Changed the password of {} on {server}.", resp.name));
    Ok(json!({ "server": server.as_str(), "name": resp.name }))
}

/// The single sign-on a server offers (`{label, password_login}`), or `null` when it offers
/// none. Needs no token.
pub async fn sso_offer(ctx: &Ctx, url: &str) -> OpResult<Value> {
    let server = ctx.parse_server(url)?;
    let info = ctx.anonymous_client(&server)?.info().await?;
    Ok(sso_json(&info))
}

/// What answers at `url`: `{server, version, sso}` when it is a Lode server, with `sso` as
/// [`sso_offer`] gives it. Otherwise a `not_a_lode_server` error, whose `details.suggested`
/// holds the address to use instead when a Lode server answers at the same host without the
/// path. Needs no token.
pub async fn server_info(ctx: &Ctx, url: &str) -> OpResult<Value> {
    let server = ctx.parse_server(url)?;
    let info = find_server(ctx, &server).await?;
    Ok(json!({
        "server": server.as_str(),
        "version": info.server_version,
        "sso": sso_json(&info),
    }))
}

fn sso_json(info: &Info) -> Value {
    match &info.sso {
        Some(s) => json!({ "label": s.label, "password_login": s.password_login }),
        None => Value::Null,
    }
}

/// Asks `server` what it is, and turns an answer that is not Lode's into an error that names
/// the right address when there is one. `/info` needs no token and every Lode server answers
/// it, so anything but a connection problem or a server that is down means the address
/// points somewhere else: a web page, the admin pages under `/admin/`, another program.
async fn find_server(ctx: &Ctx, server: &ServerUrl) -> OpResult<Info> {
    use lode_client::Error as E;
    match ctx.anonymous_client(server)?.info().await {
        Ok(info) => Ok(info),
        Err(e @ (E::Transport(_) | E::InsecureUrl(_) | E::InvalidUrl { .. })) => Err(e.into()),
        Err(e) if e.is_transient() => Err(e.into()),
        Err(_) => Err(not_a_lode_server(ctx, server).await),
    }
}

async fn not_a_lode_server(ctx: &Ctx, server: &ServerUrl) -> OpError {
    let mut root = server.clone();
    root.set_path("/");
    if root.path() != server.path() {
        if let Ok(client) = ctx.anonymous_client(&root) {
            if client.info().await.is_ok() {
                let shown = root.as_str().trim_end_matches('/');
                return OpError::failed(
                    "not_a_lode_server",
                    format!(
                        "no Lode server answers at {server}; the server is at {shown}, use that address"
                    ),
                )
                .with_details(json!({ "server": server.as_str(), "suggested": shown }));
            }
        }
    }
    OpError::failed(
        "not_a_lode_server",
        format!("no Lode server answers at {server}; check the address"),
    )
    .with_details(json!({ "server": server.as_str() }))
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
