//! `lode admin`: server administration over the HTTP API (depots, principals, groups,
//! tokens, access rules, maintenance). Needs a token with the `admin` scope; nothing here
//! touches the server's database directly.

use lode_client::Client;
use lode_core::{DepotPath, PathGlobSet};
use lode_proto::admin::{
    AccessCheck, AccessLevel, AclEffect, AclRule, AclSubject, CreatePrincipalRequest,
    MaintenanceReport, ObliterateReport, ObliterateRequest, ObliterateTarget,
    PatchPrincipalRequest,
};
use lode_proto::auth::{CreateTokenRequest, PrincipalView, TokenView};
use lode_proto::webhooks::{
    CreateWebhookRequest, DeliveryState, DeliveryView, WebhookFormat, WebhookView,
};
use lode_proto::PrincipalKind;
use serde_json::{json, Value};

use crate::cmd::history::format_time;
use crate::context::Ctx;
use crate::error::{OpError, OpResult};

/// `lode admin` options.
#[derive(Debug, clap::Args)]
#[command(
    long_about = "Administer a server: depots, users, agents, groups, tokens, access rules,
garbage collection and consistency checks. Uses the stored token for the server (`lode login`),
which needs the `admin` scope.
\nToken values are printed once, on stdout, and never stored or logged by this command.
\nWith --json each subcommand prints the server's answer: a principal is {\"name\",\"kind\",
\"display_name\"?,\"owner\"?,\"disabled\"?}; a token {\"id\",\"name\",\"scopes\",\"created_at\",
\"expires_at\"?,\"last_used_at\"?}; a rule {\"id\",\"depot\"?,\"subject\":{\"kind\",\"name\"?},
\"branch\",\"path\",\"effect\",\"level\"}; a check {\"principal\",\"depot\",\"branch\",\"path\",
\"level\":\"read\"|\"write\"|\"admin\"|null}."
)]
pub struct AdminArgs {
    #[command(subcommand)]
    pub command: AdminCmd,
}

/// `lode admin` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum AdminCmd {
    /// Create, list, delete or restore depots.
    #[command(subcommand)]
    Depot(DepotCmd),
    /// Manage users.
    #[command(subcommand)]
    User(UserCmd),
    /// Manage agents: principals owned by a user, for automated tools.
    #[command(subcommand)]
    Agent(AgentCmd),
    /// Manage groups.
    #[command(subcommand)]
    Group(GroupCmd),
    /// Create, list or revoke tokens (your own, or another principal's with --for).
    #[command(subcommand)]
    Token(TokenCmd),
    /// Manage access rules.
    #[command(subcommand)]
    Acl(AclCmd),
    /// Remove objects no revision uses any more.
    Gc {
        /// Depot.
        depot: String,
        /// Report what would be removed, and remove nothing.
        #[arg(long, short = 'n')]
        dry_run: bool,
    },
    /// Check that a depot's revisions and objects are intact.
    Fsck {
        /// Depot.
        depot: String,
        /// Also re-hash every object.
        #[arg(long)]
        full: bool,
    },
    /// Remove file content from a depot for good, such as a leaked password or a huge file
    /// committed by mistake. History stays; the content can no longer be read or stored.
    #[command(
        long_about = "Remove file content from a depot for good, such as a leaked password
or a huge file committed by mistake. Revisions, messages and paths stay; the stored content is
deleted, and reading it (cat, checkout, download) answers `obliterated`. Uploading the same
content again is refused.
\nPATH removes every version of a file, or of every file under a folder, on every branch.
PATH@REV removes only the version that revision had. Content still in the head of an active
branch is refused: commit its removal or a fixed file first.
\nShows what would be removed and asks before removing it; --yes skips the question, --dry-run
only shows. Copies of the content elsewhere are not reached: existing backups, workspaces and
shared caches on people's computers, and webhook payloads already sent."
    )]
    Obliterate {
        /// Depot.
        depot: String,
        /// `PATH` (every version) or `PATH@REV` (the version at a revision).
        #[arg(required = true)]
        paths: Vec<String>,
        /// Why; kept with the record. Required unless --dry-run.
        #[arg(long, short = 'm')]
        reason: Option<String>,
        /// Show what would be removed, and remove nothing.
        #[arg(long, short = 'n')]
        dry_run: bool,
        /// Do not ask before removing.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// List what was obliterated from a depot.
    Obliterated {
        /// Depot.
        depot: String,
    },
    /// Manage webhooks (needs the webhooks module on the server).
    #[command(subcommand)]
    Webhook(WebhookCmd),
}

/// `lode admin webhook`.
#[derive(Debug, clap::Subcommand)]
pub enum WebhookCmd {
    /// Register a URL for events of a depot. Prints the signing secret once, on stdout.
    Add {
        /// Depot.
        depot: String,
        /// Receiver URL (https://, or http:// if the server allows it).
        url: String,
        /// Events, comma-separated: revision.created, branch.created, branch.archived,
        /// lock.acquired, lock.released, shelf.created, shelf.updated, shelf.deleted,
        /// shelf.submitted, check.updated.
        #[arg(long, value_delimiter = ',', required = true)]
        events: Vec<String>,
        /// Only events on branches matching this glob.
        #[arg(long)]
        branch: Option<String>,
        /// What to send: lode (signed JSON, the default), or one line of text for slack,
        /// discord or telegram (a bot's sendMessage URL with ?chat_id=).
        #[arg(long, default_value = "lode", value_parser = ["lode", "slack", "discord", "telegram"])]
        format: String,
    },
    /// List the hooks of a depot.
    List {
        /// Depot.
        depot: String,
    },
    /// Remove a hook and its delivery history.
    Remove {
        /// Depot.
        depot: String,
        /// Hook id.
        id: i64,
    },
    /// Show the recent deliveries of a hook, newest first.
    Deliveries {
        /// Depot.
        depot: String,
        /// Hook id.
        id: i64,
    },
    /// Queue a `ping` event for a hook, to test the receiver.
    Ping {
        /// Depot.
        depot: String,
        /// Hook id.
        id: i64,
    },
}

