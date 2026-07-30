//! Requests structured output from a configured Qwen model and prints the
//! validated JSON.

use std::error::Error;
use std::io::{self, Write};

use ag_harness::{Model, ModelRequest, OutputSchema, Qwen, QwenConfig};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let model = Qwen::new(QwenConfig {
        api_key: std::env::var("DASHSCOPE_API_KEY")?,
        base_url: std::env::var("DASHSCOPE_BASE_URL")?,
        model: "qwen-plus".to_string(),
    });
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "const": "hello"
            }
        },
        "required": ["message"],
        "additionalProperties": false
    }))?;
    let response = model
        .complete(ModelRequest::new(
            "Return a JSON greeting with the message set to hello.",
            schema,
        ))
        .await?;

    writeln!(io::stdout().lock(), "{}", response.output())?;

    Ok(())
}
