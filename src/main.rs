use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::Parser;
use mdfolio::{
    catalog::Catalog,
    server::{AppState, app},
    watcher,
};
use tokio::net::TcpListener;

#[derive(Debug, Parser)]
#[command(
    name = "mdfolio",
    version,
    about = "Read the Markdown already in your repositories"
)]
struct Cli {
    /// Directory to gather. Defaults to the current directory.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Keep the browser closed and print the local URL.
    #[arg(long)]
    no_open: bool,

    /// Loopback port. Zero chooses an available port.
    #[arg(long, default_value_t = 0)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .without_time()
        .with_target(false)
        .with_max_level(tracing::Level::WARN)
        .init();

    let cli = Cli::parse();
    let catalog = Catalog::scan(&cli.path)?;
    print_catalog_summary(&catalog);

    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, cli.port)))
        .await
        .with_context(|| format!("could not bind 127.0.0.1:{}", cli.port))?;
    let address = listener.local_addr()?;
    let state = AppState::new(catalog);
    let watch_runtime = match watcher::start(state.clone()).await {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            eprintln!("warning: live reload unavailable: {error}");
            None
        }
    };

    let url = format!("http://{address}/_mdfolio/");
    println!("{url}");

    if !cli.no_open
        && let Err(error) = webbrowser::open(&url)
    {
        eprintln!("warning: could not open the browser: {error}");
    }

    let shutdown_state = state.clone();
    axum::serve(listener, app(state))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_state.begin_shutdown();
        })
        .await
        .context("local server failed")?;

    if let Some(runtime) = watch_runtime {
        runtime.shutdown().await;
    }
    Ok(())
}

fn print_catalog_summary(catalog: &Catalog) {
    println!("mdfolio  {}", catalog.root().display());
    println!(
        "{} repositories · {} pages",
        catalog.repositories().len(),
        catalog.documents().len()
    );
    for diagnostic in catalog.diagnostics() {
        if let Some(path) = &diagnostic.path {
            eprintln!("warning: {}: {}", path.display(), diagnostic.message);
        } else {
            eprintln!("warning: {}", diagnostic.message);
        }
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!("could not listen for Ctrl-C: {error}");
    }
}
