//! Live ChatGPT-subscription Codex provider check.

use ag_harness::{Codex, CodexConfig};

use crate::{DynError, greeting};

const MODEL: &str = "gpt-5.6-luna";

#[tokio::test]
#[ignore = "requires live ChatGPT credentials and verifies ag-harness routing"]
async fn test_codex_luna_with_ag_harness_originator() -> Result<(), DynError> {
    // Arrange
    let model = Codex::new(CodexConfig::new(MODEL))?;

    // Act and Assert
    greeting::request(model, "Codex Luna").await
}
