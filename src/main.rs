use clap::Parser;
use opscodex::cli::{Cli, execute};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "opscodex=info".into()),
        )
        .with_target(false)
        .init();
    execute(Cli::parse()).await?;
    Ok(())
}
