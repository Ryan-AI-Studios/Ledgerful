use ledgerful::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket, TemporalCoupling};
use ledgerful::index::references::ImportExport;
use ledgerful::verify::predict::{PredictionReason, Predictor};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[test]
fn test_structural_prediction() {
    let mut current = ImpactPacket::default();
    current.changes.push(ChangedFile {
        path: PathBuf::from("src/models/user.rs"),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: true,

        symbols: None,
        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    });

    let mut history = ImpactPacket::default();
    history.changes.push(ChangedFile {
        path: PathBuf::from("src/handlers/auth.rs"),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: false,

        symbols: None,
        imports: Some(ImportExport {
            imported_from: vec!["src/models/user.rs".to_string()],
            exported_symbols: vec![],
        }),
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    });

    let result = Predictor::predict(&current, &[history]);

    assert_eq!(result.files.len(), 1);
    assert_eq!(result.files[0].path, PathBuf::from("src/handlers/auth.rs"));
    assert_eq!(result.files[0].reason, PredictionReason::Structural);
}

#[test]
fn test_temporal_prediction() {
    let mut current = ImpactPacket::default();
    current.changes.push(ChangedFile {
        path: PathBuf::from("src/a.rs"),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: true,

        symbols: None,
        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    });
    current.temporal_couplings.push(TemporalCoupling {
        file_a: PathBuf::from("src/a.rs"),
        file_b: PathBuf::from("src/b.rs"),
        score: 0.9,
    });

    let result = Predictor::predict(&current, &[]);

    assert_eq!(result.files.len(), 1);
    assert_eq!(result.files[0].path, PathBuf::from("src/b.rs"));
    assert_eq!(result.files[0].reason, PredictionReason::Temporal);
}

#[test]
fn test_deduplication_and_sorting() {
    let mut current = ImpactPacket::default();
    current.changes.push(ChangedFile {
        path: PathBuf::from("src/a.rs"),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: true,

        symbols: None,
        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    });
    // Temporal predicts B
    current.temporal_couplings.push(TemporalCoupling {
        file_a: PathBuf::from("src/a.rs"),
        file_b: PathBuf::from("src/b.rs"),
        score: 0.9,
    });

    // Structural also predicts B
    let mut history = ImpactPacket::default();
    history.changes.push(ChangedFile {
        path: PathBuf::from("src/b.rs"),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: false,

        symbols: None,
        imports: Some(ImportExport {
            imported_from: vec!["src/a.rs".to_string()],
            exported_symbols: vec![],
        }),
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    });

    let result = Predictor::predict(&current, &[history]);

    assert_eq!(result.files.len(), 2);
    assert_eq!(result.files[0].path, PathBuf::from("src/b.rs"));
    assert_eq!(result.files[1].path, PathBuf::from("src/b.rs"));
}

#[test]
fn test_current_imports_take_part_in_structural_prediction() {
    let mut current = ImpactPacket::default();
    current.changes.push(ChangedFile {
        path: PathBuf::from("src/models/user.rs"),
        status: "Modified".to_string(),
        old_path: None,
        is_staged: true,

        symbols: None,
        imports: None,
        runtime_usage: None,
        analysis_status: FileAnalysisStatus::default(),
        analysis_warnings: Vec::new(),
        api_routes: Vec::new(),
        data_models: Vec::new(),
        ci_gates: Vec::new(),
    });

    let mut imports = BTreeMap::new();
    imports.insert(
        PathBuf::from("src/handlers/auth.rs"),
        ImportExport {
            imported_from: vec!["src/models/user.rs".to_string()],
            exported_symbols: vec![],
        },
    );

    let result = Predictor::predict_with_current_imports(&current, &[], &imports);

    assert_eq!(result.files.len(), 1);
    assert_eq!(result.files[0].path, PathBuf::from("src/handlers/auth.rs"));
    assert_eq!(result.files[0].reason, PredictionReason::Structural);
}
