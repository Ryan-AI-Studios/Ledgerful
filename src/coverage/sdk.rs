use crate::impact::packet::{ChangedFile, SdkDependency, SdkDependencyDelta};
use crate::index::references::extract_import_export;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

pub fn detect_sdk_changes(
    changed_files: &[ChangedFile],
    patterns: &[String],
    repo_root: &Path,
) -> SdkDependencyDelta {
    let mut delta = SdkDependencyDelta::default();
    let lower_patterns: Vec<String> = patterns.iter().map(|p| p.to_lowercase()).collect();

    for file in changed_files {
        let current_imports = if let Some(ref imports) = file.imports {
            imports.imported_from.clone()
        } else {
            if let Ok(content) = std::fs::read_to_string(repo_root.join(&file.path)) {
                if let Ok(Some(extracted)) = extract_import_export(&file.path, &content) {
                    extracted.imported_from
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        };

        let previous_imports = if file.status == "Added" {
            Vec::new()
        } else {
            let path_to_show = file.old_path.as_ref().unwrap_or(&file.path);
            if let Some(content) = get_git_content(repo_root, path_to_show) {
                if let Ok(Some(extracted)) = extract_import_export(path_to_show, &content) {
                    extracted.imported_from
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        };

        let current_sdk_imports: Vec<String> = current_imports
            .iter()
            .filter(|imp| {
                let imp_lower = imp.to_lowercase();
                lower_patterns.iter().any(|pat| imp_lower.contains(pat))
            })
            .cloned()
            .collect();

        let previous_sdk_imports: Vec<String> = previous_imports
            .iter()
            .filter(|imp| {
                let imp_lower = imp.to_lowercase();
                lower_patterns.iter().any(|pat| imp_lower.contains(pat))
            })
            .cloned()
            .collect();

        // Added
        for imp in &current_sdk_imports {
            if !previous_sdk_imports.contains(imp) {
                delta.added.push(SdkDependency {
                    sdk_name: find_sdk_name(imp, &lower_patterns),
                    file_path: file.path.clone(),
                    import_statement: imp.clone(),
                });
            }
        }

        // Removed
        for imp in &previous_sdk_imports {
            if !current_sdk_imports.contains(imp) {
                delta.removed.push(SdkDependency {
                    sdk_name: find_sdk_name(imp, &lower_patterns),
                    file_path: file.path.clone(),
                    import_statement: imp.clone(),
                });
            }
        }

        // Modified (simplified: if SDK was there and file changed)
        if file.status == "Modified" {
            for imp in &current_sdk_imports {
                if previous_sdk_imports.contains(imp) {
                    delta.modified.push(SdkDependency {
                        sdk_name: find_sdk_name(imp, &lower_patterns),
                        file_path: file.path.clone(),
                        import_statement: imp.clone(),
                    });
                }
            }
        }
    }

    // Sort for determinism
    delta.added.sort();
    delta.removed.sort();
    delta.modified.sort();

    delta
}

fn find_sdk_name(import: &str, lower_patterns: &[String]) -> String {
    let imp_lower = import.to_lowercase();
    for pat in lower_patterns {
        if imp_lower.contains(pat) {
            return pat.clone();
        }
    }
    "unknown".to_string()
}

fn get_git_content(repo_root: &Path, rel_path: &Path) -> Option<String> {
    let path_str = rel_path.to_string_lossy().replace('\\', "/");
    let output = std::process::Command::new("git")
        .args(["show", &format!("HEAD:{}", path_str)])
        .current_dir(repo_root)
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::ChangedFile;
    use crate::index::references::ImportExport;

    #[test]
    fn test_find_sdk_name() {
        let patterns = ["stripe".to_string(), "auth0".to_string()];
        let lower_patterns: Vec<String> = patterns.iter().map(|p| p.to_lowercase()).collect();
        assert_eq!(
            find_sdk_name("use stripe::Charge", &lower_patterns),
            "stripe"
        );
        assert_eq!(
            find_sdk_name("import { Auth0 } from 'auth0'", &lower_patterns),
            "auth0"
        );
    }

    #[test]
    fn test_sdk_dependency_delta_default() {
        let delta = SdkDependencyDelta::default();
        assert!(delta.added.is_empty());
        assert!(delta.removed.is_empty());
        assert!(delta.modified.is_empty());
    }

    #[test]
    fn test_detect_sdk_changes_rust() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("src/main.rs"),
            status: "Added".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: Some(ImportExport {
                imported_from: vec!["stripe::Charge".to_string()],
                exported_symbols: vec![],
            }),
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["stripe".to_string()];
        let delta = detect_sdk_changes(&changed_files, &patterns, Path::new("."));
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].sdk_name, "stripe");
        assert_eq!(delta.added[0].import_statement, "stripe::Charge");
    }

    #[test]
    fn test_detect_sdk_changes_python() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("app.py"),
            status: "Added".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: Some(ImportExport {
                imported_from: vec!["stripe".to_string()],
                exported_symbols: vec![],
            }),
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["stripe".to_string()];
        let delta = detect_sdk_changes(&changed_files, &patterns, Path::new("."));
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].sdk_name, "stripe");
    }

    #[test]
    fn test_detect_sdk_changes_js() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("app.js"),
            status: "Added".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: Some(ImportExport {
                imported_from: vec!["stripe".to_string()],
                exported_symbols: vec![],
            }),
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["stripe".to_string()];
        let delta = detect_sdk_changes(&changed_files, &patterns, Path::new("."));
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].sdk_name, "stripe");
    }

    #[test]
    fn test_detect_sdk_changes_go() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("main.go"),
            status: "Added".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: Some(ImportExport {
                imported_from: vec!["github.com/stripe/stripe-go".to_string()],
                exported_symbols: vec![],
            }),
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["stripe".to_string()];
        let delta = detect_sdk_changes(&changed_files, &patterns, Path::new("."));
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].sdk_name, "stripe");
        assert_eq!(
            delta.added[0].import_statement,
            "github.com/stripe/stripe-go"
        );
    }

    #[test]
    fn test_detect_sdk_changes_case_insensitive() {
        let changed_files = vec![ChangedFile {
            path: PathBuf::from("src/main.rs"),
            status: "Added".to_string(),
            old_path: None,
            is_staged: true,
            symbols: None,
            imports: Some(ImportExport {
                imported_from: vec!["STRIPE::Charge".to_string()],
                exported_symbols: vec![],
            }),
            runtime_usage: None,
            analysis_status: Default::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }];
        let patterns = vec!["Stripe".to_string()];
        let delta = detect_sdk_changes(&changed_files, &patterns, Path::new("."));
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added[0].sdk_name, "stripe");
    }
}
