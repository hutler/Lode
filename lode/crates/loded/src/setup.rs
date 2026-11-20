//! `loded setup`: first run without editing files (T082).
//!
//! Asks for the data directory, the domain (or none for local use), the listen address,
//! backups and the admin password; every answer can be given as a flag instead, and without a
//! terminal (or with `--yes`) the defaults are taken for anything not given. The resulting
//! `loded.toml` is the commented default with those answers filled in, and is parsed before it
//! is written.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use clap::Args;
use lode_server::config::{CONFIG_FILE_NAME, DEFAULT_CONFIG};
use lode_server::ServerConfig;

/// Flags of `loded setup`.
#[derive(Args, Debug, Default)]
pub struct SetupArgs {
    /// Data directory [default: /var/lib/lode, or C:\ProgramData\Lode on Windows].
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    /// Domain name for HTTPS with a Let's Encrypt certificate, such as lode.example.com.
    #[arg(long, conflicts_with = "local")]
    pub domain: Option<String>,
    /// No domain: plain HTTP on this machine only (or behind your own TLS proxy).
    #[arg(long)]
    pub local: bool,
    /// Contact address for certificate notices (with --domain).
    #[arg(long, requires = "domain")]
    pub email: Option<String>,
    /// Address and port to listen on [default: 0.0.0.0:443 with a domain, 127.0.0.1:7480
    /// without].
    #[arg(long)]
    pub listen: Option<SocketAddr>,
    /// Directory for daily backups [default: backups inside the data directory].
    #[arg(long, conflicts_with = "no_backup")]
    pub backup_dir: Option<PathBuf>,
    /// No scheduled backups.
    #[arg(long)]
    pub no_backup: bool,
    /// File holding the admin password on its first line; `-` reads standard input.
    #[arg(long, conflicts_with = "generate_admin_password")]
    pub admin_password_file: Option<PathBuf>,
    /// Make a random admin password and print it once. It must be changed at the first
    /// sign-in and stops working after 24 hours if unused. Taken anyway when there is no
    /// terminal and no --admin-password-file.
    #[arg(long)]
    pub generate_admin_password: bool,
    /// Also print an admin token, as setup did before 0.2.1 (for scripts). Without it the
    /// password is the only secret.
    #[arg(long)]
    pub print_admin_token: bool,
    /// Ask nothing: take the defaults for anything not given.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

/// The answers, complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answers {
    pub data_dir: PathBuf,
    pub domain: Option<String>,
    pub email: Option<String>,
    pub listen: SocketAddr,
    pub backup_dir: Option<PathBuf>,
}

fn default_data_dir() -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into());
        PathBuf::from(base).join("Lode")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/usr/local/var/lode")
    } else {
        PathBuf::from("/var/lib/lode")
    }
}

fn default_listen(domain: Option<&str>) -> SocketAddr {
    if domain.is_some() {
        SocketAddr::from(([0, 0, 0, 0], 443))
    } else {
        SocketAddr::from(([127, 0, 0, 1], 7480))
    }
}

/// Reads answers from a terminal.
struct Prompter<'a> {
    input: &'a mut dyn BufRead,
}

impl Prompter<'_> {
    fn ask(&mut self, question: &str, default: &str) -> anyhow::Result<String> {
        if default.is_empty() {
            print!("{question}: ");
        } else {
            print!("{question} [{default}]: ");
        }
        std::io::stdout().flush()?;
        let mut line = String::new();
        self.input.read_line(&mut line)?;
        let line = line.trim();
        Ok(if line.is_empty() {
            default.to_owned()
        } else {
            line.to_owned()
        })
    }
}

