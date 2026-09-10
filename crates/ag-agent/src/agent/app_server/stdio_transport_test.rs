use super::*;

#[tokio::test]
async fn explicit_response_timeout_round_trips_matching_line() {
    // Arrange
    let command = std::process::Command::new("cat");
    let (mut child, stdin, stdout) = app_server_transport::spawn_runtime_command(command, "cat")
        .expect("`cat` transport fixture should spawn");
    let mut transport = AppServerStdioTransport::new(
        stdin,
        stdout,
        "test stdin is unavailable",
        "failed reading test stdout",
    );
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "response-1",
        "result": {},
    });
    transport
        .write_json_line(payload.clone())
        .await
        .expect("fixture request should be written");

    // Act
    let response_line = transport
        .wait_for_response_line_with_timeout("response-1".to_string(), Duration::from_secs(1))
        .await;

    // Assert
    assert_eq!(
        response_line.expect("matching response should arrive"),
        payload.to_string()
    );
    transport.close_stdin();
    app_server_transport::shutdown_child(&mut child).await;
}
