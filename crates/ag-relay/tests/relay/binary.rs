//! The `ag-relay` binary: startup output, stop signals, and startup errors.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Command, Stdio};

use crate::support::TestResult;

/// Starts the binary on a free port, probes it, sends `signal`, and returns
/// the probe response with whether the binary exited successfully.
fn serve_until(signal: &str) -> TestResult<(String, bool)> {
    let mut relay = Command::new(env!("CARGO_BIN_EXE_ag-relay"))
        .args(["--listen", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = relay.stdout.take().ok_or("stdout should be piped")?;
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line)?;
    let address = line
        .trim()
        .strip_prefix("listening on ")
        .ok_or("unexpected startup line")?;
    let mut connection = TcpStream::connect(address)?;
    connection.write_all(b"GET /healthz HTTP/1.1\r\nHost: relay\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    connection.read_to_string(&mut response)?;
    let signaled = Command::new("kill")
        .args([signal, &relay.id().to_string()])
        .status()?;
    let exit = relay.wait()?;

    Ok((response, signaled.success() && exit.success()))
}

#[test]
fn binary_serves_until_terminated() -> TestResult {
    // Arrange
    let signal = "-TERM";

    // Act
    let (response, stopped_cleanly) = serve_until(signal)?;

    // Assert
    assert!(response.ends_with("ok"));
    assert!(stopped_cleanly);

    Ok(())
}

#[test]
fn binary_stops_on_interrupt() -> TestResult {
    // Arrange
    let signal = "-INT";

    // Act
    let (response, stopped_cleanly) = serve_until(signal)?;

    // Assert
    assert!(response.ends_with("ok"));
    assert!(stopped_cleanly);

    Ok(())
}

#[test]
fn binary_reports_an_address_it_cannot_bind() -> TestResult {
    // Arrange
    let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let address = occupied.local_addr()?.to_string();

    // Act
    let output = Command::new(env!("CARGO_BIN_EXE_ag-relay"))
        .args(["--listen", &address])
        .output()?;

    // Assert
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success());
    assert!(stderr.contains("ag-relay: Cannot bind the relay listener"));

    Ok(())
}
