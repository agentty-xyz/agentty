//! The `ag-relay` binary: startup output, stop signals, its options, and
//! startup errors.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};

use crate::support::{TestResult, keypair, refused_registration, register_at};

/// Starts the binary on a free port with `options` and returns it with the
/// address its startup line names.
fn spawn_relay(options: &[&str]) -> TestResult<(Child, SocketAddr)> {
    let mut relay = Command::new(env!("CARGO_BIN_EXE_ag-relay"))
        .args(["--listen", "127.0.0.1:0"])
        .args(options)
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = relay.stdout.take().ok_or("stdout should be piped")?;
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line)?;
    let address = line
        .trim()
        .strip_prefix("listening on ")
        .ok_or("unexpected startup line")?
        .parse()?;

    Ok((relay, address))
}

/// Sends `signal` to `relay` and returns whether it exited successfully.
fn stop(mut relay: Child, signal: &str) -> TestResult<bool> {
    let signaled = Command::new("kill")
        .args([signal, &relay.id().to_string()])
        .status()?;
    let exit = relay.wait()?;

    Ok(signaled.success() && exit.success())
}

/// Starts the binary on a free port, probes it, sends `signal`, and returns
/// the probe response with whether the binary exited successfully.
fn serve_until(signal: &str) -> TestResult<(String, bool)> {
    let (relay, address) = spawn_relay(&[])?;
    let mut connection = TcpStream::connect(address)?;
    connection.write_all(b"GET /healthz HTTP/1.1\r\nHost: relay\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    connection.read_to_string(&mut response)?;
    let stopped_cleanly = stop(relay, signal)?;

    Ok((response, stopped_cleanly))
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

#[tokio::test]
async fn allow_instance_registers_only_the_listed_instances() -> TestResult {
    // Arrange
    let listed = keypair()?;
    let unlisted = keypair()?;
    let listed_id = listed.public().fingerprint().to_ascii_uppercase();
    let (relay, address) = spawn_relay(&["--allow-instance", &listed_id])?;

    // Act
    let registered = register_at(address, &listed).await;
    let refusal = refused_registration(address, &unlisted).await;
    let stopped_cleanly = stop(relay, "-TERM")?;

    // Assert
    assert!(registered.is_ok());
    assert_eq!(refusal?, (1008, "Instance is not admitted".to_string()));
    assert!(stopped_cleanly);

    Ok(())
}

#[test]
fn binary_rejects_an_allowed_instance_that_is_not_an_identifier() -> TestResult {
    // Arrange
    let short_id = "ab".repeat(31);

    // Act
    let output = Command::new(env!("CARGO_BIN_EXE_ag-relay"))
        .args(["--allow-instance", &short_id])
        .output()?;

    // Assert
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success());
    assert!(stderr.contains("expected a 64-character hexadecimal instance identifier"));

    Ok(())
}