/// `lode admin depot`.
#[derive(Debug, clap::Subcommand)]
pub enum DepotCmd {
    /// Create a depot.
    Create {
        /// Name.
        name: String,
    },
    /// List depots.
    ///
    /// For administrators, STORED SIZE shows the total length of files in the local
    /// depot store, cached for up to ten minutes. A dash means the size is unavailable
    /// (including S3 storage).
    List,
    /// Delete a depot. It goes to the trash, where it is kept for 30 days and can be
    /// restored; then it is erased. With --force it is erased now, history and content,
    /// which cannot be undone.
    Delete {
        /// Name.
        name: String,
        /// Erase it now instead of moving it to the trash; also erases a depot already in
        /// the trash.
        #[arg(long)]
        force: bool,
        /// Do not ask to type the name; needed when stdin is not a terminal.
        #[arg(long)]
        yes: bool,
    },
    /// Take a depot out of the trash.
    Restore {
        /// Name.
        name: String,
    },
    /// List depots in the trash and when each will be erased.
    Trash,
}

/// How a password is given.
#[derive(Debug, Default, clap::Args)]
pub struct PasswordArgs {
    /// Read the password from stdin instead of prompting.
    #[arg(long)]
    pub password_stdin: bool,
}

/// `lode admin user`.
#[derive(Debug, clap::Subcommand)]
pub enum UserCmd {
    /// Create a user with a password, or one who signs in with single sign-on (--sso).
    Create {
        /// Login name.
        name: String,
        /// Display name.
        #[arg(long)]
        display_name: Option<String>,
        /// Link the identity the company's sign-in names them by, such as their e-mail
        /// address. No password is asked for unless --password-stdin is given.
        #[arg(long, value_name = "IDENTITY")]
        sso: Option<String>,
        #[command(flatten)]
        password: PasswordArgs,
    },
    /// List users.
    List,
    /// Disable a user (or enable again with --enable): they can no longer authenticate.
    Disable {
        /// Login name.
        name: String,
        /// Enable instead.
        #[arg(long)]
        enable: bool,
    },
    /// Delete a user and the agents they own. They can no longer sign in and disappear
    /// from lists; their tokens, group memberships, access rules and locks go. Their name
    /// stays in history and cannot be given to someone else.
    Delete {
        /// Login name.
        name: String,
        /// Do not ask; needed when stdin is not a terminal.
        #[arg(long)]
        yes: bool,
    },
    /// Set a user's password.
    Passwd {
        /// Login name.
        name: String,
        #[command(flatten)]
        password: PasswordArgs,
    },
    /// Link a user to a single sign-on identity, such as their e-mail address, or remove
    /// the link with --remove.
    Sso {
        /// Login name.
        name: String,
        /// The identity.
        #[arg(required_unless_present = "remove", conflicts_with = "remove")]
        identity: Option<String>,
        /// Remove the link.
        #[arg(long)]
        remove: bool,
    },
}

/// `lode admin agent`.
#[derive(Debug, clap::Subcommand)]
pub enum AgentCmd {
    /// Create an agent owned by a user; with --with-token and --grant, ready to work in one
    /// step.
    Create {
        /// Name.
        name: String,
        /// The user who owns it.
        #[arg(long)]
        owner: String,
        /// Display name.
        #[arg(long)]
        display_name: Option<String>,
        /// Allow the agent LEVEL (read, write or admin) in DEPOT, on every branch and path
        /// (repeatable): `--grant game=write`.
        #[arg(long = "grant", value_name = "DEPOT=LEVEL")]
        grants: Vec<String>,
        /// Also create a token for it and print it (once). Scopes: the agent defaults (read,
        /// lock, shelve) unless --scope is given.
        #[arg(long)]
        with_token: bool,
        /// With --with-token: the token's scopes, such as read,shelve (repeatable).
        #[arg(long = "scope", value_delimiter = ',', requires = "with_token")]
        scopes: Vec<String>,
    },
    /// List agents.
    List,
    /// Disable an agent (or enable again with --enable).
    Disable {
        /// Name.
        name: String,
        /// Enable instead.
        #[arg(long)]
        enable: bool,
    },
    /// Delete an agent: its tokens stop working and its name stays in history.
    Delete {
        /// Name.
        name: String,
        /// Do not ask; needed when stdin is not a terminal.
        #[arg(long)]
        yes: bool,
    },
}

/// `lode admin group`.
#[derive(Debug, clap::Subcommand)]
pub enum GroupCmd {
    /// Create a group.
    Create {
        /// Name.
        name: String,
    },
    /// Add a member.
    Add {
        /// Group.
        group: String,
        /// User or agent.
        member: String,
    },
    /// Remove a member.
    Remove {
        /// Group.
        group: String,
        /// User or agent.
        member: String,
    },
    /// List groups with their members.
    List,
}

