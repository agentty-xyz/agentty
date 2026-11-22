//! Relay binary: listens until interrupted or terminated.

use std::error::Error;
use std::io::Write as _;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use ag_relay::{InstanceAllowList, RelayConfig, RelayLimits};
use clap::Parser;
#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};

/// Characters in an instance identifier: a hexadecimal SHA-256 digest.
const INSTANCE_ID_CHARS: usize = 64;

/// Command-line options.
#[derive(Parser)]
#[command(name = "ag-relay", version, about = "Relay for Agentty remote control")]
struct Cli {
    /// Instance identifier that may register: the 64-character hexadecimal
    /// SHA-256 digest of the instance key. Repeat it for several instances;
    /// without it, every instance may register.
    #[arg(long = "allow-instance", value_name = "INSTANCE_ID", value_parser = parse_instance_id)]
    allowed_instances: Vec<String>,
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
///
/// Stop signals are listened for before the startup line is written, so a
/// supervisor that signals right after reading it still gets a clean stop.
async fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    let mut stop_signals = StopSignals::listen()?;
    let config = RelayConfig {
        bind_addr: cli.listen,
        client_address_header: cli.client_address_header,
        limits: RelayLimits::default(),
    };
    let relay = if cli.allowed_instances.is_empty() {
        ag_relay::start(config).await?
    } else {
        let allow_list = InstanceAllowList::new(cli.allowed_instances);

        ag_relay::start_with_admission(config, Arc::new(allow_list)).await?
    };
    writeln!(std::io::stdout(), "listening on {}", relay.local_addr())?;
    stop_signals.wait().await;
    relay.shutdown().await;

    Ok(())
}

/// Accepts an instance identifier, so a mistyped one fails at startup
/// instead of refusing the instance later.
fn parse_instance_id(text: &str) -> Result<String, String> {
    let is_instance_id =
        text.len() == INSTANCE_ID_CHARS && text.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !is_instance_id {
        return Err(format!(
            "expected a {INSTANCE_ID_CHARS}-character hexadecimal instance identifier"
        ));
    }

    Ok(text.to_string())
}

/// Signals that stop the relay: interrupt and termination.
#[cfg(unix)]
struct StopSignals {
    interrupt: Signal,
    terminate: Signal,
}

#[cfg(unix)]
impl StopSignals {
    /// Starts listening for both signals.
    fn listen() -> std::io::Result<Self> {
        let interrupt = signal(SignalKind::interrupt())?;
        let terminate = signal(SignalKind::terminate())?;

        Ok(Self {
            interrupt,
            terminate,
        })
    }

    /// Resolves when either signal arrives.
    async fn wait(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
    }
}

/// Signal that stops the relay: interrupt.
#[cfg(not(unix))]
struct StopSignals;

#[cfg(not(unix))]
impl StopSignals {
    /// The interrupt handler is installed when [`Self::wait`] first runs.
    fn listen() -> std::io::Result<Self> {
        Ok(Self)
    }

    /// Resolves when the interrupt arrives.
    async fn wait(&mut self) {
        let _ = tokio::signal::ctrl_c().await;
    }
}
