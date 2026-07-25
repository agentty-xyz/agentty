//! Sends a fixed prompt to a configured Qwen model and prints its response.

use std::error::Error;
use std::io;
use std::io::Write;

use ag_harness::{Model, ModelRequest, Qwen, QwenConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let model = Qwen::new(QwenConfig {
        api_key: std::env::var("DASHSCOPE_API_KEY")?,
        base_url: std::env::var("DASHSCOPE_BASE_URL")?,
        model: "qwen-plus".to_string(),
    });
    let response = model
        .complete(ModelRequest::text("Reply with exactly: hello"))
        .await?;

    writeln!(io::stdout().lock(), "{}", response.text())?;

    Ok(())
}
