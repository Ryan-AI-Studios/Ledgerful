//! Agent session briefing (track 0224).
//!
//! Composes git identity, ledger pending/collisions, doctor sidecar counts,
//! budgeted change-context (`max_files=5`), bounded non-test hotspots, and an
//! honest `impactCache` HEAD comparator. Never writes `latest-impact.json`.

mod build;
mod emit;
mod packet;

pub use build::build_session;
pub use packet::{
    SESSION_DIRTY_PATH_CAP, SESSION_HOTSPOT_COMMITS_CAP, SESSION_HOTSPOT_DAYS,
    SESSION_HOTSPOT_LIMIT, SESSION_KIND, SESSION_MAX_FILES, SESSION_SCHEMA_VERSION,
    SessionEnvelope, cap_dirty_paths, classify_impact_cache,
};

use crate::commands::change_context::open_storage_for_change_context;
use crate::config::checklist::apply_checklist_cookie;
use crate::state::cli_session::{CliSession, env_session_id};
use chrono::Utc;
use miette::Result;

/// CLI entrypoint: resolve layout/storage/config, build envelope, print human or JSON.
///
/// Layout and storage failures are hard errors (not a zeroed success envelope).
/// Cookie persist is catalog-emit only: `--json` may write; human `session` does not.
pub fn execute_session(json: bool) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()
        .map_err(|e| miette::miette!("session: layout unavailable: {e}"))?;
    let config = crate::config::load::load_config(&layout).unwrap_or_default();
    let storage = match open_storage_for_change_context(&layout) {
        Ok(s) => s,
        Err((e, _)) => {
            return Err(miette::miette!("session: storage unavailable: {e}"));
        }
    };

    let envelope = build_session(&layout, &storage, &config)?;
    let _ = storage.shutdown();
    emit::emit_session(&envelope, json)?;
    if json {
        let mut cli_session = CliSession::load(&layout, env_session_id().as_deref(), Utc::now());
        apply_checklist_cookie(&mut cli_session, &envelope.config_checklist);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
