//! `ag-release` plans and packages GitHub releases of the binaries in a Cargo
//! workspace, configured through `[workspace.metadata.release]`.
//!
//! The crate README is the public contract for configuration, commands, and
//! artifacts.

mod archive;
mod artifact;
mod assemble;
mod build;
mod changelog;
mod checksum;
mod cli;
mod config;
mod file_system;
mod npm_package;
mod plan;
mod process;
mod release_note;
mod repository;
mod shell_installer;
mod tag;
mod target;
mod workspace;

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Host};
use crate::file_system::HostFileSystem;
use crate::process::HostProcessRunner;

/// Runs the selected release command and returns the process exit code.
fn main() -> ExitCode {
    let cli = Cli::parse();
    let host = Host {
        cargo: env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")),
        file_system: &HostFileSystem,
        process: &HostProcessRunner,
        source_date_epoch: env::var_os(archive::SOURCE_DATE_EPOCH),
    };

    match host.run(&cli.command, &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Nothing is left to report to when standard error is closed.
            let _ = writeln!(io::stderr().lock(), "error: {error}");

            ExitCode::FAILURE
        }
    }
}
