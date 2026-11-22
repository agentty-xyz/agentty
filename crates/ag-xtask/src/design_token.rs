//! Design tokens: `design/tokens.json` is the only place the web surfaces'
//! design values are written. This task generates their stylesheets from it
//! and checks that those are current, that hand-written stylesheets name no
//! raw colour, and that the declared colour pairs keep their contrast.

mod contrast;
mod stylesheet;
mod task;
mod token;

pub(crate) use task::{Options, run};
