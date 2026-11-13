//! `lode admin`: server administration over the HTTP API (depots, principals, groups,
//! tokens, access rules, maintenance). Needs a token with the `admin` scope; nothing here
//! touches the server's database directly.

use std::io::{BufRead, IsTerminal};

use lode_client::Client;
use lode_core::{DepotPath, PathGlobSet};
use lode_proto::admin::{
    AccessCheck, AccessLevel, AclEffect, AclRule, AclSubject, CreatePrincipalRequest,
    MaintenanceReport, PatchPrincipalRequest,
};
use lode_proto::auth::{CreateTokenRequest, PrincipalView, TokenView};
use lode_proto::webhooks::{CreateWebhookRequest, DeliveryState, DeliveryView, WebhookView};
use lode_proto::PrincipalKind;
use serde_json::{json, Value};

use crate::cmd::history::format_time;
use crate::context::Ctx;
use crate::error::{CliError, CliResult};

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
    /// Create or list depots.
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
    List,
}

/// How a password is given.
#[derive(Debug, clap::Args)]
pub struct PasswordArgs {
    /// Read the password from stdin instead of prompting.
    #[arg(long)]
    pub password_stdin: bool,
}

/// `lode admin user`.
#[derive(Debug, clap::Subcommand)]
pub enum UserCmd {
    /// Create a user with a password.
    Create {
        /// Login name.
        name: String,
        /// Display name.
        #[arg(long)]
        display_name: Option<String>,
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
    /// Set a user's password.
    Passwd {
        /// Login name.
        name: String,
        #[command(flatten)]
        password: PasswordArgs,
    },
}

/// `lode admin agent`.
#[derive(Debug, clap::Subcommand)]
pub enum AgentCmd {
    /// Create an agent owned by a user. Give it a token with `lode admin token create --for`.
    Create {
        /// Name.
        name: String,
        /// The user who owns it.
        #[arg(long)]
        owner: String,
        /// Display name.
        #[arg(long)]
        display_name: Option<String>,
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
    List,
    /// Add a rule: EFFECT LEVEL SUBJECT, such as `allow write group:devs`.
    Add {
        /// `allow` or `deny`.
        #[arg(value_parser = ["allow", "deny"])]
        effect: String,
        /// `read`, `write` or `admin`. A deny refuses this level and every higher one.
        #[arg(value_parser = ["read", "write", "admin"])]
        level: String,
        /// `user:NAME` (or just NAME), `group:NAME`, or `all`.
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

fn client(ctx: &Ctx) -> CliResult<Client> {
    let ws = ctx.workspace().ok();
    let server = ctx.server_url(ws.as_ref())?;
    ctx.client(&server)
}

fn to_json(v: &impl serde::Serialize) -> CliResult<Value> {
    serde_json::to_value(v).map_err(|e| CliError::failed("internal", e.to_string()))
}

/// A password from stdin (`--password-stdin`) or typed twice at a prompt.
fn password(ctx: &Ctx, args: &PasswordArgs) -> CliResult<String> {
    if args.password_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let p = line.trim_end_matches(['\r', '\n']).to_owned();
        if p.is_empty() {
            return Err(CliError::usage("no password on stdin"));
        }
        return Ok(p);
    }
    if ctx.g.json || !std::io::stdin().is_terminal() {
        return Err(CliError::usage(
            "password needed but stdin is not a terminal; use --password-stdin",
        ));
    }
    let first = rpassword::prompt_password("New password: ")?;
    let again = rpassword::prompt_password("Again: ")?;
    if first != again {
        return Err(CliError::usage("the passwords differ"));
    }
    if first.is_empty() {
        return Err(CliError::usage("empty password"));
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
        if p.disabled {
            line.push_str(" [disabled]");
        }
        ctx.out.line(line);
    }
}

async fn principals_of(ctx: &Ctx, c: &Client, kind: PrincipalKind) -> CliResult<Value> {
    let all = c.principals().await?;
    let list: Vec<&PrincipalView> = all.principals.iter().filter(|p| p.kind == kind).collect();
    print_principals(ctx, &list);
    Ok(json!({ "principals": list }))
}

async fn set_disabled(ctx: &Ctx, c: &Client, name: &str, enable: bool) -> CliResult<Value> {
    let p = c
        .patch_principal(
            name,
            &PatchPrincipalRequest {
                disabled: Some(!enable),
                password: None,
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

fn parse_subject(s: &str) -> CliResult<AclSubject> {
    match s.split_once(':') {
        None if s == "all" || s == "*" => Ok(AclSubject::All),
        None => Ok(AclSubject::User(s.to_owned())),
        Some(("user", n)) if !n.is_empty() => Ok(AclSubject::User(n.to_owned())),
        Some(("group", n)) if !n.is_empty() => Ok(AclSubject::Group(n.to_owned())),
        _ => Err(CliError::usage(format!(
            "`{s}` is not a subject: use user:NAME, group:NAME or all"
        ))),
    }
}

fn subject_text(s: &AclSubject) -> String {
    match s {
        AclSubject::All => "all".to_owned(),
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

fn check_glob(what: &str, pattern: &str) -> CliResult<()> {
    if pattern.is_empty() {
        return Err(CliError::usage(format!("the {what} pattern is empty")));
    }
    PathGlobSet::new([pattern])
        .map(|_| ())
        .map_err(|e| CliError::usage(format!("bad {what} pattern `{pattern}`: {e}")))
}

fn depot_path(p: &str) -> CliResult<DepotPath> {
    DepotPath::parse(p.trim_matches('/'))
        .map_err(|e| CliError::usage(format!("`{p}` is not a depot path: {e}")))
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
        "{:>4}  {}  events {}{}{}",
        h.id,
        h.url,
        h.events.join(","),
        h.branch
            .as_deref()
            .map(|b| format!("  branch {b}"))
            .unwrap_or_default(),
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

async fn webhook(ctx: &Ctx, c: &Client, cmd: WebhookCmd) -> CliResult<Value> {
    match cmd {
        WebhookCmd::Add {
            depot,
            url,
            events,
            branch,
        } => {
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
                    },
                )
                .await?;
            // The secret goes to stdout once; the rest is commentary on stderr.
            ctx.out.line(&created.secret);
            if !ctx.g.json {
                eprintln!(
                    "Webhook {} on {}: {}. The secret above is shown only now; receivers check \
                     X-Lode-Signature with it.",
                    created.hook.id,
                    created.hook.depot,
                    webhook_line(&created.hook).trim()
                );
            }
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

/// Runs `lode admin`.
pub async fn admin(ctx: &Ctx, args: AdminArgs) -> CliResult<Value> {
    let c = client(ctx)?;
    match args.command {
        AdminCmd::Depot(DepotCmd::Create { name }) => {
            let d = c.create_depot(&name).await?;
            ctx.out.line(format!("Created depot {}", d.name));
            to_json(&d)
        }
        AdminCmd::Depot(DepotCmd::List) => {
            let list = c.depots().await?;
            for d in &list.depots {
                ctx.out.line(&d.name);
            }
            to_json(&list)
        }
        AdminCmd::User(UserCmd::Create {
            name,
            display_name,
            password: pw,
        }) => {
            let password = password(ctx, &pw)?;
            let p = c
                .create_principal(&CreatePrincipalRequest {
                    kind: PrincipalKind::User,
                    name,
                    display_name,
                    owner: None,
                    password: Some(password),
                })
                .await?;
            ctx.out.line(format!("Created user {}", p.name));
            to_json(&p)
        }
        AdminCmd::User(UserCmd::List) => principals_of(ctx, &c, PrincipalKind::User).await,
        AdminCmd::User(UserCmd::Disable { name, enable }) => {
            set_disabled(ctx, &c, &name, enable).await
        }
        AdminCmd::User(UserCmd::Passwd { name, password: pw }) => {
            let password = password(ctx, &pw)?;
            let p = c
                .patch_principal(
                    &name,
                    &PatchPrincipalRequest {
                        disabled: None,
                        password: Some(password),
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
        }) => {
            let p = c
                .create_principal(&CreatePrincipalRequest {
                    kind: PrincipalKind::Agent,
                    name,
                    display_name,
                    owner: Some(owner),
                    password: None,
                })
                .await?;
            ctx.out.line(format!(
                "Created agent {} (owner {}); give it a token with `lode admin token create --for {}`",
                p.name,
                p.owner.as_deref().unwrap_or("?"),
                p.name
            ));
            to_json(&p)
        }
        AdminCmd::Agent(AgentCmd::List) => principals_of(ctx, &c, PrincipalKind::Agent).await,
        AdminCmd::Agent(AgentCmd::Disable { name, enable }) => {
            set_disabled(ctx, &c, &name, enable).await
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
            if !ctx.g.json {
                eprintln!(
                    "Token {} for {} with scopes {}. It is shown only now.",
                    resp.info.id,
                    for_.as_deref().unwrap_or("you"),
                    resp.info.scopes.join(", ")
                );
            }
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
        AdminCmd::Acl(AclCmd::List) => {
            let list = c.acl().await?;
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
                        return Err(CliError::usage("--check needs --depot"));
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
                return Err(CliError::failed(
                    "fsck_failed",
                    format!("{} problem(s) found in depot {depot}", r.problems.len()),
                )
                .with_details(v));
            }
            Ok(v)
        }
    }
}
