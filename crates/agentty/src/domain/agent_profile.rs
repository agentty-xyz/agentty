//! Compatibility exports for frontend-neutral workspace agents.

pub use ag_session::{
    AGENT_PROFILE_PROMPT_MAX_BYTES, AgentDefinition, AgentDefinitionParseError,
    AgentDefinitionSource, AgentDefinitionSummary, agent_definition_identity,
    bundled_agent_definitions, parse_agent_definition, parse_agent_summary,
};