/// Fills in what the flags left open, asking when `prompter` is given.
fn answers(args: &SetupArgs, mut prompter: Option<Prompter<'_>>) -> anyhow::Result<Answers> {
    let data_dir = match (&args.data_dir, prompter.as_mut()) {
        (Some(d), _) => d.clone(),
        (None, Some(p)) => {
            PathBuf::from(p.ask("Data directory", &default_data_dir().display().to_string())?)
        }
        (None, None) => default_data_dir(),
    };
    let domain = match (&args.domain, args.local, prompter.as_mut()) {
        (Some(d), _, _) => Some(d.clone()),
        (None, true, _) | (None, false, None) => None,
        (None, false, Some(p)) => {
            let d = p.ask(
                "Domain name for HTTPS with Let's Encrypt (no domain? <public-ip>.sslip.io; \
                 empty: this machine only)",
                "",
            )?;
            (!d.is_empty()).then_some(d)
        }
    };
    let email = match (&args.email, &domain, prompter.as_mut()) {
        (Some(e), _, _) => Some(e.clone()),
        (None, Some(_), Some(p)) => {
            let e = p.ask("Email for certificate notices (optional)", "")?;
            (!e.is_empty()).then_some(e)
        }
        _ => None,
    };
    let listen = match (args.listen, prompter.as_mut()) {
        (Some(l), _) => l,
        (None, Some(p)) => {
            let default = default_listen(domain.as_deref()).to_string();
            let text = p.ask("Listen address", &default)?;
            text.parse()
                .with_context(|| format!("{text:?} is not an address such as 0.0.0.0:443"))?
        }
        (None, None) => default_listen(domain.as_deref()),
    };
    let backup_dir = match (&args.backup_dir, args.no_backup, prompter.as_mut()) {
        (Some(b), _, _) => Some(b.clone()),
        (None, true, _) => None,
        (None, false, None) => Some(PathBuf::from("backups")),
        (None, false, Some(p)) => {
            let b = p.ask(
                "Daily backups into (a path, best on another disk, or \"none\")",
                &data_dir.join("backups").display().to_string(),
            )?;
            (b != "none").then(|| PathBuf::from(b))
        }
    };
    Ok(Answers {
        data_dir,
        domain,
        email,
        listen,
        backup_dir,
    })
}

fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

/// The default configuration with the answers filled in.
pub fn config_text(a: &Answers) -> anyhow::Result<String> {
    let mut text = DEFAULT_CONFIG.to_owned();
    let listen_line = "\nlisten = \"127.0.0.1:7480\"\n";
    if !text.contains(listen_line) {
        bail!("the default configuration has no listen line");
    }
    text = text.replacen(
        listen_line,
        &format!("\nlisten = {}\n", toml_string(&a.listen.to_string())),
        1,
    );
    if let Some(domain) = &a.domain {
        let mut tls = format!("[tls]\nacme_domain = {}\n", toml_string(domain));
        if let Some(email) = &a.email {
            tls.push_str(&format!("acme_email = {}\n", toml_string(email)));
        }
        tls.push('\n');
        let at = text
            .find("[connections]")
            .context("the default configuration has no [connections] table")?;
        text.insert_str(at, &tls);
    }
    if let Some(dir) = &a.backup_dir {
        // Inside the data directory, written relative to it (loded.toml lives there).
        let dir = dir.strip_prefix(&a.data_dir).unwrap_or(dir);
        text.push_str(&format!(
            "\n[backup]\nevery = \"24h\"\nkeep = 7\ndir = {}\n",
            toml_string(&dir.to_string_lossy())
        ));
        text.push_str(BACKUP_COPY_HINT);
    }
    Ok(text)
}

/// Written after `[backup]`: how to send copies to another machine, ready to fill in.
const BACKUP_COPY_HINT: &str =
    "\n# To also send each backup to an S3-compatible bucket on another machine (Backblaze B2,\n\
# Wasabi, AWS, Hetzner, or MinIO on a second server), fill this in and restart:\n\
# [backup.copy_to]\n\
# bucket = \"lode-backups\"\n\
# endpoint = \"https://s3.eu-central-003.backblazeb2.com\"   # leave out for AWS\n\
# region = \"eu-central-003\"\n\
# access_key_id = \"...\"\n\
# secret_access_key_file = \"backup-s3-secret.txt\"\n";

