use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

mod batch;
mod config;
mod pdf;
mod routes;
mod state;

#[derive(Parser)]
#[command(about = "PaperStream NX Manager compatible server")]
struct Cli {
    #[arg(short, long, help = "Path to config YAML")]
    config: PathBuf,

    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    #[arg(long, default_value = "10447")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("nx_boss=info".parse()?))
        .init();

    let cli = Cli::parse();
    let config = config::Config::load(&cli.config)?;
    info!("Loaded {} job(s)", config.jobs.len());

    let state = state::AppState::new(config);
    let app = routes::router(state);

    let addr = format!("{}:{}", cli.host, cli.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
