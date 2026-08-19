mod builder;
mod persist;
mod types;

pub use builder::CallGraphBuilder;
#[cfg(test)]
pub(crate) use builder::parse_symbol_kind;
pub use types::{CallEdge, CallGraph, CallGraphStats, CallKind, ResolutionStatus};

#[cfg(test)]
mod tests;
