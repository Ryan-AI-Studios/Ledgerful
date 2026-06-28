use crate::index::references::ImportExport;
use crate::index::runtime_usage::RuntimeUsage;
use crate::index::symbols::Symbol;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisStatus {
    #[default]
    NotRun,
    Ok,
    Unsupported,
    ReadFailed,
    ExtractionFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileAnalysisStatus {
    pub symbols: AnalysisStatus,
    pub imports: AnalysisStatus,
    pub runtime_usage: AnalysisStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: PathBuf,
    pub status: String, // e.g., "Added", "Modified", "Deleted", "Renamed"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<PathBuf>,
    pub is_staged: bool,
    pub symbols: Option<Vec<Symbol>>,
    pub imports: Option<ImportExport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_usage: Option<RuntimeUsage>,
    #[serde(default)]
    pub analysis_status: FileAnalysisStatus,
    #[serde(default)]
    pub analysis_warnings: Vec<String>,
    #[serde(default)]
    pub api_routes: Vec<super::ApiRoute>,
    #[serde(default)]
    pub data_models: Vec<super::DataModel>,
    #[serde(default)]
    pub ci_gates: Vec<super::CIGate>,
}
