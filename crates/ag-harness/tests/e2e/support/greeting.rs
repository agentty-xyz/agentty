use std::io::{self, Write};

use ag_harness::{Model, ModelRequest, OutputSchema};
use serde_json::{Value, json};

use crate::DynError;

/// Requests and prints the validated greeting shared by provider checks.
pub(crate) async fn request(model: impl Model, provider_name: &str) -> Result<(), DynError> {
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
    let response = model
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
