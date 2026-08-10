pub mod cpp;
pub mod go;
pub mod python;
pub mod rust;
pub mod types;
pub mod typescript;

pub use self::types::Language;
use crate::index::call_graph::CallEdge;
use crate::index::data_models::ExtractedModel;
use crate::index::observability::{ErrorHandlingPattern, LoggingPattern, TelemetryPattern};
use crate::index::routes::ExtractedRoute;
use crate::index::symbols::Symbol;
use miette::Result;
use std::path::Path;

/// C/C++ extensions sharing `Language::Cpp` / tree-sitter-cpp (D2).
const CPP_EXTS: &[&str] = &["c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "h++"];

fn is_cpp_ext(ext: &str) -> bool {
    CPP_EXTS.contains(&ext)
}

pub fn parse_symbols(path: &Path, content: &str) -> Result<Option<Vec<Symbol>>> {
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");

    match extension {
        "rs" => rust::extract_symbols(content),
        "ts" | "tsx" | "js" | "jsx" => typescript::extract_symbols(content, Some(path)),
        "py" => python::extract_symbols(content),
        "go" => go::extract_symbols(content),
        ext if is_cpp_ext(ext) => cpp::extract_symbols(content),
        _ => Ok(None),
    }
}

pub fn extract_calls(path: &Path, content: &str, symbols: &[Symbol]) -> Result<Vec<CallEdge>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => rust::extract_calls(path, content, symbols),
        Some("ts") | Some("tsx") => typescript::extract_calls(path, content, symbols),
        Some("py") => python::extract_calls(path, content, symbols),
        Some("go") => go::extract_calls(path, content, symbols),
        Some(ext) if is_cpp_ext(ext) => cpp::extract_calls(path, content, symbols),
        _ => Ok(Vec::new()),
    }
}

pub fn extract_routes(
    path: &Path,
    content: &str,
    symbols: &[Symbol],
) -> Result<Vec<ExtractedRoute>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => rust::extract_routes(content, symbols),
        Some("ts") | Some("tsx") => typescript::extract_routes(content, symbols),
        Some("py") => python::extract_routes(content, symbols),
        Some("go") => go::extract_routes(content, symbols),
        Some(ext) if is_cpp_ext(ext) => cpp::extract_routes(content, symbols),
        _ => Ok(Vec::new()),
    }
}

pub fn extract_data_models(
    path: &Path,
    content: &str,
    symbols: &[Symbol],
) -> Result<Vec<ExtractedModel>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => rust::extract_data_models(content, &path.to_string_lossy(), symbols),
        Some("ts") | Some("tsx") => {
            typescript::extract_data_models(content, &path.to_string_lossy(), symbols)
        }
        Some("py") => python::extract_data_models(content, &path.to_string_lossy(), symbols),
        Some("go") => go::extract_data_models(content, &path.to_string_lossy(), symbols),
        Some(ext) if is_cpp_ext(ext) => {
            cpp::extract_data_models(content, &path.to_string_lossy(), symbols)
        }
        _ => Ok(Vec::new()),
    }
}

pub fn extract_logging_patterns(path: &Path, content: &str) -> Result<Vec<LoggingPattern>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => rust::extract_logging_patterns(content),
        Some("ts") | Some("tsx") => typescript::extract_logging_patterns(content),
        Some("py") => python::extract_logging_patterns(content),
        Some("go") => go::extract_logging_patterns(content),
        Some(ext) if is_cpp_ext(ext) => cpp::extract_logging_patterns(content),
        _ => Ok(Vec::new()),
    }
}

pub fn extract_error_handling(path: &Path, content: &str) -> Result<Vec<ErrorHandlingPattern>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => rust::extract_error_handling(content),
        Some("ts") | Some("tsx") => typescript::extract_error_handling(content),
        Some("py") => python::extract_error_handling(content),
        Some("go") => go::extract_error_handling(content),
        Some(ext) if is_cpp_ext(ext) => cpp::extract_error_handling(content),
        _ => Ok(Vec::new()),
    }
}

pub fn extract_telemetry_patterns(path: &Path, content: &str) -> Result<Vec<TelemetryPattern>> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => rust::extract_telemetry_patterns(content),
        Some("ts") | Some("tsx") => typescript::extract_telemetry_patterns(content),
        Some("py") => python::extract_telemetry_patterns(content),
        Some("go") => go::extract_telemetry_patterns(content),
        Some(ext) if is_cpp_ext(ext) => cpp::extract_telemetry_patterns(content),
        _ => Ok(Vec::new()),
    }
}
