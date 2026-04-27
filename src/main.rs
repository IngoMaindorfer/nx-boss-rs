use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

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
    info!("shutdown signal received, draining connections…");
}

mod batch;
mod build_info;
mod config;
mod pdf;
mod retention;
mod routes;
mod state;
mod translations;

#[derive(Parser)]
#[command(about = "PaperStream NX Manager compatible server")]
struct Cli {
    #[arg(short, long, help = "Path to config YAML")]
    config: PathBuf,

    #[arg(long, default_value = "0.0.0.0")]
    host: String,

    #[arg(long, default_value = "10447")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("nx_boss_rs=info".parse()?))
        .init();

    let cli = Cli::parse();
    let config = config::Config::load(&cli.config)?;
    for job in &config.jobs {
        info!(
            job = job.name(),
            output_path = %job.output_path.display(),
            "path OK"
        );
        if let Some(ref cp) = job.consume_path {
            info!(job = job.name(), consume_path = %cp.display(), "consume path OK");
        }
    }
    if config.jobs.is_empty() {
        warn!("no jobs configured — scanner will see an empty job list");
    }
    info!("Loaded {} job(s)", config.jobs.len());

    let config_path = cli.config.canonicalize().unwrap_or(cli.config.clone());
    let state = state::AppState::new(config).with_config_path(config_path);
    let app = routes::router(state.clone());

    tokio::spawn(retention::run_forever(
        state.jobs.clone(),
        state.retention.clone(),
        state.batches.clone(),
    ));

    let addr = format!("{}:{}", cli.host, cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
