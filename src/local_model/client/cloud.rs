use crate::config::model::LocalModelConfig;
use crate::local_model::client::types::CompletionEndpoint;

/// Strip a pasted `ollama run ` CLI prefix from `ollama_cloud_model`.
pub(crate) fn normalize_ollama_cloud_model(raw: &str) -> &str {
    let trimmed = raw.trim();
    const PREFIX: &str = "ollama run ";
    if trimmed.len() >= PREFIX.len() && trimmed[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
        trimmed[PREFIX.len()..].trim()
    } else {
        trimmed
    }
}

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
            .is_some_and(|model| !normalize_ollama_cloud_model(model).is_empty())
}

pub fn ollama_cloud_endpoint<'a>(config: &'a LocalModelConfig) -> Option<CompletionEndpoint<'a>> {
    let base_url = config.ollama_cloud_url.as_deref()?.trim();
    let api_key = config.ollama_cloud_api_key.as_deref()?.trim();
    let model = normalize_ollama_cloud_model(config.ollama_cloud_model.as_deref()?);
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

#[cfg(test)]
mod tests {
    use super::normalize_ollama_cloud_model;

    #[test]
    fn normalize_ollama_cloud_model_strips_cli_prefix() {
        assert_eq!(
            normalize_ollama_cloud_model("ollama run glm-5.3:cloud"),
            "glm-5.3:cloud"
        );
        assert_eq!(
            normalize_ollama_cloud_model("  Ollama Run  glm-5.3:cloud  "),
            "glm-5.3:cloud"
        );
        assert_eq!(
            normalize_ollama_cloud_model("glm-5.3:cloud"),
            "glm-5.3:cloud"
        );
    }
}
