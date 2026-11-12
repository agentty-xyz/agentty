//! Provider-call limits are fixed at first launch and spending survives
//! reopening, so relaunching a graph never replenishes its credit.

use std::collections::BTreeMap;
use std::error::Error;
use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, TurnPrompt};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, EnvId, ExecutionGraph, GraphContext,
    GraphExecutionProfile, GraphNode, GraphOutput, GraphPostProcessing, GraphWorkspaceAccess,
};
use ag_store::Database;

#[tokio::test]
async fn launch_fixes_limits_and_reopening_resumes_from_recorded_spending()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::Builder::new()
        .prefix("graph-budget-")
        .tempdir_in(".")?;
    let path = directory.path().join("budget.sqlite");
    let database = open(&path).await?;
    let limits = BTreeMap::from([("calls".to_string(), 4)]);

    // Act
    let first = database
        .graphs()
        .launch_graph(&graph("graph"), &limits)
        .await?;
    database
        .graphs()
        .record_budget_spending("graph", "calls", 3)
        .await?;
    database
        .graphs()
        .record_budget_spending("graph", "calls", -1)
        .await?;
    database.pool().close().await;
    drop(database);
    let reopened = Database::open(&path).await?;
    let relaunched = reopened
        .graphs()
        .launch_graph(&graph("graph"), &limits)
        .await?;
    reopened
        .graphs()
        .record_budget_spending("graph", "calls", 10)
        .await?;
    let exhausted = reopened
        .graphs()
        .launch_graph(&graph("graph"), &limits)
        .await?;
    reopened
        .graphs()
        .record_budget_spending("graph", "calls", -10)
        .await?;
    let refunded = reopened
        .graphs()
        .launch_graph(&graph("graph"), &limits)
        .await?;

    // Assert
    assert_eq!(first, limits);
    assert_eq!(relaunched["calls"], 2);
    assert_eq!(exhausted["calls"], 0);
    assert_eq!(refunded["calls"], 4);
    assert_eq!(reopened.graphs().budget_limits("graph").await?, limits);
    assert!(
        reopened
            .graphs()
            .launch_graph(&graph("graph"), &BTreeMap::from([("calls".into(), 9)]))
            .await
            .is_err()
    );
    assert!(
        reopened
            .graphs()
            .launch_graph(&graph("graph"), &BTreeMap::new())
            .await
            .is_err()
    );

    Ok(())
}

#[tokio::test]
async fn graphs_without_limits_or_with_unknown_budgets_keep_no_ledger() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;

    // Act
    let unbudgeted = database
        .graphs()
        .launch_graph(&graph("plain"), &BTreeMap::new())
        .await?;
    let unknown = database
        .graphs()
        .record_budget_spending("plain", "calls", 1)
        .await;
    let oversized = database
        .graphs()
        .launch_graph(
            &graph("oversized"),
            &BTreeMap::from([("calls".into(), usize::MAX)]),
        )
        .await;

    // Assert
    assert_eq!(unbudgeted, BTreeMap::new());
    assert!(unknown.is_err());
    assert!(oversized.is_err());
    assert_eq!(
        database.graphs().budget_limits("plain").await?,
        BTreeMap::new()
    );
    assert!(
        database
            .graphs()
            .load_graph("oversized", None)
            .await?
            .is_none()
    );

    Ok(())
}

#[tokio::test]
async fn a_graph_that_executed_without_a_ledger_cannot_gain_fresh_limits()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;
    let snapshot = graph("legacy");
    database.graphs().save_graph(&snapshot, None).await?;
    let admission = database
        .graph_dispatch()
        .claim(
            "legacy",
            snapshot.revision,
            "node",
            &uuid::Uuid::new_v4().to_string(),
            std::path::Path::new("workspace"),
        )
        .await?;
    assert!(matches!(admission, ag_worker::GraphAdmission::Claimed(_)));

    // Act
    let launched = database
        .graphs()
        .launch_graph(&snapshot, &BTreeMap::from([("calls".into(), 2)]))
        .await;

    // Assert
    assert!(
        launched
            .expect_err("legacy graph")
            .to_string()
            .contains("before durable provider budgets")
    );
    assert_eq!(
        database.graphs().budget_limits("legacy").await?,
        BTreeMap::new()
    );

    Ok(())
}

async fn open(path: &std::path::Path) -> Result<Database, Box<dyn Error>> {
    let database = Database::open(path).await?;
    seed(&database).await?;

    Ok(database)
}

async fn seed(database: &Database) -> Result<(), Box<dyn Error>> {
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await?;

    Ok(())
}

fn graph(id: &str) -> ExecutionGraph {
    ExecutionGraph {
        id: id.into(),
        nodes: vec![GraphNode {
            agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
            definition: AgentDefinition {
                description: "Inspect".into(),
                id: "inspector".into(),
                name: "Inspector".into(),
                prompt: "Inspect the workspace".into(),
                read_only: true,
            },
            dependencies: Vec::new(),
            env_id: EnvId::SessionWorktree("session".into()),
            execution: GraphExecutionProfile {
                context: GraphContext::Fresh,
                post_processing: GraphPostProcessing::None,
                response_profile: ProtocolRequestProfile::UtilityPrompt,
                shared_budget_id: Some("calls".into()),
                workspace_access: GraphWorkspaceAccess::SharedRead,
            },
            id: "node".into(),
            inputs: Vec::new(),
            objective: "Inspect".into(),
            outputs: vec![GraphOutput {
                name: "response".into(),
                schema: ProtocolRequestProfile::UtilityPrompt,
            }],
            request: TurnPrompt::from_agent_data("Inspect".into()),
        }],
        revision: NonZeroU64::MIN,
        session_id: "session".into(),
    }
}
