use std::io::{self, Write};

use ag_harness::{ModelClient, ModelRequest, OutputSchema};
use serde_json::{Value, json};

use crate::telemetry::DynError;

/// Requests and prints the validated greeting shared by provider examples.
pub(crate) async fn request(client: ModelClient, provider_name: &str) -> Result<(), DynError> {
    let schema = json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "const": "hello"
            }
        },
        "required": ["message"],
        "additionalProperties": false
    });
    let schema = OutputSchema::new(schema)?;
    let response = client
        .complete(ModelRequest::new(
            "Return a JSON greeting with the message set to hello.",
            schema,
        ))
        .await?;

    write_output(response.output(), provider_name)?;

    Ok(())
}

fn write_output(output: Option<&Value>, provider_name: &str) -> Result<(), io::Error> {
    let output = output.ok_or_else(|| {
        io::Error::other(format!("{provider_name} returned an unexpected tool call"))
    })?;
    writeln!(io::stdout().lock(), "{output}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_output_identifies_unexpected_tool_call_provider() {
        // Arrange and Act
        let error =
            write_output(None, "Test provider").expect_err("missing structured output should fail");

        // Assert
        assert_eq!(
            error.to_string(),
            "Test provider returned an unexpected tool call"
        );
    }
}
