use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use serangkai::{app::App, persistence::Database, terminal::TerminalGuard};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(
    name = "serangkai",
    version,
    about = "A terminal-native AI coding workspace"
)]
struct Cli {
    /// Launch Serangkai for a specific project directory.
    #[arg(value_name = "PROJECT", default_value = ".")]
    project: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let project = std::fs::canonicalize(cli.project)?;

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .init();

    let database = Database::open(&project)?;
    let mut app = App::load(project, database)?;
    app.discover_opencode().await;
    let _terminal = TerminalGuard::enter()?;
    app.run().await
}
