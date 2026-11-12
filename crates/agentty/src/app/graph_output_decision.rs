//! Model decision for a chat reply to a graph output awaiting acceptance.

use std::path::PathBuf;

use ag_session::AgentSelection;
use ag_worker::RunClient;
use askama::Template;

use crate::app::graph_reply_decision;

/// Askama view model whose inputs are already JSON-encoded strings.
#[derive(Template)]
#[template(path = "graph_output_reply_decision_prompt.md", escape = "none")]
struct GraphOutputReplyDecisionPromptTemplate<'a> {
    output: &'a str,
    reply: &'a str,
}

/// Output action a chat reply resolves to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphOutputReply {
    /// Accept the exact inspected output version.
    Accept,
    /// Send the reply to the producing node as revision guidance.
    Revise,
}

/// Inputs for deciding whether a reply accepts one awaited output.
pub(crate) struct GraphOutputReplyDecision {
    /// Provider and model captured by the producing node.
    pub(crate) agent: AgentSelection,
    /// Session workspace used as the read-only run folder.
    pub(crate) folder: PathBuf,
    /// Producer, waiting work, and the output text the reply refers to.
    pub(crate) output: String,
    /// User-authored reply to classify.
    pub(crate) reply: String,
}

impl GraphOutputReplyDecision {
    /// Decides through one read-only utility run on the Run Worker. Only an
    /// explicit `accept` accepts the output; run failures, unparsable output,
    /// and unknown words revise it, so waiting work never starts on an
    /// uncertain decision.
    pub(crate) async fn decide(&self, run_client: &dyn RunClient) -> GraphOutputReply {
        graph_reply_decision::decide(
            run_client,
            self.agent,
            &self.folder,
            self.prompt(),
            "graph output",
            Self::parse,
        )
        .await
        .unwrap_or(GraphOutputReply::Revise)
    }

    /// Renders the task contract with output and reply passed as JSON data.
    fn prompt(&self) -> String {
        let output = serde_json::json!(self.output).to_string();
        let reply = serde_json::json!(self.reply).to_string();
        let template = GraphOutputReplyDecisionPromptTemplate {
            output: &output,
            reply: &reply,
        };

        // Rendering borrowed strings into a `String` cannot fail. A blank
        // prompt would still degrade safely to a revision.
        template.render().unwrap_or_default()
    }

    /// Accepts only the two contract words.
    fn parse(answer: &str) -> Option<GraphOutputReply> {
        match graph_reply_decision::contract_word(answer).as_str() {
            "accept" => Some(GraphOutputReply::Accept),
            "revise" => Some(GraphOutputReply::Revise),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "graph_output_decision_test.rs"]
mod tests;
