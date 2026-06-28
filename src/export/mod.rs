//! SOC2 evidence export pipeline.
//!
//! Gated behind the `web` cargo feature: the export is served by the web
//! dashboard's `/api/compliance/export` endpoint and depends on the `zip`
//! crate, which is only available under `web` (and `sync`).

pub mod soc2;