/// The admin password from a file, standard input or the terminal; `None` means generate one.
fn read_password(args: &SetupArgs, interactive: bool) -> anyhow::Result<Option<String>> {
    if args.generate_admin_password {
        return Ok(None);
    }
    if let Some(file) = &args.admin_password_file {
        let mut text = String::new();
        if file.as_os_str() == "-" {
            std::io::stdin().read_to_string(&mut text)?;
        } else {
            text = std::fs::read_to_string(file)
                .with_context(|| format!("cannot read {}", file.display()))?;
        }
        return Ok(Some(text.lines().next().unwrap_or("").to_owned()));
    }
    if !interactive {
        // Nobody to ask: a generated password, printed once, is the way in.
        return Ok(None);
    }
    loop {
        let first = rpassword::prompt_password("Admin password (at least 8 characters): ")?;
        if first.chars().count() < lode_server::auth::MIN_PASSWORD_LEN {
            println!("Too short.");
            continue;
        }
        let again = rpassword::prompt_password("Admin password again: ")?;
        if first == again {
            return Ok(Some(first));
        }
        println!("The passwords differ.");
    }
}

pub async fn run(args: SetupArgs) -> anyhow::Result<()> {
    let interactive = !args.yes && std::io::stdin().is_terminal();
    let answers = if interactive {
        let stdin = std::io::stdin();
        let mut lock = stdin.lock();
        answers(&args, Some(Prompter { input: &mut lock }))?
    } else {
        answers(&args, None)?
    };
    let password = read_password(&args, interactive)?;
    if password
        .as_ref()
        .is_some_and(|p| p.chars().count() < lode_server::auth::MIN_PASSWORD_LEN)
    {
        bail!(
            "the admin password must have at least {} characters",
            lode_server::auth::MIN_PASSWORD_LEN
        );
    }

    let data_dir = &answers.data_dir;
    if data_dir.join(lode_server::server::META_DB_FILE).exists()
        || data_dir
            .join(lode_server::server::TEST_POSTGRES_FILE)
            .exists()
    {
        bail!(
            "{} is already set up; edit its {CONFIG_FILE_NAME} instead",
            data_dir.display()
        );
    }
    let text = config_text(&answers)?;
    let config_path = data_dir.join(CONFIG_FILE_NAME);
    // Checked before anything is written.
    ServerConfig::parse(&text, data_dir, &config_path)
        .context("the generated configuration does not parse")?;
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("cannot create {}", data_dir.display()))?;
    match std::fs::read_to_string(&config_path) {
        // A rerun after an interrupted setup with the same answers.
        Ok(existing) if existing == text => {}
        Ok(_) => bail!(
            "{} exists with other settings; remove it or set up another directory",
            config_path.display()
        ),
        Err(_) => std::fs::write(&config_path, &text)
            .with_context(|| format!("cannot write {}", config_path.display()))?,
    }
    let report = lode_server::init(data_dir).await?;
    let config = ServerConfig::load(&config_path)?;
    let generated = match &password {
        Some(p) => {
            lode_server::set_admin_password(&config, p).await?;
            None
        }
        None => Some(lode_server::generate_admin_password(&config).await?),
    };
    if !args.print_admin_token {
        lode_server::revoke_admin_tokens(&config).await?;
    }
    if let Some(dir) = config.backup.schedule().map(|s| s.dir) {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }

    let url = match &answers.domain {
        Some(d) if answers.listen.port() == 443 => format!("https://{d}"),
        Some(d) => format!("https://{d}:{}", answers.listen.port()),
        None => format!("http://{}", answers.listen),
    };
    println!();
    println!("Lode is set up in {}", data_dir.display());
    println!("Configuration: {}", config_path.display());
    println!("Server address for clients: {url}");
    println!();
    if let Some(p) = &generated {
        println!(
            "Admin password (shown only now; it must be changed at the first sign-in, and it \
             stops working after {} hours if unused):",
            lode_server::GENERATED_PASSWORD_VALID.as_secs() / 3600
        );
        println!("{p}");
        println!();
    }
    if args.print_admin_token {
        println!("Admin token (shown only now; you can also sign in as admin with the password):");
        println!("{}", report.admin_token);
        println!();
    }
    println!("Next:");
    if packaged_unit() {
        // Installed from the lode-server package, whose unit runs as `lode`.
        if running_as_root() {
            println!("  sudo chown -R lode:lode {}", quote(data_dir));
        }
        println!("  sudo systemctl enable --now loded");
    } else {
        println!(
            "  loded service install --config {}   (run as administrator or root)",
            quote(&config_path)
        );
    }
    println!("  lode login {url}   (name: admin)");
    if answers.domain.is_some() {
        println!();
        println!(
            "The domain must point at this machine and port 443 must reach {}; the certificate \
             is requested when the server starts.",
            answers.listen
        );
    }
    Ok(())
}

