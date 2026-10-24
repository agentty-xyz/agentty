pub(crate) mod command;
mod file;
mod inspection;
mod output;
mod runtime;
#[cfg(test)]
#[path = "read/read_test.rs"]
mod tests;

pub(crate) use output::{
    InspectionError, ResultFits, fit_tool_result, invalid_arguments_tool_result,
};
pub use output::{ReadError, ReadOutput};
pub(crate) use runtime::ReadTool;
