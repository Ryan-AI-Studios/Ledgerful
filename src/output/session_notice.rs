//! Print-layer collapse + additive JSON `sessionNotices` for 0300.
//!
//! `SurfacesReport` / `classify_from_probes` stay unchanged; JSON injection
//! happens on a serialized `serde_json::Value` at emit time.

use crate::config::model::ServiceInferenceState;
use crate::state::cli_session::CliSession;
use serde_json::{Map, Value};

pub const ALREADY_SHOWN_HUMAN: &str = "Already shown this session.";
pub const ALREADY_SHOWN_JSON: &str = "already_shown";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionNoticeId {
    CoverageGlobal,
    CoverageServices,
    CoverageDeploy,
    ObservabilityEmpty,
}

impl SessionNoticeId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CoverageGlobal => "coverage.global",
            Self::CoverageServices => "coverage.services",
            Self::CoverageDeploy => "coverage.deploy",
            Self::ObservabilityEmpty => "observability.empty",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmptyNoticeOutcome {
    pub already_shown: bool,
    pub human: String,
    pub json: Value,
}

pub fn notice_id_for_services(state: ServiceInferenceState) -> Option<SessionNoticeId> {
    match state {
        ServiceInferenceState::DisabledGlobally => Some(SessionNoticeId::CoverageGlobal),
        ServiceInferenceState::DisabledForServices => Some(SessionNoticeId::CoverageServices),
        ServiceInferenceState::Enabled => None,
    }
}

pub fn notice_id_for_deploy(
    coverage_enabled: bool,
    deploy_enabled: bool,
) -> Option<SessionNoticeId> {
    if !coverage_enabled {
        Some(SessionNoticeId::CoverageGlobal)
    } else if !deploy_enabled {
        Some(SessionNoticeId::CoverageDeploy)
    } else {
        None
    }
}

pub fn notice_id_for_surface(id: &str, status: &str, gate: &str) -> Option<SessionNoticeId> {
    match (id, status, gate) {
        ("services" | "deploy", "gated", "coverage.global") => {
            Some(SessionNoticeId::CoverageGlobal)
        }
        ("services", "gated", "coverage.services") => Some(SessionNoticeId::CoverageServices),
        ("deploy", "gated", "coverage.deploy") => Some(SessionNoticeId::CoverageDeploy),
        ("observability", "empty", _) => Some(SessionNoticeId::ObservabilityEmpty),
        _ => None,
    }
}

pub fn apply_empty_notice(
    session: &mut CliSession,
    id: SessionNoticeId,
    full_human: &str,
    mut json: Value,
) -> EmptyNoticeOutcome {
    let already_shown = session.is_shown(id.as_str());
    let human = if already_shown {
        ALREADY_SHOWN_HUMAN.to_string()
    } else {
        full_human.to_string()
    };
    if already_shown {
        attach_session_notices(&mut json, [id]);
    }
    session.mark_shown(id.as_str());
    EmptyNoticeOutcome {
        already_shown,
        human,
        json,
    }
}

pub fn attach_session_notices(value: &mut Value, ids: impl IntoIterator<Item = SessionNoticeId>) {
    let mut ids: Vec<SessionNoticeId> = ids.into_iter().collect();
    ids.sort();
    ids.dedup();
    let mut map = Map::new();
    for id in ids {
        map.insert(
            id.as_str().to_string(),
            Value::String(ALREADY_SHOWN_JSON.to_string()),
        );
    }
    if map.is_empty() {
        return;
    }
    if let Value::Object(obj) = value {
        obj.insert("sessionNotices".to_string(), Value::Object(map));
    }
}

