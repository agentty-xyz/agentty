//! Compatibility exports for frontend-neutral workspace personalities.

pub use ag_session::{
    PERSONALITY_PROMPT_MAX_BYTES, Personality, PersonalityParseError, PersonalitySummary,
    parse_agent_definition, parse_agent_summary,
};
