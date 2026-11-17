//! Marketing asset generators for the README and the website.
//!
//! This target is a plain binary (`harness = false`), not a test suite. Each
//! argument names one asset to re-record with VHS:
//!
//! - `demo`: the README and landing-page hero,
//!   `docs/site/static/demo/demo.gif`.
//! - `sessions` and `tab_tour`: the feature GIFs of the same name under
//!   `docs/site/static/features/`.
//!
//! Generation always re-records, fails when VHS is missing or produces no
//! GIF, and removes the asset's same-named PNG poster so a stale poster cannot
//! survive a new recording. Run it only through the `e2e-container` hook with
//! `AGENTTY_MARKETING_ASSETS`, as `docs/contributing/feature-test/recording.md`
//! describes; never record on the bare host.

mod demo;
mod feature;
mod vhs;

#[path = "../support/clock.rs"]
mod clock;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// Result type shared by the generators.
type ShowcaseResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// One recordable marketing asset.
#[derive(Clone, Copy)]
enum Asset {
    Demo,
    Sessions,
    TabTour,
}

impl Asset {
    /// Command-line names of every asset.
    const NAMES: [(&str, Self); 3] = [
        ("demo", Self::Demo),
        ("sessions", Self::Sessions),
        ("tab_tour", Self::TabTour),
    ];

    /// Resolves a command-line asset name.
    fn parse(name: &str) -> ShowcaseResult<Self> {
        Self::NAMES
            .into_iter()
            .find_map(|(known, asset)| (known == name).then_some(asset))
            .ok_or_else(|| format!("Unknown asset `{name}`; expected {}", Self::expected()).into())
    }

    /// Lists the accepted names for error messages.
    fn expected() -> String {
        Self::NAMES.map(|(name, _)| name).join(", ")
    }

    /// Records the asset and returns the GIF it wrote.
    async fn generate(self) -> ShowcaseResult<PathBuf> {
        match self {
            Self::Demo => demo::generate().await,
            Self::Sessions => feature::generate_sessions_tab().await,
            Self::TabTour => feature::generate_tab_tour().await,
        }
    }
}

/// Records every asset named on the command line.
#[tokio::main]
async fn main() -> ExitCode {
    let names = std::env::args().skip(1).collect::<Vec<_>>();
    match generate(&names).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr(), "Marketing asset generation failed: {error}");

            ExitCode::FAILURE
        }
    }
}

/// Validates every name and VHS before recording the assets in order.
async fn generate(names: &[String]) -> ShowcaseResult {
    if names.is_empty() {
        return Err(format!("Name at least one asset: {}", Asset::expected()).into());
    }
    let assets = names
        .iter()
        .map(|name| Asset::parse(name))
        .collect::<ShowcaseResult<Vec<_>>>()?;
    vhs::require_installed().await?;

    for asset in assets {
        let gif_path = asset.generate().await?;
        writeln!(io::stdout(), "Recorded {}", gif_path.display())?;
    }

    Ok(())
}