pub fn collapsed_next(already_shown: bool, full_next: &str) -> String {
    if already_shown {
        ALREADY_SHOWN_HUMAN.to_string()
    } else {
        full_next.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::surfaces::{SurfaceProbes, SurfaceStatus, classify_from_probes};
    use crate::state::cli_session::CliSession;
    use crate::state::layout::Layout;
    use chrono::Utc;
    use serde_json::json;
    use tempfile::tempdir;

    fn layout_tmp() -> (tempfile::TempDir, Layout) {
        let tmp = tempdir().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp path");
        let layout = Layout::from_roots(&root, root.join(".ledgerful"));
        (tmp, layout)
    }

    #[test]
    fn session_notice_surfaces_next_unchanged() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let report = classify_from_probes(&SurfaceProbes::default_mint());
        let mut session = CliSession::load(&layout, None, now);
        for s in &report.surfaces {
            if let Some(id) =
                notice_id_for_surface(s.id.as_str(), s.status.as_str(), s.gate.as_str())
            {
                session.mark_shown(id.as_str());
            }
        }
        session.persist();

        let session = CliSession::load(&layout, None, now);
        let mut already = Vec::new();
        for s in &report.surfaces {
            if let Some(id) =
                notice_id_for_surface(s.id.as_str(), s.status.as_str(), s.gate.as_str())
                && session.is_shown(id.as_str())
            {
                already.push(id);
            }
        }
        let mut value = serde_json::to_value(&report).expect("serialize");
        attach_session_notices(&mut value, already);
        assert_eq!(
            value["surfaces"][0]["next"],
            "ledgerful config set coverage.enabled=true"
        );
        assert_eq!(value["sessionNotices"]["coverage.global"], "already_shown");
        assert_eq!(
            value["surfaces"]
                .as_array()
                .expect("surfaces")
                .iter()
                .find(|s| s["id"] == "observability")
                .expect("obs")["next"],
            "add observability/ then ledgerful index --analyze-graph"
        );
        assert_eq!(
            value["sessionNotices"]["observability.empty"],
            "already_shown"
        );
    }

    #[test]
    fn session_notice_cross_command_services_then_surfaces() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let full = "config set coverage.enabled=true";
        let mut session = CliSession::load(&layout, None, now);
        apply_empty_notice(
            &mut session,
            SessionNoticeId::CoverageGlobal,
            full,
            json!({"schemaVersion": 1, "message": full}),
        );
        session.persist();

        let report = classify_from_probes(&SurfaceProbes::default_mint());
        let session = CliSession::load(&layout, None, now);
        let services = report
            .surfaces
            .iter()
            .find(|s| s.id == "services")
            .expect("services");
        let deploy = report
            .surfaces
            .iter()
            .find(|s| s.id == "deploy")
            .expect("deploy");
        let obs = report
            .surfaces
            .iter()
            .find(|s| s.id == "observability")
            .expect("obs");
        assert_eq!(services.status, SurfaceStatus::Gated);
        assert_eq!(
            collapsed_next(
                session.is_shown(SessionNoticeId::CoverageGlobal.as_str()),
                &services.next
            ),
            ALREADY_SHOWN_HUMAN
        );
        assert_eq!(
            collapsed_next(
                session.is_shown(SessionNoticeId::CoverageGlobal.as_str()),
                &deploy.next
            ),
            ALREADY_SHOWN_HUMAN
        );
        assert_eq!(
            collapsed_next(
                session.is_shown(SessionNoticeId::ObservabilityEmpty.as_str()),
                &obs.next
            ),
            obs.next
        );
    }

    fn envelope(message: &str) -> Value {
        json!({
            "schemaVersion": 1,
            "results": [],
            "resultCount": 0,
            "emptyReason": "disabledByConfig",
            "message": message,
        })
    }

    #[test]
    fn session_notice_second_human_services_collapses() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let full = "To enable, run: `ledgerful config set coverage.enabled=true`.";
        let mut session = CliSession::load(&layout, None, now);
        let first = apply_empty_notice(
            &mut session,
            SessionNoticeId::CoverageGlobal,
            full,
            envelope(full),
        );
        session.persist();
        assert!(first.human.contains("config set coverage.enabled=true"));
        assert!(!first.human.contains(ALREADY_SHOWN_HUMAN));

        let mut session = CliSession::load(&layout, None, now);
        let second = apply_empty_notice(
            &mut session,
            SessionNoticeId::CoverageGlobal,
            full,
            envelope(full),
        );
        assert!(second.human.contains(ALREADY_SHOWN_HUMAN));
        assert!(!second.human.contains("config set"));
    }

    #[test]
    fn session_notice_second_json_keeps_message() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let full = "To enable, run: `ledgerful config set coverage.enabled=true`.";
        let mut session = CliSession::load(&layout, None, now);
        let first = apply_empty_notice(
            &mut session,
            SessionNoticeId::CoverageGlobal,
            full,
            envelope(full),
        );
        session.persist();
        assert!(first.json.get("sessionNotices").is_none());
        assert_eq!(first.json["message"], full);
        assert_eq!(first.json["schemaVersion"], 1);

        let mut session = CliSession::load(&layout, None, now);
        let second = apply_empty_notice(
            &mut session,
            SessionNoticeId::CoverageGlobal,
            full,
            envelope(full),
        );
        assert_eq!(second.json["message"], full);
        assert_eq!(
            second.json["sessionNotices"]["coverage.global"],
            "already_shown"
        );
        assert_eq!(second.json["schemaVersion"], 1);
    }

    #[test]
    fn session_notice_deploy_impact_human_collapses() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let full = "To enable, run: `ledgerful config set coverage.enabled=true` (then `ledgerful config set coverage.deploy.enabled=true`).";
        let mut session = CliSession::load(&layout, None, now);
        apply_empty_notice(
            &mut session,
            SessionNoticeId::CoverageGlobal,
            full,
            envelope(full),
        );
        session.persist();
        let mut session = CliSession::load(&layout, None, now);
        let second = apply_empty_notice(
            &mut session,
            SessionNoticeId::CoverageGlobal,
            full,
            envelope(full),
        );
        assert!(second.human.contains(ALREADY_SHOWN_HUMAN));
        assert!(!second.human.contains("config set"));
    }

    #[test]
    fn session_notice_deploy_impact_json_keeps_message() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let full = "deploy disabled by coverage.enabled=false";
        let mut session = CliSession::load(&layout, None, now);
        apply_empty_notice(
            &mut session,
            SessionNoticeId::CoverageGlobal,
            full,
            envelope(full),
        );
        session.persist();
        let mut session = CliSession::load(&layout, None, now);
        let second = apply_empty_notice(
            &mut session,
            SessionNoticeId::CoverageGlobal,
            full,
            envelope(full),
        );
        assert_eq!(second.json["message"], full);
        assert_eq!(
            second.json["sessionNotices"]["coverage.global"],
            "already_shown"
        );
    }

    #[test]
    fn session_notice_observability_coverage_human_collapses() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let full = "add them under 'observability/' and run `ledgerful index --analyze-graph`";
        let mut session = CliSession::load(&layout, None, now);
        apply_empty_notice(
            &mut session,
            SessionNoticeId::ObservabilityEmpty,
            full,
            envelope(full),
        );
        session.persist();
        let mut session = CliSession::load(&layout, None, now);
        let second = apply_empty_notice(
            &mut session,
            SessionNoticeId::ObservabilityEmpty,
            full,
            envelope(full),
        );
        assert!(second.human.contains(ALREADY_SHOWN_HUMAN));
        assert!(!second.human.contains("index --analyze-graph"));
        assert!(session.is_shown(SessionNoticeId::ObservabilityEmpty.as_str()));
    }

    #[test]
    fn session_notice_observability_coverage_json_keeps_message() {
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let full = "run `ledgerful index --analyze-graph` to populate this surface.";
        let mut session = CliSession::load(&layout, None, now);
        apply_empty_notice(
            &mut session,
            SessionNoticeId::ObservabilityEmpty,
            full,
            envelope(full),
        );
        session.persist();
        let mut session = CliSession::load(&layout, None, now);
        let second = apply_empty_notice(
            &mut session,
            SessionNoticeId::ObservabilityEmpty,
            full,
            envelope(full),
        );
        assert_eq!(second.json["message"], full);
        assert_eq!(
            second.json["sessionNotices"]["observability.empty"],
            "already_shown"
        );
    }

    #[test]
    fn session_notice_services_specific_gate_collapses() {
        assert_eq!(
            notice_id_for_services(ServiceInferenceState::DisabledForServices),
            Some(SessionNoticeId::CoverageServices)
        );
        assert_eq!(
            notice_id_for_services(ServiceInferenceState::DisabledGlobally),
            Some(SessionNoticeId::CoverageGlobal)
        );
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let mut session = CliSession::load(&layout, None, now);
        session.mark_shown(SessionNoticeId::CoverageGlobal.as_str());
        session.persist();
        let session = CliSession::load(&layout, None, now);
        assert!(!session.is_shown(SessionNoticeId::CoverageServices.as_str()));
        assert!(session.is_shown(SessionNoticeId::CoverageGlobal.as_str()));
    }

    #[test]
    fn session_notice_deploy_specific_gate_collapses() {
        assert_eq!(
            notice_id_for_deploy(true, false),
            Some(SessionNoticeId::CoverageDeploy)
        );
        assert_eq!(
            notice_id_for_deploy(false, true),
            Some(SessionNoticeId::CoverageGlobal)
        );
        let (_tmp, layout) = layout_tmp();
        let now = Utc::now();
        let mut session = CliSession::load(&layout, None, now);
        session.mark_shown(SessionNoticeId::CoverageGlobal.as_str());
        session.persist();
        let session = CliSession::load(&layout, None, now);
        assert!(!session.is_shown(SessionNoticeId::CoverageDeploy.as_str()));
    }

    #[test]
    fn attach_session_notices_skips_empty() {
        let mut v = envelope("x");
        attach_session_notices(&mut v, [] as [SessionNoticeId; 0]);
        assert!(v.get("sessionNotices").is_none());
    }
}
