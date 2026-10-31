use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use tracing::{info, warn};

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = sigterm => {},
    }
    info!(
        event = "service.shutdown_started",
        "shutdown signal received, draining connections…"
    );
}

async fn wait_for_shutdown(mut receiver: tokio::sync::watch::Receiver<bool>) {
    if !*receiver.borrow() {
        let _ = receiver.changed().await;
    }
}

async fn healthcheck(port: u16) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let check = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await?;
        let mut response = [0; 13];
        stream.read_exact(&mut response).await?;
        ensure!(
            health_response_ok(&response),
            "scanner health endpoint is not HTTP 200"
        );
        Ok::<(), anyhow::Error>(())
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), check).await??;
    Ok(())
}

fn health_response_ok(response: &[u8]) -> bool {
    response.starts_with(b"HTTP/1.1 200 ") || response.starts_with(b"HTTP/1.0 200 ")
}

mod admin_auth;
mod atomic_file;
mod batch;
mod build_info;
mod config;
mod event_log;
mod logging;
mod paperless;
mod pdf;
mod retention;
mod routes;
mod scanner_auth;
mod state;
mod translations;

#[derive(Parser)]
#[command(about = "PaperStream NX Manager compatible server")]
struct Cli {
    #[arg(
        short,
        long,
        required_unless_present = "healthcheck",
        help = "Path to config YAML"
    )]
    config: Option<PathBuf>,

    #[arg(long, help = "Check the local scanner HTTP health endpoint")]
    healthcheck: bool,

    #[arg(
        long,
        help = "Convert legacy scanner environment credentials into a private Argon2id config hash, then exit"
    )]
    migrate_scanner_auth: bool,

    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    #[arg(long, default_value = "10447")]
    port: u16,

    #[arg(long, default_value = "127.0.0.1")]
    admin_host: String,

    #[arg(long, default_value = "10448")]
    admin_port: u16,

    #[arg(long, help = "Additional Host header allowed on the admin listener")]
    admin_allowed_host: Vec<String>,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let event_log = logging::init();
    match run(event_log).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(
                event = "service.fatal",
                error_class = logging::error_class(&error),
                "service failed"
            );
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(event_log: event_log::EventLog) -> Result<()> {
    let cli = Cli::parse();
    if cli.healthcheck {
        return healthcheck(cli.port).await;
    }
    let config_arg = cli.config.as_ref().expect("clap requires --config");
    let config = config::Config::load(config_arg)?;
    let config_path = config_arg
        .canonicalize()
        .unwrap_or_else(|_| config_arg.clone());
    let config_dir = config_path.parent().unwrap_or(std::path::Path::new("."));
    let pepper_path = std::env::var_os("NX_BOSS_AUTH_PEPPER_FILE")
        .context("NX_BOSS_AUTH_PEPPER_FILE is required")?;
    let pepper = admin_auth::read_pepper(std::path::Path::new(&pepper_path))?;
    if cli.migrate_scanner_auth {
        ensure!(
            config.scanner_auth.is_none(),
            "scanner_auth already exists in config"
        );
        let legacy = scanner_auth::ScannerAuthMode::from_env()?;
        ensure!(
            legacy.requires_login(),
            "legacy scanner userpass mode is required"
        );
        let hashed = legacy.export_hashed_config(&pepper)?;
        config::Config::save_with_next(
            &config.jobs,
            &config.retention,
            &config.lang,
            config.next_job_id,
            config.paperless.as_ref(),
            Some(&hashed),
            &config_path,
        )?;
        info!(
            event = "scanner.auth_migrated",
            "legacy scanner credentials converted to a hash"
        );
        return Ok(());
    }
    let admin_domain =
        std::env::var("NX_BOSS_ADMIN_DOMAIN").unwrap_or_else(|_| "localhost".to_owned());
    let admin_port = std::env::var("NX_BOSS_ADMIN_HTTPS_PORT").unwrap_or_else(|_| "443".to_owned());
    let admin_origin = if admin_port == "443" {
        format!("https://{admin_domain}")
    } else {
        format!("https://{admin_domain}:{admin_port}")
    };
    let admin_auth = admin_auth::AdminAuth::open_with_origin(config_dir, &pepper, &admin_origin)?;
    let scanner_auth_mode = if let Some(ref scanner_config) = config.scanner_auth {
        scanner_auth::ScannerAuthMode::from_hashed_config(scanner_config, &pepper)?
    } else {
        scanner_auth::ScannerAuthMode::from_env()?
    };
    let scanner_auth_experiment = match std::env::var("NX_BOSS_SCANNER_AUTH_EXPERIMENT") {
        Err(std::env::VarError::NotPresent) => None,
        Ok(value) if value.is_empty() => None,
        Ok(value) if value == "job" => {
            ensure!(
                scanner_auth_mode.requires_login(),
                "scanner diagnostic requires userpass protection"
            );
            Some("job")
        }
        _ => anyhow::bail!("unsupported scanner diagnostic mode"),
    };
    let paperless_client = config
        .paperless
        .clone()
        .map(paperless::PaperlessClient::from_env)
        .transpose()?;
    if config.jobs.is_empty() {
        warn!(
            event = "config.no_jobs",
            "scanner will see an empty job list"
        );
    }
    info!(
        event = "service.started",
        service = "nx-boss-rs",
        version = env!("CARGO_PKG_VERSION"),
        job_count = config.jobs.len(),
        "service started"
    );

    if event_log
        .attach(config_dir.join("system-events.json"))
        .is_err()
    {
        warn!(
            event = "event_log.unavailable",
            "event journal is not persistent"
        );
    }
    let probe = config_dir.join(format!(".nx-boss-write-probe-{}", std::process::id()));
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|_| std::fs::remove_file(&probe))
        .is_err()
    {
        warn!(
            event = "config.not_writable",
            "job changes will not survive a restart"
        );
    }
    let mut state = state::AppState::new(config)
        .with_config_path(config_path)
        .with_admin_auth(admin_auth)
        .with_event_log(event_log);
    if scanner_auth_mode.requires_login() {
        info!(
            event = "scanner.userpass_enabled",
            "scanner username/password authentication enabled"
        );
    }
    state = state.with_scanner_auth_mode(scanner_auth_mode);
    if let Some(wire_type) = scanner_auth_experiment {
        state = state.with_scanner_auth_experiment(wire_type);
        warn!(
            event = "scanner.auth_experiment_enabled",
            "fail-closed scanner authentication diagnostic enabled"
        );
    }
    if let Some(path) = std::env::var_os("NX_BOSS_OUTPUT_PATH") {
        state = state.with_default_output_path(PathBuf::from(path));
    }
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    if let Some(client) = paperless_client {
        state = state.with_paperless_client(client);
    }
    let paperless_worker = Some(tokio::spawn(paperless::run_forever(
        state.jobs.clone(),
        state.paperless_client.clone(),
        shutdown_rx.clone(),
    )));
    let scanner_app = routes::scanner_router(state.clone());
    let mut allowed_admin_hosts = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "[::1]".to_string(),
    ];
    if cli.admin_host != "0.0.0.0" && cli.admin_host != "::" {
        allowed_admin_hosts.push(cli.admin_host.to_ascii_lowercase());
    }
    allowed_admin_hosts.extend(
        cli.admin_allowed_host
            .iter()
            .map(|host| host.to_ascii_lowercase()),
    );
    let admin_app = routes::admin_router_with_hosts(state.clone(), allowed_admin_hosts);

    let retention_worker = tokio::spawn(retention::run_forever(
        state.jobs.clone(),
        state.retention.clone(),
        state.batches.clone(),
        shutdown_rx.clone(),
    ));

    let addr = format!("{}:{}", cli.host, cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let admin_addr = format!("{}:{}", cli.admin_host, cli.admin_port);
    let admin_listener = tokio::net::TcpListener::bind(&admin_addr).await?;
    info!(
        event = "listener.started",
        listener = "scanner",
        "scanner listener started"
    );
    info!(
        event = "listener.started",
        listener = "admin",
        "admin listener started"
    );
    let signal_tx = shutdown_tx.clone();
    let signal_worker = tokio::spawn(async move {
        shutdown_signal().await;
        let _ = signal_tx.send(true);
    });
    let servers = tokio::try_join!(
        axum::serve(listener, scanner_app)
            .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone())),
        axum::serve(admin_listener, admin_app)
            .with_graceful_shutdown(wait_for_shutdown(shutdown_rx)),
    );
    let _ = shutdown_tx.send(true);
    let paperless_result = if let Some(worker) = paperless_worker {
        worker.await
    } else {
        Ok(())
    };
    let retention_result = retention_worker.await;
    signal_worker.abort();
    servers?;
    paperless_result?;
    retention_result?;
    info!(event = "service.shutdown_complete", "shutdown complete");
    Ok(())
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn healthcheck_accepts_http_success() {
        assert!(health_response_ok(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
        ));
    }

    #[test]
    fn healthcheck_rejects_http_error_and_other_protocols() {
        assert!(!health_response_ok(
            b"HTTP/1.1 503 Service Unavailable\r\n\r\n"
        ));
        assert!(!health_response_ok(b"HTTP/1.1 302 Found\r\n\r\n"));
        assert!(!health_response_ok(b"SSH-2.0-server\r\n"));
        assert!(!health_response_ok(b""));
    }

    #[test]
    fn healthcheck_can_run_without_config() {
        let cli = Cli::try_parse_from(["nx-boss-rs", "--healthcheck"]).unwrap();
        assert!(cli.healthcheck);
        assert!(cli.config.is_none());
    }

    #[test]
    fn scanner_listener_defaults_to_loopback() {
        let cli = Cli::try_parse_from(["nx-boss-rs", "--config", "test.yaml"]).unwrap();
        assert_eq!(cli.host, "127.0.0.1");
    }
}
