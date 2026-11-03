//! The Windows service. `acceptrequest-service run` runs it in the console for debugging.

#[cfg(windows)]
mod service {
    use acceptrequest_client::agent::Agent;
    use acceptrequest_client::config::Config;
    use acceptrequest_windows::win::platform::Windows;
    use acceptrequest_windows::win::{DATA_DIR, pipe};
    use anyhow::{Context, Result};
    use std::ffi::OsString;
    use std::path::Path;
    use std::time::Duration;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    pub const NAME: &str = "acceptrequest";

    /// Read `config.env` (KEY=VALUE lines) from the data directory into the environment, so the
    /// shared `Config::from_env` works the same as on Linux.
    fn load_config_file() -> Result<()> {
        let path = Path::new(DATA_DIR).join("config.env");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some((k, v)) = line.split_once('=')
                && k.trim().starts_with("ACCEPTREQUEST_")
            {
                // Single-threaded here: this runs before the runtime starts.
                unsafe { std::env::set_var(k.trim(), v.trim()) };
            }
        }
        Ok(())
    }

    fn init_logging() {
        let path = Path::new(DATA_DIR).join("service.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
            let _ = std::fs::rename(&path, path.with_extension("log.old"));
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path);
        let builder = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()));
        match file {
            Ok(f) => builder.with_writer(std::sync::Mutex::new(f)).init(),
            Err(_) => builder.init(),
        }
    }

    pub fn run_agent(stop: tokio::sync::oneshot::Receiver<()>) -> Result<()> {
        std::fs::create_dir_all(DATA_DIR)?;
        load_config_file()?;
        let mut config = Config::from_env()?;
        if std::env::var_os("ACCEPTREQUEST_STATE_DIR").is_none() {
            config.state_dir = DATA_DIR.into();
        }
        let pipe_name = config.socket.display().to_string();
        let platform = Windows::new(config.users.clone());
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async move {
            let agent = Agent::new(config, platform)?;
            tokio::spawn(pipe::serve(agent.clone(), pipe_name));
            tokio::select! {
                _ = agent.run() => {}
                _ = stop => tracing::info!("stopping"),
            }
            Ok(())
        })
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_service() {
            tracing::error!("service failed: {e:#}");
        }
    }

    fn run_service() -> Result<()> {
        init_logging();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let mut stop_tx = Some(stop_tx);
        let handle = service_control_handler::register(NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(tx) = stop_tx.take() {
                    let _ = tx.send(());
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;
        let status = |state, accept| ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        };
        handle.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN))?;
        let result = run_agent(stop_rx);
        if let Err(e) = &result {
            tracing::error!("{e:#}");
        }
        handle.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))?;
        result
    }

    pub fn main() -> Result<()> {
        if std::env::args().nth(1).as_deref() == Some("run") {
            tracing_subscriber::fmt().init();
            let (_tx, rx) = tokio::sync::oneshot::channel();
            return run_agent(rx);
        }
        service_dispatcher::start(NAME, ffi_service_main).context("starting the service dispatcher")?;
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    service::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("acceptrequest-service only runs on Windows.");
    std::process::exit(1);
}
