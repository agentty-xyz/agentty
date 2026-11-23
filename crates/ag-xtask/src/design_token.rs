//! Design tokens: `design/tokens.json` is the only place the web surfaces'
//! design values and self-hosted font faces are written, `design/base.css`
//! the only place their shared foundations and components are, and `design/`
//! the only place the brand assets are. This task generates the token and
//! font stylesheets, the surfaces' copies of the base stylesheet and the
//! brand assets, and the value of every `theme-color` meta in their
//! templates, and checks that those are current, that hand-written
//! stylesheets and the surfaces' scripts and templates name no raw colour,
//! and that the declared colour pairs reach their contrast minimum.

mod contrast;
mod font_face;
mod script;
mod stylesheet;
mod task;
mod template;
mod token;

pub(crate) use task::{Options, run};