/// `lode admin token`.
#[derive(Debug, clap::Subcommand)]
pub enum TokenCmd {
    /// Create a token and print it (once).
    Create {
        /// For this principal instead of yourself (an admin, or an agent's owner).
        #[arg(long = "for", value_name = "PRINCIPAL")]
        for_: Option<String>,
        /// Label.
        #[arg(long, default_value = "lode admin")]
        name: String,
        /// Scopes, such as read,write (repeatable). Required for users; an agent gets
        /// read plus the agent defaults when none are given.
        #[arg(long = "scope", value_delimiter = ',')]
        scopes: Vec<String>,
        /// Expire after this many days.
        #[arg(long, value_name = "DAYS")]
        expires_in_days: Option<u32>,
    },
    /// List tokens (never their values).
    List {
        /// Another principal's tokens.
        #[arg(long = "for", value_name = "PRINCIPAL")]
        for_: Option<String>,
    },
    /// Revoke a token.
    Revoke {
        /// Token id, as `list` shows it.
        id: i64,
        /// Another principal's token.
        #[arg(long = "for", value_name = "PRINCIPAL")]
        for_: Option<String>,
    },
}

/// `lode admin acl`.
#[derive(Debug, clap::Subcommand)]
pub enum AclCmd {
    /// List the rules in evaluation order.
    List {
        /// Only the rules that can apply in this depot (its own and the server-wide ones).
        #[arg(long)]
        depot: Option<String>,
        /// Only this subject's rules: `user:NAME` (or just NAME), `group:NAME`, or `all`.
        #[arg(long)]
        subject: Option<String>,
    },
    /// Add a rule: EFFECT LEVEL SUBJECT, such as `allow write group:devs`.
    Add {
        /// `allow` or `deny`.
        #[arg(value_parser = ["allow", "deny"])]
        effect: String,
        /// `read`, `write` or `admin`. A deny refuses this level and every higher one.
        #[arg(value_parser = ["read", "write", "admin"])]
        level: String,
        /// `user:NAME` (or just NAME), `group:NAME`, `all`, or `agents` (every agent).
        subject: String,
        /// Only this depot (default: every depot).
        #[arg(long)]
        depot: Option<String>,
        /// Branch pattern.
        #[arg(long, default_value = "**")]
        branch: String,
        /// Path pattern.
        #[arg(long, default_value = "**")]
        path: String,
        /// Then show what PRINCIPAL gets on BRANCH and PATH (in --depot).
        #[arg(long, num_args = 3, value_names = ["PRINCIPAL", "BRANCH", "PATH"])]
        check: Option<Vec<String>>,
    },
    /// Remove a rule by id.
    Remove {
        /// Rule id, as `list` shows it.
        id: i64,
    },
    /// Show the access the rules give a principal on one path.
    Check {
        /// User or agent.
        principal: String,
        /// Depot.
        depot: String,
        /// Branch.
        branch: String,
        /// Path.
        path: String,
    },
}

fn client(ctx: &Ctx) -> OpResult<Client> {
    let ws = ctx.workspace().ok();
    let server = ctx.server_url(ws.as_ref())?;
    ctx.client(&server)
}

fn to_json(v: &impl serde::Serialize) -> OpResult<Value> {
    serde_json::to_value(v).map_err(|e| OpError::failed("internal", e.to_string()))
}

/// A password from stdin (`--password-stdin`) or typed twice at a prompt.
fn password(ctx: &Ctx, args: &PasswordArgs) -> OpResult<String> {
    let ui = ctx.out.ui();
    if args.password_stdin {
        let p = ui.read_line()?;
        if p.is_empty() {
            return Err(OpError::usage("no password on stdin"));
        }
        return Ok(p);
    }
    if ctx.g.json || !ui.interactive() {
        return Err(OpError::usage(
            "password needed but stdin is not a terminal; use --password-stdin",
        ));
    }
    let first = ui.prompt("New password: ", true)?;
    let again = ui.prompt("Again: ", true)?;
    if first != again {
        return Err(OpError::usage("the passwords differ"));
    }
    if first.is_empty() {
        return Err(OpError::usage("empty password"));
    }
    Ok(first)
}

fn kind_name(k: PrincipalKind) -> &'static str {
    match k {
        PrincipalKind::User => "user",
        PrincipalKind::Agent => "agent",
    }
}

fn print_principals(ctx: &Ctx, list: &[&PrincipalView]) {
    for p in list {
        let mut line = p.name.clone();
        if let Some(d) = &p.display_name {
            line.push_str(&format!(" ({d})"));
        }
        if let Some(o) = &p.owner {
            line.push_str(&format!(", owner {o}"));
        }
        if let Some(s) = &p.sso {
            line.push_str(&format!(", sso {s}"));
        }
        if p.disabled {
            line.push_str(" [disabled]");
        }
        ctx.out.line(line);
    }
}

async fn principals_of(ctx: &Ctx, c: &Client, kind: PrincipalKind) -> OpResult<Value> {
    let all = c.principals().await?;
    let list: Vec<&PrincipalView> = all.principals.iter().filter(|p| p.kind == kind).collect();
    print_principals(ctx, &list);
    Ok(json!({ "principals": list }))
}

async fn set_disabled(ctx: &Ctx, c: &Client, name: &str, enable: bool) -> OpResult<Value> {
    let p = c
        .patch_principal(
            name,
            &PatchPrincipalRequest {
                disabled: Some(!enable),
                password: None,
                sso: None,
            },
        )
        .await?;
    ctx.out.line(format!(
        "{} {} {}",
        if enable { "Enabled" } else { "Disabled" },
        kind_name(p.kind),
        p.name
    ));
    to_json(&p)
}

