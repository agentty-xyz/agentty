use crate::agent::app_server::stdio_transport::{
    AppServerRuntimeTransport, AppServerStdioTransport,
};
use crate::app_server_transport::{self, AppServerTransportError};

#[tokio::test]
async fn next_stdout_reports_unreadable_output_with_transport_context() {
    // Arrange
    let mut command = std::process::Command::new("sh");
    command.args(["-c", r"printf '\377\n'"]);
    let (mut child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(command, "invalid-output fixture")
            .expect("invalid-output fixture should spawn");
    let mut transport = AppServerStdioTransport::new(
        stdin,
        stdout,
        "test stdin is unavailable",
        "failed reading test stdout",
    );

    // Act
    let result = transport.next_stdout().await;

    // Assert
    assert!(
        matches!(
            &result,
            Err(AppServerTransportError::Io { context, source })
                if context == "failed reading test stdout"
                    && source.kind() == std::io::ErrorKind::InvalidData
        ),
        "unexpected stdout result: {result:?}"
    );
    transport.close_stdin();
    app_server_transport::shutdown_child(&mut child).await;
}

#[tokio::test]
async fn response_wait_round_trips_matching_line() {
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
        .wait_for_response_line("response-1".to_string())
        .await;

    // Assert
    assert_eq!(
        response_line.expect("matching response should arrive"),
        payload.to_string()
    );
    transport.close_stdin();
    app_server_transport::shutdown_child(&mut child).await;
}
