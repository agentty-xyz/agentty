//! Declarative YAML scenario layer.
//!
//! [`model`] holds the pure-data scenario types deserialized from a YAML file;
//! [`runtime`] lowers them onto the runtime engine and evaluates expectations.
//! This layer powers the language-agnostic `testty run scenario.yaml` front end
//! on top of the same engine as the Rust authoring API.

pub mod model;
pub mod runtime;