async fn delete_principal(ctx: &Ctx, c: &Client, name: &str, yes: bool) -> OpResult<Value> {
    if !yes {
        if ctx.g.json || !ctx.out.ui().interactive() {
            return Err(OpError::usage(
                "deleting cannot be undone; pass --yes to confirm",
            ));
        }
        let q = format!(
            "Delete {name}? They can no longer sign in, and their agents are deleted too. \
             This cannot be undone."
        );
        if !ctx.out.ui().confirm(&q)? {
            return Err(OpError::failed("aborted", "nothing was deleted"));
        }
    }
    let r = c.delete_principal(name).await?;
    ctx.out.line(format!("Deleted {}", r.name));
    for a in &r.agents {
        ctx.out
            .line(format!("Deleted agent {a} (owned by {})", r.name));
    }
    to_json(&r)
}

async fn delete_depot(
    ctx: &Ctx,
    c: &Client,
    name: &str,
    force: bool,
    yes: bool,
) -> OpResult<Value> {
    if !yes {
        if ctx.g.json || !ctx.out.ui().interactive() {
            return Err(OpError::usage(
                "pass --yes to confirm deleting a depot when stdin is not a terminal",
            ));
        }
        let what = if force {
            "This erases its history and content now and cannot be undone."
        } else {
            "It goes to the trash for 30 days and can be restored until then."
        };
        ctx.out.line(format!("Deleting depot {name}. {what}"));
        let typed = ctx
            .out
            .ui()
            .prompt("Type the depot's name to confirm: ", false)?;
        if typed.trim() != name {
            return Err(OpError::failed(
                "aborted",
                "the name did not match; nothing was deleted",
            ));
        }
    }
    let r = c.delete_depot(name, name, force).await?;
    if r.purged {
        ctx.out.line(format!("Erased depot {}", r.name));
    } else {
        let when = r
            .purge_after
            .map(format_time)
            .unwrap_or_else(|| "later".to_owned());
        ctx.out.line(format!(
            "Moved depot {} to the trash; it will be erased after {when}. Undo with `lode admin depot restore {}`",
            r.name, r.name
        ));
    }
    to_json(&r)
}

fn token_line(t: &TokenView) -> String {
    let mut line = format!(
        "{:>5}  {:<20} {:<24} created {}",
        t.id,
        t.name,
        t.scopes.join(","),
        format_time(t.created_at)
    );
    if let Some(e) = t.expires_at {
        line.push_str(&format!(", expires {}", format_time(e)));
    }
    if let Some(u) = t.last_used_at {
        line.push_str(&format!(", last used {}", format_time(u)));
    }
    line
}

fn parse_subject(s: &str) -> OpResult<AclSubject> {
    match s.split_once(':') {
        None if s == "all" || s == "*" => Ok(AclSubject::All),
        None if s == "agents" => Ok(AclSubject::Agents),
        None => Ok(AclSubject::User(s.to_owned())),
        Some(("user", n)) if !n.is_empty() => Ok(AclSubject::User(n.to_owned())),
        Some(("group", n)) if !n.is_empty() => Ok(AclSubject::Group(n.to_owned())),
        _ => Err(OpError::usage(format!(
            "`{s}` is not a subject: use user:NAME, group:NAME, all or agents"
        ))),
    }
}

/// `game=write` of `agent create --grant`.
fn parse_grant(g: &str) -> OpResult<(String, AccessLevel)> {
    let bad = || OpError::usage(format!("`{g}` is not DEPOT=LEVEL, such as game=write"));
    let (depot, level) = g.split_once('=').ok_or_else(bad)?;
    let level = match level {
        "read" => AccessLevel::Read,
        "write" => AccessLevel::Write,
        "admin" => AccessLevel::Admin,
        _ => return Err(bad()),
    };
    if depot.is_empty() {
        return Err(bad());
    }
    Ok((depot.to_owned(), level))
}

/// ` and read in game, write in art`, for the agent's creation message.
fn grants_text(rules: &[AclRule]) -> String {
    if rules.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = rules
        .iter()
        .map(|r| {
            format!(
                "{} in {}",
                level_name(r.level),
                r.depot.as_deref().unwrap_or("every depot")
            )
        })
        .collect();
    format!(", allowed {}", parts.join(", "))
}

fn subject_text(s: &AclSubject) -> String {
    match s {
        AclSubject::All => "all".to_owned(),
        AclSubject::Agents => "agents".to_owned(),
        AclSubject::User(n) => format!("user:{n}"),
        AclSubject::Group(n) => format!("group:{n}"),
    }
}

fn level_name(l: AccessLevel) -> &'static str {
    match l {
        AccessLevel::Read => "read",
        AccessLevel::Write => "write",
        AccessLevel::Admin => "admin",
    }
}

fn rule_line(r: &AclRule) -> String {
    format!(
        "{:>5}  {:<5} {:<5} {:<20} depot {:<12} branch {:<16} path {}",
        r.id.map(|i| i.to_string()).unwrap_or_default(),
        match r.effect {
            AclEffect::Allow => "allow",
            AclEffect::Deny => "deny",
        },
        level_name(r.level),
        subject_text(&r.subject),
        r.depot.as_deref().unwrap_or("*"),
        r.branch,
        r.path
    )
}

