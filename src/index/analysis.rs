use crate::impact::packet::{AnalysisStatus, FileAnalysisStatus};
use crate::index::languages::{Language, parse_symbols};
use crate::index::metrics::ComplexityScorer;
use crate::index::references::extract_import_export;
use crate::index::runtime_usage::extract_runtime_usage;
use crate::util::fs::read_to_string_with_encoding;
use std::path::Path;

pub struct AnalysisOutcome {
    pub symbols: Option<Vec<crate::index::symbols::Symbol>>,
    pub imports: Option<crate::index::references::ImportExport>,
    pub runtime_usage: Option<crate::index::runtime_usage::RuntimeUsage>,
    pub analysis_status: FileAnalysisStatus,
    pub analysis_warnings: Vec<String>,
}

/// Analyzes a single file to extract symbols, imports, and runtime usage patterns.
/// This is the core logic for the indexing of individual files.
pub fn analyze_file(relative_path: &Path, base_dir: &Path) -> AnalysisOutcome {
    let full_path = base_dir.join(relative_path);
    let mut warnings = Vec::new();
    let mut status = FileAnalysisStatus::default();

    let Some(extension) = relative_path.extension().and_then(|ext| ext.to_str()) else {
        status.symbols = AnalysisStatus::Unsupported;
        status.imports = AnalysisStatus::Unsupported;
        status.runtime_usage = AnalysisStatus::Unsupported;
        warnings.push(format!(
            "{relative_path:?}: analysis unsupported for files without an extension"
        ));
        return AnalysisOutcome {
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: status,
            analysis_warnings: warnings,
        };
    };

    let supported = matches!(
        extension,
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "md"
    );
    if !supported {
        status.symbols = AnalysisStatus::Unsupported;
        status.imports = AnalysisStatus::Unsupported;
        status.runtime_usage = AnalysisStatus::Unsupported;
        warnings.push(format!(
            "{}: analysis unsupported for extension .{}",
            relative_path.display(),
            extension
        ));
        return AnalysisOutcome {
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: status,
            analysis_warnings: warnings,
        };
    }

    let content = match read_to_string_with_encoding(&full_path) {
        Ok(content) => content,
        Err(err) => {
            status.symbols = AnalysisStatus::ReadFailed;
            status.imports = AnalysisStatus::ReadFailed;
            status.runtime_usage = AnalysisStatus::ReadFailed;
            warnings.push(format!(
                "{}: failed to read file: {}",
                relative_path.display(),
                err
            ));
            return AnalysisOutcome {
                symbols: None,
                imports: None,
                runtime_usage: None,
                analysis_status: status,
                analysis_warnings: warnings,
            };
        }
    };

    let mut symbols = match parse_symbols(relative_path, &content) {
        Ok(symbols) => {
            status.symbols = AnalysisStatus::Ok;
            symbols
        }
        Err(err) => {
            status.symbols = AnalysisStatus::ExtractionFailed;
            warnings.push(format!(
                "{}: symbol extraction failed: {}",
                relative_path.display(),
                err
            ));
            None
        }
    };

    // Integrate Complexity Scoring
    if let (Some(syms), Some(lang)) = (&mut symbols, Language::from_extension(extension)) {
        let scorer = crate::index::metrics::NativeComplexityScorer::new();
        if let Some(path) = camino::Utf8Path::from_path(relative_path) {
            match scorer.score_file(path, &content, lang) {
                Ok(file_complexity) => {
                    for sym in syms {
                        if let Some(symbol_complexity) = file_complexity
                            .functions
                            .iter()
                            .find(|f| f.name == sym.name)
                        {
                            sym.cognitive_complexity = Some(symbol_complexity.cognitive as i32);
                            sym.cyclomatic_complexity = Some(symbol_complexity.cyclomatic as i32);
                        }
                    }
                }
                Err(e) => {
                    warnings.push(format!(
                        "{}: complexity scoring failed: {e}",
                        relative_path.display()
                    ));
                }
            }
        } else {
            warnings.push(format!(
                "{}: complexity scoring skipped: path is not valid UTF-8",
                relative_path.display()
            ));
        }
    }

    let imports = match extract_import_export(relative_path, &content) {
        Ok(imports) => {
            status.imports = AnalysisStatus::Ok;
            imports
        }
        Err(err) => {
            status.imports = AnalysisStatus::ExtractionFailed;
            warnings.push(format!(
                "{}: import/export extraction failed: {}",
                relative_path.display(),
                err
            ));
            None
        }
    };

    status.runtime_usage = AnalysisStatus::Ok;
    let runtime_usage = extract_runtime_usage(relative_path, &content);

    AnalysisOutcome {
        symbols,
        imports,
        runtime_usage,
        analysis_status: status,
        analysis_warnings: warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_analyze_file_marks_unsupported_extensions() {
        let tmp = tempdir().unwrap();
        let path = Path::new("notes.txt");

        let outcome = analyze_file(path, tmp.path());

        assert_eq!(outcome.analysis_status.symbols, AnalysisStatus::Unsupported);
        assert_eq!(outcome.analysis_status.imports, AnalysisStatus::Unsupported);
        assert_eq!(
            outcome.analysis_status.runtime_usage,
            AnalysisStatus::Unsupported
        );
        assert_eq!(outcome.analysis_warnings.len(), 1);
        assert!(outcome.analysis_warnings[0].contains("unsupported"));
    }

    #[test]
    fn test_analyze_file_marks_read_failures() {
        let tmp = tempdir().unwrap();
        let path = Path::new("missing.rs");

        let outcome = analyze_file(path, tmp.path());

        assert_eq!(outcome.analysis_status.symbols, AnalysisStatus::ReadFailed);
        assert_eq!(outcome.analysis_status.imports, AnalysisStatus::ReadFailed);
        assert_eq!(
            outcome.analysis_status.runtime_usage,
            AnalysisStatus::ReadFailed
        );
        assert_eq!(outcome.analysis_warnings.len(), 1);
        assert!(outcome.analysis_warnings[0].contains("failed to read file"));
    }
}
