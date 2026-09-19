use std::io::{self, Write};
use std::sync::Arc;

use ag_harness::{
    Harness, ImageContent, ImageMediaType, InputBlock, MemoryStore, ModelClient, ModelError,
    OutputSchema, TurnError, TurnInput,
};
use serde_json::{Value, json};

use crate::DynError;

const RED_PNG: &[u8] = include_bytes!("red.png");
const BLUE_JPEG: &[u8] = include_bytes!("blue.jpg");
/// Keeps the first durable answer free of colors, so a correct follow-up can
/// only come from the replayed images.
pub(crate) const REMEMBER: &str =
    "Remember both images for later. For this turn only, answer black for both fields.";
pub(crate) const REPORT: &str = "Report the dominant color of each image.";
pub(crate) const RECALL: &str =
    "No new images. Report the actual dominant colors of the two images I sent earlier.";

/// Returns ordered text, PNG, text, and JPEG blocks whose colors identify
/// each image and its position.
pub(crate) fn colored_images(instruction: &str) -> Result<TurnInput, DynError> {
    let input = TurnInput::from_blocks(vec![
        InputBlock::Text(format!("{instruction} First image:")),
        InputBlock::Image(ImageContent::new(ImageMediaType::Png, RED_PNG.to_vec())?),
        InputBlock::Text("Second image:".to_string()),
        InputBlock::Image(ImageContent::new(ImageMediaType::Jpeg, BLUE_JPEG.to_vec())?),
    ])?;

    Ok(input)
}

pub(crate) fn color_schema() -> Result<OutputSchema, DynError> {
    let color =
        json!({"type": "string", "enum": ["red", "green", "blue", "yellow", "black", "white"]});
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {"first": color, "second": color},
        "required": ["first", "second"],
        "additionalProperties": false
    }))?;

    Ok(schema)
}

pub(crate) fn assert_colors(output: &Value, scenario: &str) -> Result<(), DynError> {
    writeln!(io::stdout().lock(), "{scenario}: {output}")?;
    if output != &json!({"first": "red", "second": "blue"}) {
        return Err(format!("{scenario} misread the ordered images: {output}").into());
    }

    Ok(())
}

/// Sends ordered PNG and JPEG content through a one-shot turn, then through a
/// durable turn whose text follow-up depends on replayed image history.
pub(crate) async fn describes_images(
    client: ModelClient,
    provider_name: &str,
) -> Result<(), DynError> {
    let harness = Harness::new(client).store(Arc::new(MemoryStore::new()));

    let one_shot = harness
        .run_once(colored_images(REPORT)?, color_schema()?)
        .await?;
    assert_colors(one_shot.output(), &format!("{provider_name} one-shot"))?;

    let mut session = harness.session("vision", color_schema()?).create().await?;
    session.send(colored_images(REMEMBER)?).await?;
    let follow_up = session.send(RECALL).await?;

    assert_colors(
        follow_up.output(),
        &format!("{provider_name} replayed history"),
    )
}

/// Confirms a text-only configuration rejects images with the typed error and
/// still completes text turns.
pub(crate) async fn rejects_images(
    client: ModelClient,
    provider_name: &str,
) -> Result<(), DynError> {
    let harness = Harness::new(client);

    let rejected = harness
        .run_once(colored_images(REPORT)?, color_schema()?)
        .await;
    if !matches!(
        rejected,
        Err(TurnError::Model(ModelError::UnsupportedImageInput { .. }))
    ) {
        return Err(format!("{provider_name} must reject image input: {rejected:?}").into());
    }
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {"message": {"type": "string", "const": "hello"}},
        "required": ["message"],
        "additionalProperties": false
    }))?;
    let text = harness
        .run_once(
            "Return a JSON greeting with the message set to hello.",
            schema,
        )
        .await?;
    writeln!(
        io::stdout().lock(),
        "{provider_name} text after rejection: {}",
        text.output()
    )?;

    Ok(())
}
