use crate::daemon::state::ReadOnlyStorage;
use crate::index::languages::types::Language;
use crate::index::metrics::{ComplexityScorer, NativeComplexityScorer};
use crate::output::lsp::map_impact_to_diagnostics;
use camino::Utf8Path;
use std::path::PathBuf;
use std::sync::Arc;
use tower_lsp_server::Client;
use tower_lsp_server::ls_types::*;
use tracing::warn;

pub struct LspHandlers {
    client: Client,
    storage: Arc<ReadOnlyStorage>,
}

impl LspHandlers {
    pub fn new(client: Client, storage: Arc<ReadOnlyStorage>) -> Self {
        Self { client, storage }
    }

    pub async fn on_open(&self, params: DidOpenTextDocumentParams) {
        self.trigger_analysis(params.text_document.uri, Some(params.text_document.text))
            .await;
    }

    pub async fn on_change(&self, params: DidChangeTextDocumentParams) {
        let text = params.content_changes.first().map(|c| c.text.clone());
        self.trigger_analysis(params.text_document.uri, text).await;
    }

    pub async fn on_save(&self, params: DidSaveTextDocumentParams) {
        self.trigger_analysis(params.text_document.uri, params.text)
            .await;
    }

    async fn trigger_analysis(&self, uri: Uri, content: Option<String>) {
        let Some(path) = uri_to_path(&uri) else {
            return;
        };

        let mut all_diagnostics = Vec::new();
        let mut data_stale = false;

        match self.storage.get_latest_packet() {
            Ok(result) => {
                data_stale = result.data_stale;
                if let Some(packet) = result.data {
                    let diagnostics_map = map_impact_to_diagnostics(&packet);
                    if let Some(diagnostics) = diagnostics_map.get(&path) {
                        all_diagnostics.extend(diagnostics.clone());
                    }
                }
            }
            Err(e) => warn!("Failed to get latest packet for diagnostics: {e}"),
        }

        if let Some(text) = content
            && let Some(ext) = path.extension().and_then(|e| e.to_str())
            && let Some(lang) = Language::from_extension(ext)
            && let Some(utf8_path) = Utf8Path::from_path(&path)
        {
            let scorer = NativeComplexityScorer::new();
            match scorer.score_file(utf8_path, &text, lang) {
                Ok(file_complexity) => {
                    for func in file_complexity.functions {
                        if func.cognitive > 10 {
                            all_diagnostics.push(Diagnostic {
                                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                                severity: Some(DiagnosticSeverity::WARNING),
                                code: Some(NumberOrString::String(
                                    "high-cognitive-complexity".to_string(),
                                )),
                                source: Some("Ledgerful".to_string()),
                                message: format!(
                                    "Function '{}' has high cognitive complexity: {}",
                                    func.name, func.cognitive
                                ),
                                ..Default::default()
                            });
                        }
                    }
                }
                Err(e) => warn!("Real-time complexity analysis failed: {e}"),
            }
        }

        if data_stale {
            all_diagnostics.push(Diagnostic {
                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                severity: Some(DiagnosticSeverity::WARNING),
                code: Some(NumberOrString::String("data-stale".to_string())),
                source: Some("Ledgerful".to_string()),
                message: "Ledgerful data is stale (database busy/locked).".to_string(),
                ..Default::default()
            });
        }

        self.client
            .publish_diagnostics(uri, all_diagnostics, None)
            .await;
    }

    pub async fn on_hover(
        &self,
        params: HoverParams,
    ) -> tower_lsp_server::jsonrpc::Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let Some(path) = uri_to_path(&uri) else {
            return Ok(None);
        };

        match self.storage.get_latest_packet() {
            Ok(result) => {
                let Some(packet) = result.data else {
                    return Ok(None);
                };

                let mut hover_text = Vec::new();
                hover_text.push(format!(
                    "### Ledgerful Impact: {}",
                    path.file_name().and_then(|f| f.to_str()).unwrap_or("File")
                ));
                hover_text.push(format!("**Global Risk Level**: {:?}", packet.risk_level));

                if !packet.risk_reasons.is_empty() {
                    hover_text.push(format!(
                        "**Risk Reasons**: {}",
                        packet.risk_reasons.join(", ")
                    ));
                }

                if let Some(file_change) = packet.changes.iter().find(|c| c.path == path) {
                    hover_text.push(format!("**Status**: {}", file_change.status));
                    if !file_change.analysis_warnings.is_empty() {
                        hover_text.push("**Warnings**:".to_string());
                        for warning in &file_change.analysis_warnings {
                            hover_text.push(format!("- {warning}"));
                        }
                    }
                }

                let couplings: Vec<_> = packet
                    .temporal_couplings
                    .iter()
                    .filter(|c| c.file_a == path || c.file_b == path)
                    .collect();

                if !couplings.is_empty() {
                    hover_text.push("**Temporal Couplings**:".to_string());
                    for coupling in couplings {
                        let other = if coupling.file_a == path {
                            &coupling.file_b
                        } else {
                            &coupling.file_a
                        };
                        hover_text.push(format!(
                            "- {} (Score: {:.2})",
                            other.display(),
                            coupling.score
                        ));
                    }
                }

                if result.data_stale {
                    hover_text.push(
                        "\n*Warning: data may be stale due to database contention.*".to_string(),
                    );
                }

                Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(hover_text.join("\n\n"))),
                    range: None,
                }))
            }
            Err(_) => Ok(None),
        }
    }

    pub async fn on_code_lens(
        &self,
        params: CodeLensParams,
    ) -> tower_lsp_server::jsonrpc::Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let Some(path) = uri_to_path(&uri) else {
            return Ok(None);
        };

        match self.storage.get_latest_packet() {
            Ok(result) => {
                let Some(packet) = result.data else {
                    return Ok(None);
                };

                let mut lenses = vec![CodeLens {
                    range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                    command: Some(Command {
                        title: format!("Risk: {:?}", packet.risk_level),
                        command: "".to_string(),
                        arguments: None,
                    }),
                    data: None,
                }];

                if let Some(file_change) = packet.changes.iter().find(|c| c.path == path)
                    && let Some(symbols) = &file_change.symbols
                {
                    let max_complexity = symbols
                        .iter()
                        .filter_map(|s| s.cognitive_complexity.or(s.cyclomatic_complexity))
                        .max();

                    if let Some(complexity) = max_complexity {
                        let score = normalized_complexity_score(complexity);
                        lenses.push(CodeLens {
                            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                            command: Some(Command {
                                title: format!(
                                    "Complexity: {:.2} ({})",
                                    score,
                                    complexity_category(score)
                                ),
                                command: "".to_string(),
                                arguments: None,
                            }),
                            data: None,
                        });
                    }
                }

                if result.data_stale {
                    lenses.push(CodeLens {
                        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                        command: Some(Command {
                            title: "Ledgerful data stale: database busy".to_string(),
                            command: "".to_string(),
                            arguments: None,
                        }),
                        data: None,
                    });
                }

                Ok(Some(lenses))
            }
            Err(_) => Ok(None),
        }
    }
}

pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    uri.to_file_path().map(|path| path.as_ref().to_path_buf())
}

fn normalized_complexity_score(complexity: i32) -> f32 {
    (complexity.max(0) as f32 / 50.0).min(1.0) * 100.0
}

fn complexity_category(score: f32) -> &'static str {
    if score >= 70.0 {
        "High"
    } else if score >= 35.0 {
        "Medium"
    } else {
        "Low"
    }
}
