//! `loded service install|uninstall|start|stop` (T082): a native Windows service, or a
//! systemd unit on Linux.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Subcommand;
use lode_server::ServerConfig;

/// The service's name on both systems.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub const SERVICE_NAME: &str = "loded";

/// `loded service ...`.
#[derive(Subcommand, Debug)]
pub enum ServiceCommand {
    /// Register loded to start at boot with this configuration.
    Install {
        /// Path to loded.toml.
        #[arg(long)]
        config: PathBuf,
        /// Linux: the account the service runs as; it is given the data and backup
        /// directories.
        #[arg(long, default_value = "lode")]
        user: String,
        /// Print what would be written and run, and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop and remove the service. The data directory is kept.
    Uninstall {
        /// Print what would be run, and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Start the service.
    Start,
    /// Stop the service.
    Stop,
}

pub fn run(cmd: ServiceCommand) -> anyhow::Result<()> {
    match cmd {
        ServiceCommand::Install {
            config,
            user,
            dry_run,
        } => {
            // An absolute path, checked, so the service finds the same file.
            let config = std::path::absolute(&config)
                .with_context(|| format!("cannot resolve {}", config.display()))?;
            let cfg = ServerConfig::load(&config)?;
            platform::install(&config, &cfg, &user, dry_run)
        }
        ServiceCommand::Uninstall { dry_run } => platform::uninstall(dry_run),
        ServiceCommand::Start => platform::start(),
        ServiceCommand::Stop => platform::stop(),
    }
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
/// The systemd unit for `loded serve --config <config>`, running as `user`, allowed to write
/// only `writable`, and able to bind ports below 1024 when `low_port`.
pub fn systemd_unit(
    exe: &Path,
    config: &Path,
    user: &str,
    writable: &[PathBuf],
    low_port: bool,
) -> String {
    let paths: Vec<String> = writable.iter().map(|p| systemd_quote(p)).collect();
    let capability = if low_port {
        "AmbientCapabilities=CAP_NET_BIND_SERVICE\nCapabilityBoundingSet=CAP_NET_BIND_SERVICE\n"
    } else {
        ""
    };
    format!(
        "# Written by `loded service install`. Remove with `loded service uninstall`.\n\
         [Unit]\n\
         Description=Lode version control server\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         User={user}\n\
         Group={user}\n\
         ExecStart={} serve --config {}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         # Let in-flight requests finish on stop (loded shuts down gracefully on SIGTERM).\n\
         TimeoutStopSec=60\n\
         Environment=LODED_LOG=info\n\
         LimitNOFILE=65536\n\
         {capability}\
         NoNewPrivileges=yes\n\
         ProtectSystem=strict\n\
         ProtectHome=yes\n\
         PrivateTmp=yes\n\
         PrivateDevices=yes\n\
         ProtectKernelTunables=yes\n\
         ProtectKernelModules=yes\n\
         ProtectControlGroups=yes\n\
         RestrictSUIDSGID=yes\n\
         LockPersonality=yes\n\
         ReadWritePaths={}\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        systemd_quote(exe),
        systemd_quote(config),
        paths.join(" "),
    )
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
/// A path as one systemd word: quoted when it holds spaces.
fn systemd_quote(p: &Path) -> String {
    let s = p.display().to_string();
    if s.contains([' ', '"', '\\']) {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s
    }
}

#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
/// The directories the server writes: the data directory, and the backup directory when
/// backups are on and it lies outside.
pub fn writable_dirs(cfg: &ServerConfig) -> Vec<PathBuf> {
    // `components` drops inner `.` parts, such as the `data_dir = "."` of a default config.
    let data: PathBuf = cfg.data_dir.components().collect();
    let mut dirs = vec![data.clone()];
    if let Some(s) = cfg.backup.schedule() {
        let backups: PathBuf = s.dir.components().collect();
        if !backups.starts_with(&data) {
            dirs.push(backups);
        }
    }
    dirs
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::Path;
    use std::process::Command;

    use anyhow::{bail, Context};
    use lode_server::ServerConfig;

    use super::{systemd_unit, writable_dirs, SERVICE_NAME};

    const UNIT_PATH: &str = "/etc/systemd/system/loded.service";

    fn exec(dry_run: bool, program: &str, args: &[&str]) -> anyhow::Result<()> {
        println!("{program} {}", args.join(" "));
        if dry_run {
            return Ok(());
        }
        let status = Command::new(program)
            .args(args)
            .status()
            .with_context(|| format!("cannot run {program}"))?;
        if !status.success() {
            bail!("`{program} {}` failed ({status})", args.join(" "));
        }
        Ok(())
    }

    pub fn install(
        config: &Path,
        cfg: &ServerConfig,
        user: &str,
        dry_run: bool,
    ) -> anyhow::Result<()> {
        let exe = std::env::current_exe().context("cannot find the loded executable")?;
        let dirs = writable_dirs(cfg);
        let unit = systemd_unit(&exe, config, user, &dirs, cfg.listen.port() < 1024);
        if dry_run {
            println!("# {UNIT_PATH}");
            print!("{unit}");
        } else {
            let known = Command::new("id")
                .arg("-u")
                .arg(user)
                .output()
                .is_ok_and(|o| o.status.success());
            if !known {
                bail!(
                    "there is no user `{user}`; create it with\n  useradd --system --home-dir {} \
                     --shell /usr/sbin/nologin {user}\nor pass --user",
                    cfg.data_dir.display()
                );
            }
        }
        for dir in &dirs {
            if !dry_run {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("cannot create {}", dir.display()))?;
            }
            let owner = format!("{user}:");
            exec(dry_run, "chown", &["-R", &owner, &dir.to_string_lossy()])?;
        }
        if !dry_run {
            std::fs::write(UNIT_PATH, &unit)
                .with_context(|| format!("cannot write {UNIT_PATH} (run as root)"))?;
            println!("Wrote {UNIT_PATH}");
        }
        exec(dry_run, "systemctl", &["daemon-reload"])?;
        exec(dry_run, "systemctl", &["enable", SERVICE_NAME])?;
        println!("Installed. Start it with `loded service start`; logs: journalctl -u loded");
        Ok(())
    }

    pub fn uninstall(dry_run: bool) -> anyhow::Result<()> {
        exec(dry_run, "systemctl", &["disable", "--now", SERVICE_NAME])?;
        println!("rm {UNIT_PATH}");
        if !dry_run {
            std::fs::remove_file(UNIT_PATH)
                .with_context(|| format!("cannot remove {UNIT_PATH} (run as root)"))?;
        }
        exec(dry_run, "systemctl", &["daemon-reload"])?;
        println!("Removed. The data directory is kept.");
        Ok(())
    }

    pub fn start() -> anyhow::Result<()> {
        exec(false, "systemctl", &["start", SERVICE_NAME])
    }

    pub fn stop() -> anyhow::Result<()> {
        exec(false, "systemctl", &["stop", SERVICE_NAME])
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsString;
    use std::path::Path;
    use std::time::Duration;

    use anyhow::Context;
    use lode_server::ServerConfig;
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
        ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType,
        ServiceState, ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use super::SERVICE_NAME;

    fn manager(access: ServiceManagerAccess) -> anyhow::Result<ServiceManager> {
        ServiceManager::local_computer(None::<&str>, access)
            .context("cannot open the service manager (run as administrator)")
    }

    pub fn install(
        config: &Path,
        _cfg: &ServerConfig,
        _user: &str,
        dry_run: bool,
    ) -> anyhow::Result<()> {
        let exe = std::env::current_exe().context("cannot find the loded executable")?;
        let arguments: Vec<OsString> = vec![
            "serve".into(),
            "--config".into(),
            config.as_os_str().to_owned(),
            "--service".into(),
        ];
        println!(
            "Service {SERVICE_NAME}: \"{}\" serve --config \"{}\" --service (automatic start, \
             LocalSystem, restart on failure)",
            exe.display(),
            config.display()
        );
        if dry_run {
            return Ok(());
        }
        let info = ServiceInfo {
            name: SERVICE_NAME.into(),
            display_name: "Lode server".into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe,
            launch_arguments: arguments,
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };
        let service =
            manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?
                .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
                .context("cannot create the service (is it installed already?)")?;
        service.set_description("Lode version control server")?;
        let restart = ServiceAction {
            action_type: ServiceActionType::Restart,
            delay: Duration::from_secs(5),
        };
        service.update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart.clone(), restart.clone(), restart]),
        })?;
        println!(
            "Installed. Start it with `loded service start`; logs: <data dir>\\logs\\loded.log"
        );
        Ok(())
    }

