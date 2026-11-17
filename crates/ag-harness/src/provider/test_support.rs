//! Expected wire limits and instruction text for provider boundary tests.

pub(crate) const ERROR_BODY_LIMIT_BYTES: usize = 4 * 1024;
pub(crate) const RESPONSE_ENVELOPE_LIMIT_BYTES: usize = 64 * 1024;
pub(crate) const SUCCESS_BODY_LIMIT_BYTES: usize =
    2 * 1024 * 1024 * 6 + RESPONSE_ENVELOPE_LIMIT_BYTES;
pub(crate) const STRUCTURED_OUTPUT_INSTRUCTION: &str = concat!(
    "Return only one JSON object. The object must validate against this JSON Schema. ",
    "Do not include Markdown fences or any other text.\n\nJSON Schema:\n",
);

pub(crate) fn reqwest_source<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a reqwest::Error> {
    let mut current = error.source();
    while let Some(source) = current {
        if let Some(reqwest) = source.downcast_ref::<reqwest::Error>() {
            return Some(reqwest);
        }
        current = source.source();
    }

    None
}

#[cfg(test)]
#[path = "test_support_test.rs"]
mod tests;
