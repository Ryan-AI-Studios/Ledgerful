//! SCIP index-command surface (0095).
//!
//! Implementation lives in `crate::scip::augment` so graph analysis can call it
//! without a `commands` ↔ `index` cycle. This module documents the CLI entry
//! points; prefer `crate::scip::{maybe_run_scip_augment, ScipIndexJson}` in code.

// Intentionally no re-exports: use `crate::scip::*` directly.
