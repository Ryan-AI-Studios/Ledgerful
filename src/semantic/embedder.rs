use crate::config::model::LocalModelConfig;
use crate::embed::client::{
    MAX_BATCH_SIZE, embed_batch, embed_long_text, is_embedding_backend_configured,
};
use miette::Result;

/// Error message when semantic embed is attempted without a configured backend.
/// Named so tests and call sites can assert the user-facing guidance.
pub const EMBEDDING_NOT_CONFIGURED_MSG: &str = "Embedding backend not configured. Set `local_model.base_url` (or `local_model.embedding_url`) \
     to your embedding server URL before running semantic index or search. \
     Inspect with `ledgerful index --semantic-dry-run`.";

pub struct SemanticEmbedder {
    config: LocalModelConfig,
}

impl SemanticEmbedder {
    pub fn new(config: LocalModelConfig) -> Self {
        Self { config }
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        if !is_embedding_backend_configured(&self.config) {
            return Err(miette::miette!("{}", EMBEDDING_NOT_CONFIGURED_MSG));
        }
        match embed_long_text(&self.config, text) {
            Ok(v) => Ok(v),
            Err(e) => Err(miette::miette!(e)),
        }
    }

    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if !is_embedding_backend_configured(&self.config) {
            return Err(miette::miette!("{}", EMBEDDING_NOT_CONFIGURED_MSG));
        }

        let mut all_vectors = Vec::with_capacity(texts.len());
        let url = self
            .config
            .embedding_url
            .as_deref()
            .unwrap_or(&self.config.base_url);

        for chunk in texts.chunks(MAX_BATCH_SIZE) {
            let batch_vectors = match embed_batch(
                url,
                &self.config.embedding_model,
                chunk,
                self.config.timeout_secs,
            ) {
                Ok(vecs) if vecs.len() == chunk.len() => vecs,
                Ok(vecs) if vecs.is_empty() => {
                    return Err(miette::miette!(
                        "Embedding backend returned no vectors for a non-empty batch"
                    ));
                }
                Ok(vecs) => {
                    return Err(miette::miette!(
                        "Embedding backend returned {} vectors for {} texts",
                        vecs.len(),
                        chunk.len()
                    ));
                }
                Err(e) => return Err(miette::miette!(e)),
            };
            all_vectors.extend(batch_vectors);
        }
        Ok(all_vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::LocalModelConfig;

    #[test]
    fn embed_not_configured_returns_error_naming_config_key() {
        let embedder = SemanticEmbedder::new(LocalModelConfig::default());
        let err = embedder.embed("hello").expect_err("must refuse");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("local_model.base_url"),
            "error must name config key, got: {msg}"
        );
        assert!(
            msg.contains("not configured") || msg.contains("Not configured"),
            "error must state not configured, got: {msg}"
        );
    }

    #[test]
    fn embed_batch_not_configured_returns_error_naming_config_key() {
        let embedder = SemanticEmbedder::new(LocalModelConfig {
            embedding_model: "nomic-embed-text".to_string(),
            dimensions: 768,
            ..Default::default()
        });
        let err = embedder
            .embed_batch(&["a", "b"])
            .expect_err("partial config (model name only) must refuse");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("local_model.base_url"),
            "error must name config key, got: {msg}"
        );
    }

    #[test]
    fn embed_not_configured_does_not_return_zero_vector() {
        let embedder = SemanticEmbedder::new(LocalModelConfig {
            dimensions: 768,
            ..Default::default()
        });
        // DoD-1: never fabricate zeros
        assert!(embedder.embed("x").is_err());
    }
}
