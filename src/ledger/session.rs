use std::sync::OnceLock;
use uuid::Uuid;

static SESSION_ID: OnceLock<String> = OnceLock::new();

/// Returns a stable session ID for the current process.
pub fn get_session_id() -> &'static str {
    SESSION_ID.get_or_init(|| Uuid::new_v4().to_string())
}
