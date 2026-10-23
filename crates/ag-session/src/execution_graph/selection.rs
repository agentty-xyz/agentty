//! Graph snapshots retain provider and model together without default fallback.

use std::borrow::Borrow;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::{AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata};

pub(super) fn serialize<S>(
    selection: impl Borrow<AgentSelection>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let selection = selection.borrow();

    (selection.kind().name(), selection.model().as_str()).serialize(serializer)
}

pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<AgentSelection, D::Error>
where
    D: Deserializer<'de>,
{
    let (kind, model) = <(String, String)>::deserialize(deserializer)?;
    let kind: AgentKind = kind.parse().map_err(de::Error::custom)?;
    let model: AgentModel = model.parse().map_err(de::Error::custom)?;
    if !kind.supports_model(model) {
        return Err(de::Error::custom("graph model does not belong to provider"));
    }

    Ok(AgentSelection::new(kind, model))
}

#[cfg(test)]
#[path = "selection_test.rs"]
mod tests;
