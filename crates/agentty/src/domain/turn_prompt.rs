//! Compatibility re-exports for shared protocol turn prompt payloads.

pub use ag_protocol::{
    TurnPrompt, TurnPromptAttachment, TurnPromptContentPart, TurnPromptTextSource,
    render_prompt_text_for_agent, split_turn_prompt_content,
};