/// True when the lode-server package's systemd unit is installed.
fn packaged_unit() -> bool {
    cfg!(target_os = "linux")
        && [
            "/usr/lib/systemd/system/loded.service",
            "/lib/systemd/system/loded.service",
        ]
        .iter()
        .any(|p| Path::new(p).is_file())
}

/// True when this process runs as root (Unix).
fn running_as_root() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn quote(p: &Path) -> String {
    let s = p.display().to_string();
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn prompts_take_defaults_and_answers() {
        let args = SetupArgs::default();
        let mut input: &[u8] = b"/srv/lode\nlode.example.com\nops@example.com\n\nnone\n";
        let a = answers(&args, Some(Prompter { input: &mut input })).unwrap();
        assert_eq!(a.data_dir, PathBuf::from("/srv/lode"));
        assert_eq!(a.domain.as_deref(), Some("lode.example.com"));
        assert_eq!(a.email.as_deref(), Some("ops@example.com"));
        assert_eq!(a.listen.to_string(), "0.0.0.0:443");
        assert_eq!(a.backup_dir, None);

        // Without a terminal: local use, default address and backups.
        let a = answers(&args, None).unwrap();
        assert_eq!(a.domain, None);
        assert_eq!(a.listen.to_string(), "127.0.0.1:7480");
        assert_eq!(a.backup_dir, Some(PathBuf::from("backups")));
    }

    #[test]
    fn config_text_parses_with_the_answers() {
        let a = Answers {
            data_dir: PathBuf::from("/srv/lode"),
            domain: Some("lode.example.com".into()),
            email: Some("ops@example.com".into()),
            listen: "0.0.0.0:443".parse().unwrap(),
            backup_dir: Some(PathBuf::from("/srv/lode/backups")),
        };
        let text = config_text(&a).unwrap();
        let cfg = ServerConfig::parse(&text, Path::new("/srv/lode"), Path::new("x")).unwrap();
        assert_eq!(cfg.listen.to_string(), "0.0.0.0:443");
        let Some(lode_server::TlsConfig::Acme(acme)) = cfg.tls else {
            panic!("acme expected");
        };
        assert_eq!(acme.domain, "lode.example.com");
        assert_eq!(acme.email.as_deref(), Some("ops@example.com"));
        let s = cfg.backup.schedule().unwrap();
        assert_eq!(s.dir, Path::new("/srv/lode/backups"));
        assert!(
            text.contains("dir = \"backups\""),
            "relative inside the data directory"
        );

        let local = Answers {
            domain: None,
            email: None,
            listen: "127.0.0.1:7480".parse().unwrap(),
            backup_dir: None,
            ..a
        };
        let cfg = ServerConfig::parse(
            &config_text(&local).unwrap(),
            Path::new("/srv/lode"),
            Path::new("x"),
        )
        .unwrap();
        assert!(cfg.tls.is_none() && cfg.backup.schedule().is_none());
    }
}
