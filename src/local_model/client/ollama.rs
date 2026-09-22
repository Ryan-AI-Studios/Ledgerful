use serde::Deserialize;

/// Ollama native `/api/chat` response (stream=false).
#[derive(Debug, Deserialize)]
pub struct OllamaChatResponse {
    pub message: OllamaChatMessage,
    /// Why generation stopped (`stop`, `length`, `load`, `unload`). Absent on older servers.
    #[serde(default)]
    pub done_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OllamaChatMessage {
    pub content: String,
    #[serde(default)]
    pub thinking: Option<String>,
}

pub fn ollama_native_num_predict(max_tokens: usize) -> usize {
    max_tokens.clamp(1, 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ollama_native_num_predict_is_bounded() {
        assert_eq!(ollama_native_num_predict(0), 1);
        assert_eq!(ollama_native_num_predict(64), 64);
        assert_eq!(ollama_native_num_predict(1024), 1024);
        assert_eq!(ollama_native_num_predict(4096), 1024);
    }

    #[test]
    fn is_length_stop_ollama_done_reason_length_stop_and_omitted() {
        let length: OllamaChatResponse = serde_json::from_str(
            r#"{"message":{"content":"abc"},"done":true,"done_reason":"length"}"#,
        )
        .unwrap();
        assert_eq!(length.done_reason.as_deref(), Some("length"));
        assert!(super::super::is_length_stop(length.done_reason.as_deref()));

        let stop: OllamaChatResponse = serde_json::from_str(
            r#"{"message":{"content":"abc"},"done":true,"done_reason":"stop"}"#,
        )
        .unwrap();
        assert!(!super::super::is_length_stop(stop.done_reason.as_deref()));

        let omitted: OllamaChatResponse =
            serde_json::from_str(r#"{"message":{"content":"abc"},"done":true}"#).unwrap();
        assert_eq!(omitted.done_reason, None);
        assert!(!super::super::is_length_stop(
            omitted.done_reason.as_deref()
        ));

        let null_reason: OllamaChatResponse =
            serde_json::from_str(r#"{"message":{"content":"abc"},"done":true,"done_reason":null}"#)
                .unwrap();
        assert_eq!(null_reason.done_reason, None);
    }
}
