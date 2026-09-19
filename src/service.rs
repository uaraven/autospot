//! Windows Service support: install/remove/status management via the Service Control
//! Manager, and the entry point the SCM actually launches into when the service starts.
//!
//! Autospot is a portable console app by default; this module lets it additionally be
//! registered as an auto-start LocalSystem service, so the watchdog can start before any
//! user logs in.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use tokio::sync::Notify;
use windows_service::service::{
    Service, ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl,
    ServiceExitCode, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::config::Config;
use crate::logging;

const SERVICE_NAME: &str = "autospot";
const SERVICE_DISPLAY_NAME: &str = "Autospot Wi-Fi Watchdog";
const SERVICE_DESCRIPTION: &str =
    "Monitors Wi-Fi and turns on Windows Mobile Hotspot when the connection drops.";

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;

/// How long to wait for a stop to take effect before giving up on it.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long the Service Control Manager should allow our state changes to take.
const STATUS_WAIT_HINT: Duration = Duration::from_secs(10);

/// Register autospot as an auto-start Windows service running as LocalSystem, using
/// `config_path` as the config it will load every time it starts, then start it.
pub fn install(config_path: &Path) -> Result<()> {
    require_elevation("installing")?;

    let config_path = std::fs::canonicalize(config_path)
        .with_context(|| format!("resolving config path {}", config_path.display()))?;
    let exe_path = std::env::current_exe().context("locating the autospot executable")?;

    let manager = connect(ServiceManagerAccess::CREATE_SERVICE)?;
    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe_path,
        launch_arguments: vec![
            OsString::from("service"),
            OsString::from("run"),
            OsString::from("--config"),
            OsString::from(config_path.as_os_str()),
        ],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };

    let service = manager
        .create_service(
            &service_info,
            ServiceAccess::START | ServiceAccess::CHANGE_CONFIG,
        )
        .map_err(elevation_friendly_error)?;

    // Best-effort: a missing description shouldn't fail the install.
    let _ = service.set_description(SERVICE_DESCRIPTION);
    start(&service)?;

    println!("autospot service installed and started.");
    Ok(())
}

/// Stop the service if running, then delete its Service Control Manager registration.
/// Not being installed at all is treated as success, not an error.
pub fn remove() -> Result<()> {
    require_elevation("removing")?;

    let access = ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS;
    let Some(service) = open(access)? else {
        println!("autospot service is not installed; nothing to remove.");
        return Ok(());
    };

    stop(&service)?;
    service
        .delete()
        .context("deleting the autospot service registration")?;

    println!("autospot service removed.");
    Ok(())
}

/// Stop the service, then start it again -- e.g. after editing autospot.toml, since the
/// service only reads the config file once, when it starts. Leaves the existing Service
/// Control Manager registration (and its `--config` path) untouched; use
/// `autospot service install` if the service isn't installed at all yet.
pub fn restart() -> Result<()> {
    require_elevation("restarting")?;

    let access = ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::QUERY_STATUS;
    let Some(service) = open(access)? else {
        bail!("autospot service is not installed; use `autospot service install` first");
    };

    stop(&service)?;
    start(&service)?;

    println!("autospot service restarted.");
    Ok(())
}

/// Print whether the service is installed and, if so, its current state.
pub fn print_status() -> Result<()> {
    match open(ServiceAccess::QUERY_STATUS)? {
        Some(service) => println!(
            "autospot service: installed, state = {:?}",
            service.query_status()?.current_state
        ),
        None => println!("autospot service: not installed"),
    }
    Ok(())
}

/// Is the service currently registered *and* in the `Running` state? A service that
/// isn't registered at all is `Ok(false)`, not an error -- this is called from plain,
/// unelevated `autospot run`, and must behave correctly on a machine where the service
/// was never installed.
pub fn is_running() -> Result<bool> {
    match open(ServiceAccess::QUERY_STATUS)? {
        Some(service) => Ok(service.query_status()?.current_state == ServiceState::Running),
        None => Ok(false),
    }
}

fn connect(access: ServiceManagerAccess) -> Result<ServiceManager> {
    ServiceManager::local_computer(None::<&str>, access).map_err(elevation_friendly_error)
}

