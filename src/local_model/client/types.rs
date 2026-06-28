use serde::Serialize;

/// Determines how the completion endpoint should be called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    /// POST /v1/chat/completions (OpenAI-compatible JSON body)
    OpenAICompatible,
    /// POST /api/chat (Ollama native JSON body with model/messages)
    OllamaNative,
}

#[derive(Debug, Clone)]
pub struct EndpointTarget {
    pub kind: EndpointKind,
    pub url: String,
}

pub struct CompletionEndpoint<'a> {
    pub label: &'a str,
    pub base_url: &'a str,
    pub model: &'a str,
    pub authorization: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct CompletionOptions {
    pub max_tokens: usize,
    pub temperature: f32,
}

impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            max_tokens: 4096,
            temperature: 0.7,
        }
    }
}

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ChoiceMessage {
    #[serde(default)]
    pub content: String,
    #[serde(default, alias = "reasoning_content")]
    pub reasoning: Option<String>,
}
