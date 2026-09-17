//! Windows Service support: install/remove/status management via the Service Control
//! Manager, and the entry point the SCM actually launches into when the service starts.
//!
//! Autospot is a portable console app by default; this module lets it additionally be
//! registered as an auto-start LocalSystem service, so the watchdog can start before any
//! user logs in.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context as _, Result};
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::config::Config;

const SERVICE_NAME: &str = "autospot";
const SERVICE_DISPLAY_NAME: &str = "Autospot Wi-Fi Watchdog";
const SERVICE_DESCRIPTION: &str =
    "Monitors Wi-Fi and turns on Windows Mobile Hotspot when the connection drops.";

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;

/// Register autospot as an auto-start Windows service running as LocalSystem, using
/// `config_path` as the config it will load every time it starts, then start it.
pub fn install(config_path: &Path) -> Result<()> {
    require_elevation("installing")?;

    let config_path = std::fs::canonicalize(config_path)
        .with_context(|| format!("resolving config path {}", config_path.display()))?;
    let exe_path = std::env::current_exe().context("locating the autospot executable")?;

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)
        .map_err(elevation_friendly_error)?;

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
        .create_service(&service_info, ServiceAccess::START | ServiceAccess::CHANGE_CONFIG)
        .map_err(elevation_friendly_error)?;
    // Best-effort: a missing description shouldn't fail the install.
    let _ = service.set_description(SERVICE_DESCRIPTION);
    service
        .start(&[] as &[&OsStr])
        .context("starting the autospot service")?;

    println!("autospot service installed and started.");
    Ok(())
}

/// Stop the service if running, then delete its Service Control Manager registration.
/// Not being installed at all is treated as success, not an error.
pub fn remove() -> Result<()> {
    require_elevation("removing")?;

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(elevation_friendly_error)?;

    let service = match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    ) {
        Ok(service) => service,
        Err(e) if is_service_missing(&e) => {
            println!("autospot service is not installed; nothing to remove.");
            return Ok(());
        }
        Err(e) => return Err(elevation_friendly_error(e)),
    };

    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop().context("stopping the autospot service")?;
        wait_for_stopped(&service)?;
    }
    service
        .delete()
        .context("deleting the autospot service registration")?;

    println!("autospot service removed.");
    Ok(())
}

fn wait_for_stopped(service: &windows_service::service::Service) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if service.query_status()?.current_state == ServiceState::Stopped {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    anyhow::bail!("timed out waiting for the autospot service to stop")
}

/// Print whether the service is installed and, if so, its current state.
pub fn print_status() -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(service) => {
            let status = service.query_status()?;
            println!(
                "autospot service: installed, state = {:?}",
                status.current_state
            );
        }
        Err(e) if is_service_missing(&e) => {
            println!("autospot service: not installed");
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Is the service currently registered *and* in the `Running` state? A service that
/// isn't registered at all is `Ok(false)`, not an error -- this is called from plain,
/// unelevated `autospot run`, and must behave correctly on a machine where the service
/// was never installed.
pub fn is_running() -> Result<bool> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(service) => Ok(service.query_status()?.current_state == ServiceState::Running),
        Err(e) if is_service_missing(&e) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Base directory for log files in service mode: `%ProgramData%\autospot\logs`, a
/// machine-wide location writable by LocalSystem and easy to find regardless of which
/// account the service runs as. Falls back to the current directory if `ProgramData`
/// isn't set, rather than failing outright.
fn program_data_log_dir() -> PathBuf {
    match std::env::var_os("ProgramData") {
        Some(program_data) => PathBuf::from(program_data).join("autospot").join("logs"),
        None => PathBuf::from("."),
    }
}

fn is_service_missing(e: &windows_service::Error) -> bool {
    matches!(e, windows_service::Error::Winapi(io_err) if io_err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST))
}

/// Fast, friendly pre-check before even touching the Service Control Manager.
fn require_elevation(action: &str) -> Result<()> {
    let elevated = unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool();
    if !elevated {
        anyhow::bail!(
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
        return anyhow::anyhow!(
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
        .map_err(|_| anyhow::anyhow!("run_dispatcher called more than once"))?;
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
    let _guard = crate::init_file_logging(&cfg, &program_data_log_dir(), false)?;

    let stop_flag = Arc::new(AtomicBool::new(false));
    let handler_stop_flag = Arc::clone(&stop_flag);

    let status_handle = service_control_handler::register(SERVICE_NAME, move |control| match control
    {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            handler_stop_flag.store(true, Ordering::Relaxed);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .context("registering the service control handler")?;

    let set_status = |state: ServiceState, controls_accepted: ServiceControlAccept| {
        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted,
            exit_code: ServiceExitCode::NO_ERROR,
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        })
    };

    set_status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    )?;
    tracing::info!("autospot service started");

    let result = crate::monitor::run(&cfg, &config_path, &stop_flag);
    if let Err(ref e) = result {
        tracing::error!("autospot service watchdog loop exited with an error: {e:#}");
    }

    set_status(ServiceState::Stopped, ServiceControlAccept::empty())?;
    result
}
