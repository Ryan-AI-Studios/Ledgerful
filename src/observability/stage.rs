//! Ungated command-stage span helper (0364).
//!
//! Lives outside `feature = "self-timing"` so `execute_search` compiles under
//! `cargo build --no-default-features --features mcp,export`.
//!
//! `tracing` 0.1.44 requires a **literal** span name (`static` callsite). Do not
//! add a runtime `fn stage(name: &str)`.

/// Open a `ledgerful::timing` span with a frozen literal name.
///
/// ```ignore
/// let _g = stage_span!("lexical_query").entered();
/// ```
macro_rules! stage_span {
    ($name:literal) => {
        ::tracing::info_span!(target: "ledgerful::timing", $name)
    };
}

pub(crate) use stage_span;
