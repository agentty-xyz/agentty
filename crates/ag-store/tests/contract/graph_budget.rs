//! Provider-call limits are fixed at first launch and spending survives
//! reopening, so relaunching a graph never replenishes its credit.

use std::collections::BTreeMap;
use std::error::Error;
use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, TurnPrompt};
use ag_session::{
    AgentDefinition, AgentKind, AgentModel, AgentSelection, EnvId, ExecutionGraph,
    ExtendGraphBudgetRequest, GraphContext, GraphControlRequest, GraphExecutionProfile, GraphNode,
    GraphOutput, GraphPostProcessing, GraphWorkspaceAccess,
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
async fn recorded_extensions_add_credit_once_and_keep_the_launch_limits()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::Builder::new()
        .prefix("graph-budget-")
        .tempdir_in(".")?;
    let path = directory.path().join("budget.sqlite");
    let database = open(&path).await?;
    let limits = BTreeMap::from([("calls".to_string(), 2)]);
    database
        .graphs()
        .launch_graph(&graph("graph"), &limits)
        .await?;
    database
        .graphs()
        .record_budget_spending("graph", "calls", 2)
        .await?;
    let extension = extension("extension", 3);

    // Act
    let recorded = database.graphs().extend_budget(&extension).await?;
    let redelivered = database.graphs().extend_budget(&extension).await?;
    database
        .graphs()
        .record_budget_spending("graph", "calls", 1)
        .await?;
    database.pool().close().await;
    drop(database);
    let reopened = Database::open(&path).await?;
    let relaunched = reopened
        .graphs()
        .launch_graph(&graph("graph"), &limits)
        .await?;

    // Assert
    assert!(recorded);
    assert!(!redelivered);
    assert_eq!(relaunched["calls"], 2);
    assert_eq!(reopened.graphs().budget_limits("graph").await?, limits);
    let extended_limits = BTreeMap::from([("calls".to_string(), 5)]);
    assert!(
        reopened
            .graphs()
            .launch_graph(&graph("graph"), &extended_limits)
            .await
            .is_err()
    );

    Ok(())
}

#[tokio::test]
async fn invalid_or_conflicting_extensions_record_nothing() -> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;
    let limits = BTreeMap::from([("calls".to_string(), 1)]);
    database
        .graphs()
        .launch_graph(&graph("graph"), &limits)
        .await?;
    database
        .graphs()
        .launch_graph(&graph("canceled"), &limits)
        .await?;
    database
        .graphs()
        .cancel(&GraphControlRequest {
            graph_id: "canceled".into(),
            revision: NonZeroU64::MIN,
        })
        .await?;
    database
        .graphs()
        .extend_budget(&extension("taken", 2))
        .await?;
    let mut stale = extension("stale", 2);
    stale.graph.revision = NonZeroU64::new(2).ok_or("revision")?;
    let mut unknown = extension("unknown", 2);
    unknown.budget_id = "other".into();
    let mut canceled = extension("canceled", 2);
    canceled.graph.graph_id = "canceled".into();

    // Act
    let rejected = [
        database.graphs().extend_budget(&extension("none", 0)).await,
        database
            .graphs()
            .extend_budget(&extension(
                "excessive",
                ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS + 1,
            ))
            .await,
        database
            .graphs()
            .extend_budget(&extension("taken", 3))
            .await,
        database.graphs().extend_budget(&stale).await,
        database.graphs().extend_budget(&unknown).await,
        database.graphs().extend_budget(&canceled).await,
    ];

    // Assert
    let messages: Vec<String> = rejected
        .into_iter()
        .map(|result| result.expect_err("rejected extension").to_string())
        .collect();
    for (message, expected) in messages.iter().zip([
        "Add between 1 and 1000 provider calls",
        "Add between 1 and 1000 provider calls",
        "used for a different extension",
        "Stale graph revision",
        "does not exist or cannot grow further",
        "Canceled graphs cannot gain provider calls",
    ]) {
        assert!(message.contains(expected), "{message}");
    }
    let remaining = database
        .graphs()
        .launch_graph(&graph("graph"), &limits)
        .await?;
    assert_eq!(remaining["calls"], 3);

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
            &ag_worker::GraphEnvLocation::Folder("workspace".into()),
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

fn extension(action_id: &str, additional_calls: usize) -> ExtendGraphBudgetRequest {
    ExtendGraphBudgetRequest {
        action_id: action_id.into(),
        additional_calls,
        budget_id: "calls".into(),
        graph: GraphControlRequest {
            graph_id: "graph".into(),
            revision: NonZeroU64::MIN,
        },
    }
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
