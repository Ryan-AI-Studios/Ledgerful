use serde::Deserialize;

/// Ollama native `/api/chat` response (stream=false).
#[derive(Debug, Deserialize)]
pub struct OllamaChatResponse {
    pub message: OllamaChatMessage,
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
        assert_eq!(ollama_native_num_predict(4096), 1024);
    }
}
