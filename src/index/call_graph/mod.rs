mod builder;
mod persist;
mod types;

pub use builder::CallGraphBuilder;
#[cfg(test)]
pub(crate) use builder::parse_symbol_kind;
pub(crate) use builder::{CallerLookupRow, lookup_caller_id};
pub use types::{CallEdge, CallGraph, CallGraphStats, CallKind, ResolutionStatus};

#[cfg(test)]
mod tests;