fn check_glob(what: &str, pattern: &str) -> OpResult<()> {
    if pattern.is_empty() {
        return Err(OpError::usage(format!("the {what} pattern is empty")));
    }
    PathGlobSet::new([pattern])
        .map(|_| ())
        .map_err(|e| OpError::usage(format!("bad {what} pattern `{pattern}`: {e}")))
}

fn depot_path(p: &str) -> OpResult<DepotPath> {
    DepotPath::parse(p.trim_matches('/'))
        .map_err(|e| OpError::usage(format!("`{p}` is not a depot path: {e}")))
}

fn print_check(ctx: &Ctx, c: &AccessCheck) {
    ctx.out.line(format!(
        "{} on {}:{} {}: {}",
        c.principal,
        c.depot,
        c.branch,
        c.path,
        c.level.map_or("no access", level_name)
    ));
}

fn webhook_line(h: &WebhookView) -> String {
    format!(
        "{:>4}  {}  events {}{}{}{}",
        h.id,
        h.url,
        h.events.join(","),
        h.branch
            .as_deref()
            .map(|b| format!("  branch {b}"))
            .unwrap_or_default(),
        if h.format.is_lode() {
            String::new()
        } else {
            format!("  format {}", h.format.as_str())
        },
        if h.active { "" } else { "  (inactive)" }
    )
}

fn delivery_line(d: &DeliveryView) -> String {
    let state = match d.state {
        DeliveryState::Pending => "pending",
        DeliveryState::Ok => "ok",
        DeliveryState::Failed => "failed",
    };
    let mut line = format!(
        "{:>6}  {:<16} {:<7} attempts {}  queued {}",
        d.id,
        d.event,
        state,
        d.attempts,
        format_time(d.created_at)
    );
    if let Some(s) = d.last_status {
        line.push_str(&format!("  status {s}"));
    }
    if let Some(e) = &d.last_error {
        line.push_str(&format!("  ({e})"));
    }
    if let Some(n) = d.next_attempt_at {
        line.push_str(&format!("  next {}", format_time(n)));
    }
    line
}

async fn webhook(ctx: &Ctx, c: &Client, cmd: WebhookCmd) -> OpResult<Value> {
    match cmd {
        WebhookCmd::Add {
            depot,
            url,
            events,
            branch,
            format,
        } => {
            let format = WebhookFormat::parse(&format)
                .ok_or_else(|| OpError::usage(format!("unknown webhook format `{format}`")))?;
            if let Some(b) = &branch {
                check_glob("branch", b)?;
            }
            let events: Vec<String> = events
                .into_iter()
                .map(|e| e.trim().to_owned())
                .filter(|e| !e.is_empty())
                .collect();
            let created = c
                .create_webhook(
                    &depot,
                    &CreateWebhookRequest {
                        url,
                        events,
                        branch,
                        format,
                    },
                )
                .await?;
            // The secret goes to stdout once; the rest is commentary on stderr.
            ctx.out.line(&created.secret);
            ctx.out.note(format!(
                "Webhook {} on {}: {}. The secret above is shown only now; receivers check \
                 X-Lode-Signature with it.",
                created.hook.id,
                created.hook.depot,
                webhook_line(&created.hook).trim()
            ));
            to_json(&created)
        }
        WebhookCmd::List { depot } => {
            let list = c.webhooks(&depot).await?;
            for h in &list.webhooks {
                ctx.out.line(webhook_line(h));
            }
            to_json(&list)
        }
        WebhookCmd::Remove { depot, id } => {
            c.delete_webhook(&depot, id).await?;
            ctx.out.line(format!("Removed webhook {id}"));
            Ok(json!({ "removed": id }))
        }
        WebhookCmd::Deliveries { depot, id } => {
            let list = c.webhook_deliveries(&depot, id).await?;
            for d in &list.deliveries {
                ctx.out.line(delivery_line(d));
            }
            to_json(&list)
        }
        WebhookCmd::Ping { depot, id } => {
            let d = c.ping_webhook(&depot, id).await?;
            ctx.out
                .line(format!("Queued ping delivery {} for webhook {id}", d.id));
            to_json(&d)
        }
    }
}

fn print_report(ctx: &Ctx, what: &str, r: &MaintenanceReport) {
    for p in &r.problems {
        ctx.out.line(p);
    }
    ctx.out.line(format!(
        "{what}: {} objects checked, {} affected, {} bytes{}",
        r.objects_checked,
        r.objects_affected,
        r.bytes_affected,
        if r.dry_run { " (dry run)" } else { "" }
    ));
}

fn print_obliterate(ctx: &Ctx, r: &ObliterateReport) {
    for o in &r.objects {
        ctx.out.line(format!(
            "{}  {}",
            o.id.to_hex().get(..12).unwrap_or_default(),
            crate::output::human_bytes(o.size)
        ));
        for u in &o.uses {
            ctx.out
                .line(format!("    r{:<6} {:<12} {}", u.rev, u.branch, u.path));
        }
    }
    for u in &r.in_use {
        ctx.out
            .line(format!("still in the head of {}: {}", u.branch, u.path));
    }
}

fn obliterate_targets(paths: &[String]) -> OpResult<Vec<ObliterateTarget>> {
    paths
        .iter()
        .map(|arg| {
            let (p, rev) = crate::cmd::history::split_rev(arg)?;
            let p = p.trim_matches('/');
            if p.is_empty() {
                return Err(OpError::usage("name a file or folder, not the whole depot"));
            }
            Ok(ObliterateTarget {
                path: depot_path(p)?,
                rev,
            })
        })
        .collect()
}

