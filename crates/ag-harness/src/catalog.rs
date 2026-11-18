//! Model catalog for hosts that select models by identifier, role, or
//! capability.
//!
//! A [`ModelCatalog`] holds [`ModelSpec`]s, each with [`Capabilities`] and
//! optional [`Pricing`] in integer micro-USD, and maps open [`Role`]s to
//! ordered candidate models. [`ModelCatalog::resolve`] turns a
//! [`ModelSelector`] into a [`ModelHandle`] carrying the model and the
//! request budget its profile declares. The catalog is optional: a harness
//! needs only one model.
//!
//! ```no_run
//! use ag_harness::catalog::{ModelCatalog, ModelSelector, Role};
//!
//! let catalog = ModelCatalog::from_environment()?;
//! let cheap = catalog.resolve(&ModelSelector::Role(Role::CHEAP))?;
//! println!("{}", cheap.id());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod model_catalog;
mod role;
mod spec;

pub use model_catalog::{CatalogBuilder, CatalogError, ModelCatalog, ModelHandle};
pub use role::{ModelSelector, Role};
pub use spec::{Capabilities, CostTier, ModelId, ModelSpec, Pricing, PricingSource};
