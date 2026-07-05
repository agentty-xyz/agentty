//! Compatibility re-exports for shared structured response protocol APIs.

pub use ag_protocol::{
    AgentResponse, AgentResponseSummary, ProtocolRequestProfile, agent_response_output_schema,
    agent_response_output_schema_json, build_protocol_repair_prompt,
    format_protocol_parse_debug_details, normalize_turn_response, parse_agent_response_strict,
};