async fn obliterate(
    ctx: &Ctx,
    c: &Client,
    depot: &str,
    paths: &[String],
    reason: Option<String>,
    dry_run: bool,
    yes: bool,
) -> OpResult<Value> {
    let targets = obliterate_targets(paths)?;
    let reason = reason.unwrap_or_default();
    if !dry_run && reason.trim().is_empty() {
        return Err(OpError::usage(
            "say why with --reason (it is kept with the record)",
        ));
    }
    let preview = c
        .obliterate(
            depot,
            &ObliterateRequest {
                targets: targets.clone(),
                reason: reason.clone(),
                dry_run: true,
            },
        )
        .await?;
    print_obliterate(ctx, &preview);
    let total = format!(
        "{} object{}, {}",
        preview.objects.len(),
        if preview.objects.len() == 1 { "" } else { "s" },
        crate::output::human_bytes(preview.bytes)
    );
    if dry_run {
        ctx.out
            .line(format!("obliterate: {total} would be removed (dry run)"));
        return to_json(&preview);
    }
    if !preview.in_use.is_empty() {
        return Err(OpError::failed(
            "conflict",
            "the content is still in the head of an active branch; commit its removal or a \
             fixed file first",
        )
        .with_details(json!({ "in_use": to_json(&preview.in_use)? })));
    }
    if preview.objects.is_empty() {
        ctx.out.line("obliterate: nothing to remove");
        return to_json(&preview);
    }
    if !yes {
        if ctx.g.json || !ctx.out.ui().interactive() {
            return Err(OpError::usage(
                "obliterating cannot be undone; pass --yes to confirm (try --dry-run first)",
            ));
        }
        let q = format!("Remove {total} from {depot} for good? This cannot be undone.");
        if !ctx.out.ui().confirm(&q)? {
            return Err(OpError::failed("aborted", "nothing was removed"));
        }
    }
    let r = c
        .obliterate(
            depot,
            &ObliterateRequest {
                targets,
                reason,
                dry_run: false,
            },
        )
        .await?;
    ctx.out.line(format!(
        "obliterate: {} object{}, {} removed",
        r.objects.len(),
        if r.objects.len() == 1 { "" } else { "s" },
        crate::output::human_bytes(r.bytes)
    ));
    to_json(&r)
}

