//! Design tokens: `design/tokens.json` is the only place the web surfaces'
//! design values are written, and `design/base.css` the only place their
//! shared foundations and components are. This task generates the token
//! stylesheets and the surfaces' copies of the base stylesheet, and checks
//! that those are current, that hand-written stylesheets name no raw colour,
//! and that the declared colour pairs keep their contrast.

mod contrast;
mod stylesheet;
mod task;
mod token;

pub(crate) use task::{Options, run};
