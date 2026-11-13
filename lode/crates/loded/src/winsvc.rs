//! Running under the Windows service manager (`loded serve --service`, T082).
//!
//! The service manager starts `loded serve --config <file> --service`; this hands control
//! to the service dispatcher, which calls [`service_main`] on its own thread. Stop and
//! shutdown requests end the server gracefully. A service has no console, so the log goes
//! to `<data_dir>\logs\loded.log`.
//!
//! The dispatcher entry point comes from `windows_service::define_windows_service!`, whose
//! expansion holds the crate's one `unsafe` call (reading the argument vector); this crate
//! itself writes no unsafe code.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use lode_server::ServerConfig;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::service::SERVICE_NAME;

/// The configuration path, passed from `run` to the dispatcher's thread.
static CONFIG: OnceLock<PathBuf> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

/// Hands the process to the service dispatcher; returns when the service has stopped.
pub fn run(config: PathBuf) -> anyhow::Result<()> {
    let _ = CONFIG.set(config);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .context("cannot start as a service (only the service manager starts it this way)")
}

fn service_main(_arguments: Vec<OsString>) {
    // Errors have nowhere else to go: the service manager records the exit code.
    let _ = run_service();
}

fn status(state: ServiceState, accept: ServiceControlAccept, exit: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(exit),
        checkpoint: 0,
        wait_hint: Duration::from_secs(if state == ServiceState::Running {
            0
        } else {
            60
        }),
        process_id: None,
    }
}

fn run_service() -> anyhow::Result<()> {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let stop_tx = Mutex::new(Some(stop_tx));
    let handle = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            if let Some(tx) = stop_tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = tx.send(());
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    handle.set_service_status(status(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        0,
    ))?;

    let result = serve_logged(stop_rx, &handle);
    let exit = if result.is_ok() { 0 } else { 1 };
    handle.set_service_status(status(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit,
    ))?;
    result
}

fn serve_logged(
    stop_rx: tokio::sync::oneshot::Receiver<()>,
    handle: &service_control_handler::ServiceStatusHandle,
) -> anyhow::Result<()> {
    let config = CONFIG.get().context("no configuration given")?;
    let cfg = ServerConfig::load(config)?;
    let logs = cfg.data_dir.join("logs");
    std::fs::create_dir_all(&logs).with_context(|| format!("cannot create {}", logs.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("loded.log"))
        .context("cannot open the log file")?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LODED_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    handle.set_service_status(status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        0,
    ))?;
    let result = runtime.block_on(crate::serve(cfg, async move {
        let _ = stop_rx.await;
        tracing::info!("service stop requested; finishing in-flight requests");
    }));
    if let Err(e) = &result {
        tracing::error!(error = %format!("{e:#}"), "the server stopped with an error");
    }
    result
}