    pub fn uninstall(dry_run: bool) -> anyhow::Result<()> {
        println!("Stop and delete service {SERVICE_NAME}");
        if dry_run {
            return Ok(());
        }
        let service = manager(ServiceManagerAccess::CONNECT)?
            .open_service(
                SERVICE_NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
            )
            .context("cannot open the service (is it installed?)")?;
        if service.query_status()?.current_state != ServiceState::Stopped {
            let _ = service.stop();
        }
        service.delete()?;
        println!("Removed. The data directory is kept.");
        Ok(())
    }

    pub fn start() -> anyhow::Result<()> {
        manager(ServiceManagerAccess::CONNECT)?
            .open_service(SERVICE_NAME, ServiceAccess::START)
            .context("cannot open the service (is it installed?)")?
            .start::<&str>(&[])
            .context("cannot start the service")?;
        println!("Started.");
        Ok(())
    }

    pub fn stop() -> anyhow::Result<()> {
        manager(ServiceManagerAccess::CONNECT)?
            .open_service(SERVICE_NAME, ServiceAccess::STOP)
            .context("cannot open the service (is it installed?)")?
            .stop()
            .context("cannot stop the service")?;
        println!("Stopping.");
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use std::path::Path;

    use anyhow::bail;
    use lode_server::ServerConfig;

    const UNSUPPORTED: &str =
        "services are supported on Windows and Linux; run `loded serve --config ...` under launchd";

    pub fn install(_: &Path, _: &ServerConfig, _: &str, _: bool) -> anyhow::Result<()> {
        bail!(UNSUPPORTED)
    }

    pub fn uninstall(_: bool) -> anyhow::Result<()> {
        bail!(UNSUPPORTED)
    }

    pub fn start() -> anyhow::Result<()> {
        bail!(UNSUPPORTED)
    }

    pub fn stop() -> anyhow::Result<()> {
        bail!(UNSUPPORTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_runs_serve_with_the_config_and_limits_writes() {
        let unit = systemd_unit(
            Path::new("/usr/bin/loded"),
            Path::new("/var/lib/lode/loded.toml"),
            "lode",
            &[
                PathBuf::from("/var/lib/lode"),
                PathBuf::from("/mnt/backup dir"),
            ],
            true,
        );
        assert!(
            unit.contains("\nExecStart=/usr/bin/loded serve --config /var/lib/lode/loded.toml\n")
        );
        assert!(unit.contains("\nUser=lode\nGroup=lode\n"));
        assert!(unit.contains("\nReadWritePaths=/var/lib/lode \"/mnt/backup dir\"\n"));
        assert!(unit.contains("\nAmbientCapabilities=CAP_NET_BIND_SERVICE\n"));
        assert!(unit.contains("\nProtectSystem=strict\n"));
        assert!(unit.contains("\nWantedBy=multi-user.target\n"));
        let plain = systemd_unit(
            Path::new("/opt/lode/loded"),
            Path::new("/etc/lode.toml"),
            "svc",
            &[PathBuf::from("/srv")],
            false,
        );
        assert!(!plain.contains("CAP_NET_BIND_SERVICE"));
    }

    #[test]
    fn backups_outside_the_data_directory_are_writable() {
        let mut cfg = ServerConfig::with_data_dir("/var/lib/lode");
        assert_eq!(writable_dirs(&cfg), [PathBuf::from("/var/lib/lode")]);
        cfg.backup.every = Some(std::time::Duration::from_secs(3600));
        cfg.backup.dir = PathBuf::from("/var/lib/lode/backups");
        assert_eq!(writable_dirs(&cfg).len(), 1);
        cfg.backup.dir = PathBuf::from("/mnt/backups");
        assert_eq!(writable_dirs(&cfg).len(), 2);
    }
}