/// Open the autospot service with `access`. `None` means it isn't installed, which every
/// caller here treats as a normal state rather than a failure.
fn open(access: ServiceAccess) -> Result<Option<Service>> {
    let manager = connect(ServiceManagerAccess::CONNECT)?;
    match manager.open_service(SERVICE_NAME, access) {
        Ok(service) => Ok(Some(service)),
        Err(e) if is_service_missing(&e) => Ok(None),
        Err(e) => Err(elevation_friendly_error(e)),
    }
}

fn start(service: &Service) -> Result<()> {
    service
        .start(&[] as &[&OsStr])
        .context("starting the autospot service")
}

/// Stop the service and wait for it to actually be stopped. Already being stopped is a
/// no-op.
fn stop(service: &Service) -> Result<()> {
    if service.query_status()?.current_state == ServiceState::Stopped {
        return Ok(());
    }
    service.stop().context("stopping the autospot service")?;

    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if service.query_status()?.current_state == ServiceState::Stopped {
            return Ok(());
        }
        std::thread::sleep(STOP_POLL_INTERVAL);
    }

    bail!("timed out waiting for the autospot service to stop")
}

fn is_service_missing(e: &windows_service::Error) -> bool {
    matches!(e, windows_service::Error::Winapi(io_err) if io_err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST))
}

/// Fast, friendly pre-check before even touching the Service Control Manager.
fn require_elevation(action: &str) -> Result<()> {
    let elevated = unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool();
    if !elevated {
        bail!(
            "{action} the autospot service requires Administrator privileges; re-run this \
             command from an elevated (\"Run as administrator\") terminal."
        );
    }
    Ok(())
}

/// Fallback in case the fast pre-check passes but the SCM call itself is still denied
/// (e.g. a locked-down SCM ACL).
fn elevation_friendly_error(e: windows_service::Error) -> anyhow::Error {
    if let windows_service::Error::Winapi(ref io_err) = e
        && io_err.raw_os_error() == Some(ERROR_ACCESS_DENIED)
    {
        return anyhow!(
            "access denied -- installing or removing the autospot service requires \
             Administrator privileges. Re-run this command from an elevated terminal."
        );
    }
    anyhow::Error::new(e).context("Windows service manager error")
}

// --- SCM entry point -------------------------------------------------------------

define_windows_service!(ffi_service_main, service_main_entry);

/// Stashed here so `service_main_entry` -- whose signature is fixed by the SCM's FFI
/// contract and can't take arbitrary captured state -- can retrieve the config path
/// that `run_dispatcher` resolved. Set exactly once, before the dispatcher starts.
static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Entry point called from `main.rs` for the hidden `service run` variant. Blocks until
/// the service is stopped, then returns.
pub fn run_dispatcher(config_path: PathBuf) -> Result<()> {
    CONFIG_PATH
        .set(config_path)
        .map_err(|_| anyhow!("run_dispatcher called more than once"))?;
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .context("starting the service control dispatcher (is this really running as a service?)")
}

fn service_main_entry(_arguments: Vec<OsString>) {
    // No console in service mode, and logging may not be initialized yet if config
    // loading itself failed -- there's nowhere better to report this than the SCM's own
    // event log entry for a failed service start.
    let _ = service_main();
}

fn service_main() -> Result<()> {
    let config_path = CONFIG_PATH
        .get()
        .expect("run_dispatcher sets this before the dispatcher starts")
        .clone();
    let cfg = Config::load(&config_path)?;
    let _guard = logging::init(&cfg, logging::Mode::Service)?;

    let stop = Arc::new(Notify::new());
    let handler_stop = Arc::clone(&stop);

    let status_handle =
        service_control_handler::register(SERVICE_NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                handler_stop.notify_one();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })
        .context("registering the service control handler")?;

    let report = |state: ServiceState, controls_accepted: ServiceControlAccept| {
        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted,
            exit_code: ServiceExitCode::NO_ERROR,
            checkpoint: 0,
            wait_hint: STATUS_WAIT_HINT,
            process_id: None,
        })
    };

    report(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    )?;
    tracing::info!("autospot service started");

    // This runs on the SCM dispatcher's own thread, reached through a fixed FFI signature
    // that can't itself be async, so it drives its own runtime rather than sharing one
    // with the rest of the process.
    let result = crate::block_on(crate::watchdog::run(&cfg, &config_path, &stop));
    if let Err(ref e) = result {
        tracing::error!("autospot service watchdog loop exited with an error: {e:#}");
    }

    report(ServiceState::Stopped, ServiceControlAccept::empty())?;
    result
}
