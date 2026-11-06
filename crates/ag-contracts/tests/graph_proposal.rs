//! Public request routing for graph proposal generation.

use ag_contracts::AgentRequestKind;
use ag_protocol::ProtocolRequestProfile;

#[test]
fn graph_proposal_requests_select_the_dedicated_wire_profile() {
    // Arrange
    let request = AgentRequestKind::GraphProposal;

    // Act
    let profile = request.protocol_profile();

    // Assert
    assert_eq!(profile, ProtocolRequestProfile::GraphProposal);
    assert!(!request.is_resume());
}
