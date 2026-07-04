//! API-specific DTOs not already moved to `web::types`.
//!
//! All frontend-facing response/query types live in `crate::commands::web::types`
//! (Phase 0.5). This module currently has no additional API-specific DTOs and
//! re-exports the shared ones for internal consistency.

pub use crate::commands::web::types::*;