/// Runs `lode admin`.
pub async fn admin(ctx: &Ctx, args: AdminArgs) -> OpResult<Value> {
    let c = client(ctx)?;
    match args.command {
        AdminCmd::Depot(DepotCmd::Create { name }) => {
            let d = c.create_depot(&name).await?;
            ctx.out.line(format!("Created depot {}", d.name));
            to_json(&d)
        }
        AdminCmd::Depot(DepotCmd::List) => {
            let list = c.depots().await?;
            if list.depots.iter().any(|d| d.revisions.is_some()) {
                let width = list
                    .depots
                    .iter()
                    .map(|d| d.name.len())
                    .max()
                    .unwrap_or(0)
                    .max(5);
                ctx.out.line(format!(
                    "{:<width$}  {:>9}  {:>11}  LAST COMMIT",
                    "DEPOT", "REVISIONS", "STORED SIZE"
                ));
                for d in &list.depots {
                    ctx.out.line(format!(
                        "{:<width$}  {:>9}  {:>11}  {}",
                        d.name,
                        d.revisions.unwrap_or(0),
                        d.size_bytes
                            .map(crate::output::human_bytes)
                            .unwrap_or_else(|| "-".into()),
                        d.last_commit_at
                            .map(crate::cmd::history::format_time)
                            .unwrap_or_else(|| "-".into())
                    ));
                }
            } else {
                // An older server: names only.
                for d in &list.depots {
                    ctx.out.line(&d.name);
                }
            }
            to_json(&list)
        }
        AdminCmd::Depot(DepotCmd::Delete { name, force, yes }) => {
            delete_depot(ctx, &c, &name, force, yes).await
        }
        AdminCmd::Depot(DepotCmd::Restore { name }) => {
            let d = c.restore_depot(&name).await?;
            ctx.out.line(format!("Restored depot {}", d.name));
            to_json(&d)
        }
        AdminCmd::Depot(DepotCmd::Trash) => {
            let list = c.trash().await?;
            if list.depots.is_empty() {
                ctx.out.line("The trash is empty");
            }
            for d in &list.depots {
                ctx.out.line(format!(
                    "{}  deleted {}, erased after {}",
                    d.name,
                    format_time(d.deleted_at),
                    format_time(d.purge_after)
                ));
            }
            to_json(&list)
        }
        AdminCmd::User(UserCmd::Create {
            name,
            display_name,
            sso,
            password: pw,
        }) => {
            let password = if sso.is_none() || pw.password_stdin {
                Some(password(ctx, &pw)?)
            } else {
                None
            };
            let p = c
                .create_principal(&CreatePrincipalRequest {
                    kind: PrincipalKind::User,
                    name,
                    display_name,
                    owner: None,
                    password,
                    sso,
                })
                .await?;
            match &p.sso {
                Some(s) => ctx
                    .out
                    .line(format!("Created user {} (single sign-on as {s})", p.name)),
                None => ctx.out.line(format!("Created user {}", p.name)),
            }
            to_json(&p)
        }
        AdminCmd::User(UserCmd::Sso {
            name,
            identity,
            remove,
        }) => {
            let value = if remove {
                String::new()
            } else {
                identity.unwrap_or_default()
            };
            let p = c
                .patch_principal(
                    &name,
                    &PatchPrincipalRequest {
                        disabled: None,
                        password: None,
                        sso: Some(value),
                    },
                )
                .await?;
            match &p.sso {
                Some(s) => ctx
                    .out
                    .line(format!("{} signs in with single sign-on as {s}", p.name)),
                None => ctx
                    .out
                    .line(format!("{} has no single sign-on identity", p.name)),
            }
            to_json(&p)
        }
        AdminCmd::User(UserCmd::List) => principals_of(ctx, &c, PrincipalKind::User).await,
        AdminCmd::User(UserCmd::Disable { name, enable }) => {
            set_disabled(ctx, &c, &name, enable).await
        }
        AdminCmd::User(UserCmd::Delete { name, yes }) => {
            delete_principal(ctx, &c, &name, yes).await
        }
        AdminCmd::User(UserCmd::Passwd { name, password: pw }) => {
            let password = password(ctx, &pw)?;
            let p = c
                .patch_principal(
                    &name,
                    &PatchPrincipalRequest {
                        disabled: None,
                        password: Some(password),
                        sso: None,
                    },
                )
                .await?;
            ctx.out.line(format!("Changed the password of {}", p.name));
            to_json(&p)
        }
        AdminCmd::Agent(AgentCmd::Create {
            name,
            owner,
            display_name,
            grants,
            with_token,
            scopes,
        }) => {
            // Checked before anything is created.
            let grants = grants
                .iter()
                .map(|g| parse_grant(g))
                .collect::<OpResult<Vec<_>>>()?;
            let p = c
                .create_principal(&CreatePrincipalRequest {
                    kind: PrincipalKind::Agent,
                    name,
                    display_name,
                    owner: Some(owner),
                    password: None,
                    sso: None,
                })
                .await?;
            // From here on the agent exists: a failure says what was already done.
            let partly = |e: lode_client::Error, done: &str| {
                let mut e = OpError::from(e);
                e.message = format!(
                    "{}; agent {} was created{done}, so finish with `lode admin acl add` and \
                     `lode admin token create --for {}`",
                    e.message, p.name, p.name
                );
                e
            };
            let mut rules = Vec::new();
            for (depot, level) in grants {
                let rule = AclRule {
                    id: None,
                    depot: Some(depot),
                    subject: AclSubject::User(p.name.clone()),
                    branch: "**".into(),
                    path: "**".into(),
                    effect: AclEffect::Allow,
                    level,
                };
                let done = if rules.is_empty() {
                    String::new()
                } else {
                    format!(" with {} of its rules", rules.len())
                };
                rules.push(c.add_acl_rule(&rule).await.map_err(|e| partly(e, &done))?);
            }
            let token = if with_token {
                let req = CreateTokenRequest {
                    name: "agent".into(),
                    scopes,
                    expires_at: None,
                };
                let done = if rules.is_empty() {
                    String::new()
                } else {
                    " with its rules".to_owned()
                };
                Some(
                    c.create_token_for(&p.name, &req)
                        .await
                        .map_err(|e| partly(e, &done))?,
                )
            } else {
                None
            };
            match &token {
                Some(t) => {
                    // The value alone on stdout, as `token create` prints it.
                    ctx.out.line(&t.token);
                    ctx.out.note(format!(
                        "Created agent {} (owner {}){} with a token (scopes {}). It is shown only now.",
                        p.name,
                        p.owner.as_deref().unwrap_or("?"),
                        grants_text(&rules),
                        t.info.scopes.join(", ")
                    ));
                }
                None => ctx.out.line(format!(
                    "Created agent {} (owner {}){}; give it a token with `lode admin token create --for {}`",
                    p.name,
                    p.owner.as_deref().unwrap_or("?"),
                    grants_text(&rules),
                    p.name
                )),
            }
            let mut v = to_json(&p)?;
            if let Value::Object(m) = &mut v {
                if !rules.is_empty() {
                    m.insert(
                        "rules".into(),
                        serde_json::to_value(&rules).unwrap_or(Value::Null),
                    );
                }
                if let Some(t) = &token {
                    m.insert(
                        "token".into(),
                        serde_json::to_value(t).unwrap_or(Value::Null),
                    );
                }
            }
            Ok(v)
        }
        AdminCmd::Agent(AgentCmd::List) => principals_of(ctx, &c, PrincipalKind::Agent).await,
        AdminCmd::Agent(AgentCmd::Disable { name, enable }) => {
            set_disabled(ctx, &c, &name, enable).await
        }
        AdminCmd::Agent(AgentCmd::Delete { name, yes }) => {
            delete_principal(ctx, &c, &name, yes).await
        }
        AdminCmd::Group(GroupCmd::Create { name }) => {
            let g = c.create_group(&name).await?;
            ctx.out.line(format!("Created group {}", g.name));
            to_json(&g)
        }
        AdminCmd::Group(GroupCmd::Add { group, member }) => {
            c.add_group_member(&group, &member).await?;
            ctx.out.line(format!("Added {member} to {group}"));
            Ok(json!({ "group": group, "added": member }))
        }
        AdminCmd::Group(GroupCmd::Remove { group, member }) => {
            c.remove_group_member(&group, &member).await?;
            ctx.out.line(format!("Removed {member} from {group}"));
            Ok(json!({ "group": group, "removed": member }))
        }
        AdminCmd::Group(GroupCmd::List) => {
            let list = c.groups().await?;
            for g in &list.groups {
                ctx.out
                    .line(format!("{}: {}", g.name, g.members.join(", ")));
            }
            to_json(&list)
        }
        AdminCmd::Token(TokenCmd::Create {
            for_,
            name,
            scopes,
            expires_in_days,
        }) => {
            let expires_at = expires_in_days
                .map(|d| time::OffsetDateTime::now_utc() + time::Duration::days(i64::from(d)));
            let req = CreateTokenRequest {
                name,
                scopes,
                expires_at,
            };
            let resp = match &for_ {
                Some(p) => c.create_token_for(p, &req).await?,
                None => c.create_token(&req).await?,
            };
            // The value goes to stdout once; the rest is commentary on stderr.
            ctx.out.line(&resp.token);
            ctx.out.note(format!(
                "Token {} for {} with scopes {}. It is shown only now.",
                resp.info.id,
                for_.as_deref().unwrap_or("you"),
                resp.info.scopes.join(", ")
            ));
            to_json(&resp)
        }
        AdminCmd::Token(TokenCmd::List { for_ }) => {
            let list = match &for_ {
                Some(p) => c.tokens_for(p).await?,
                None => c.tokens().await?,
            };
            for t in &list.tokens {
                ctx.out.line(token_line(t));
            }
            to_json(&list)
        }
        AdminCmd::Token(TokenCmd::Revoke { id, for_ }) => {
            match &for_ {
                Some(p) => c.delete_token_for(p, id).await?,
                None => c.delete_token(id).await?,
            }
            ctx.out.line(format!("Revoked token {id}"));
            Ok(json!({ "revoked": id }))
        }
        AdminCmd::Acl(AclCmd::List { depot, subject }) => {
            let subject = subject.as_deref().map(parse_subject).transpose()?;
            let list = c
                .acl_filtered(
                    depot.as_deref(),
                    subject.as_ref().map(subject_text).as_deref(),
                )
                .await?;
            for r in &list.rules {
                ctx.out.line(rule_line(r));
            }
            to_json(&list)
        }
        AdminCmd::Acl(AclCmd::Add {
            effect,
            level,
            subject,
            depot,
            branch,
            path,
            check,
        }) => {
            check_glob("branch", &branch)?;
            check_glob("path", &path)?;
            let subject = parse_subject(&subject)?;
            let sample = match check.as_deref() {
                Some([p, b, path]) => {
                    let Some(d) = depot.clone() else {
                        return Err(OpError::usage("--check needs --depot"));
                    };
                    Some((p.clone(), d, b.clone(), depot_path(path)?))
                }
                _ => None,
            };
            let rule = AclRule {
                id: None,
                depot,
                subject,
                branch,
                path,
                effect: if effect == "deny" {
                    AclEffect::Deny
                } else {
                    AclEffect::Allow
                },
                level: match level.as_str() {
                    "admin" => AccessLevel::Admin,
                    "write" => AccessLevel::Write,
                    _ => AccessLevel::Read,
                },
            };
            let added = c.add_acl_rule(&rule).await?;
            ctx.out
                .line(format!("Added rule {}", rule_line(&added).trim()));
            let mut out = to_json(&added)?;
            if let Some((p, d, b, path)) = sample {
                let check = c.acl_check(&p, &d, &b, &path).await?;
                print_check(ctx, &check);
                out["check"] = to_json(&check)?;
            }
            Ok(out)
        }
        AdminCmd::Acl(AclCmd::Remove { id }) => {
            c.delete_acl_rule(id).await?;
            ctx.out.line(format!("Removed rule {id}"));
            Ok(json!({ "removed": id }))
        }
        AdminCmd::Acl(AclCmd::Check {
            principal,
            depot,
            branch,
            path,
        }) => {
            let check = c
                .acl_check(&principal, &depot, &branch, &depot_path(&path)?)
                .await?;
            print_check(ctx, &check);
            to_json(&check)
        }
        AdminCmd::Webhook(cmd) => webhook(ctx, &c, cmd).await,
        AdminCmd::Obliterate {
            depot,
            paths,
            reason,
            dry_run,
            yes,
        } => obliterate(ctx, &c, &depot, &paths, reason, dry_run, yes).await,
        AdminCmd::Obliterated { depot } => {
            let list = c.obliterated(&depot).await?;
            for o in &list.obliterated {
                ctx.out.line(format!(
                    "{}  {}  {:<10} {}  {}  ({})",
                    format_time(o.at),
                    o.id.to_hex().get(..12).unwrap_or_default(),
                    crate::output::human_bytes(o.size),
                    o.by,
                    o.path,
                    o.reason
                ));
            }
            if list.obliterated.is_empty() {
                ctx.out
                    .line(format!("Nothing was obliterated from {depot}."));
            }
            to_json(&list)
        }
        AdminCmd::Gc { depot, dry_run } => {
            let r = c.gc(&depot, dry_run).await?;
            print_report(ctx, "gc", &r);
            to_json(&r)
        }
        AdminCmd::Fsck { depot, full } => {
            let r = c.fsck(&depot, full).await?;
            print_report(ctx, "fsck", &r);
            let v = to_json(&r)?;
            if !r.problems.is_empty() {
                return Err(OpError::failed(
                    "fsck_failed",
                    format!("{} problem(s) found in depot {depot}", r.problems.len()),
                )
                .with_details(v));
            }
            Ok(v)
        }
    }
}
