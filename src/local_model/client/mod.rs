mod cloud;
mod complete;
mod completion_text;
mod fallback_error;
mod gemini;
mod ollama;
mod openai;
mod types;
mod util;

pub use cloud::has_ollama_cloud_fallback;
#[cfg(test)]
pub(crate) use complete::cloud_fallback_env;
pub(crate) use complete::ping_completions_detailed;
pub use complete::{
    DEFAULT_CLOUD_FALLBACK_TIMEOUT_SECS, HARD_DEADLINE_BUFFER_SECS, LOCAL_TCP_PRECHECK_CAP_SECS,
    complete, complete_with_first_byte_timeout, complete_with_hard_deadline,
    configured_cloud_arm_count, hard_deadline_secs, has_cloud_fallback,
    has_cloud_fallback_credentials, is_configured, is_first_byte_timeout_error, ping_completions,
};
pub use fallback_error::{
    compact_completion_error, format_compact_report, format_full_report,
    is_multi_cause_fallback_error, local_cause_is_timeout, sanitize_cause,
};
pub use gemini::{gemini_complete, gemini_complete_unsanitized};
pub use types::{ChatMessage, CompletionOptions, EndpointKind, EndpointTarget};
pub use util::{
    check_base_url_warnings, completion_target, detect_endpoint_kind, transport_is_timeout,
};

#[cfg(test)]
mod tests;
