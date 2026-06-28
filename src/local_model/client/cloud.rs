use crate::config::model::LocalModelConfig;
use crate::local_model::client::types::CompletionEndpoint;

pub fn has_ollama_cloud_fallback(config: &LocalModelConfig) -> bool {
    config
        .ollama_cloud_url
        .as_deref()
        .is_some_and(|url| !url.trim().is_empty())
        && config
            .ollama_cloud_api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
        && config
            .ollama_cloud_model
            .as_deref()
            .is_some_and(|model| !model.trim().is_empty())
}

pub fn ollama_cloud_endpoint<'a>(config: &'a LocalModelConfig) -> Option<CompletionEndpoint<'a>> {
    let base_url = config.ollama_cloud_url.as_deref()?.trim();
    let api_key = config.ollama_cloud_api_key.as_deref()?.trim();
    let model = config.ollama_cloud_model.as_deref()?.trim();
    if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
        return None;
    }
    Some(CompletionEndpoint {
        label: "Ollama Cloud fallback",
        base_url,
        model,
        authorization: Some(format!("Bearer {api_key}")),
    })
}
