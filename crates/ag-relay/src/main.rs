//! Relay binary: listens until interrupted or terminated.

use std::error::Error;
use std::io::Write as _;
use std::net::SocketAddr;
use std::process::ExitCode;

use ag_relay::{RelayConfig, RelayLimits};
use clap::Parser;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};

/// Command-line options.
#[derive(Parser)]
#[command(name = "ag-relay", version, about = "Relay for Agentty remote control")]
struct Cli {
    /// Header a trusted reverse proxy sets to the client address, such as
    /// `X-Real-IP`. Leave it unset when clients connect directly.
    #[arg(long)]
    client_address_header: Option<String>,
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "ag-relay: {error}");

            ExitCode::FAILURE
        }
    }
}

/// Serves until the process is told to stop, then closes every connection.
async fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    let relay = ag_relay::start(RelayConfig {
        bind_addr: cli.listen,
        client_address_header: cli.client_address_header,
        limits: RelayLimits::default(),
    })
    .await?;
    writeln!(std::io::stdout(), "listening on {}", relay.local_addr())?;
    wait_for_stop().await?;
    relay.shutdown().await;

    Ok(())
}

/// Resolves on an interrupt or a termination signal.
#[cfg(unix)]
async fn wait_for_stop() -> std::io::Result<()> {
    let mut terminate = signal(SignalKind::terminate())?;

    tokio::select! {
        _ = terminate.recv() => Ok(()),
        interrupted = tokio::signal::ctrl_c() => interrupted,
    }
}

/// Resolves on an interrupt.
#[cfg(not(unix))]
async fn wait_for_stop() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}
