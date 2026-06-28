use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const SOURCE_PATTERNS: &[&str] = &["src", "lib", "app", "pkg", "internal"];
const TEST_PATTERNS: &[&str] = &["tests", "test", "spec", "specs", "__tests__", "test_utils"];
const CONFIG_PATTERNS: &[&str] = &["config", "configs", "conf", ".config"];
const INFRA_PATTERNS: &[&str] = &[
    ".github",
    ".github/workflows",
    ".gitlab",
    ".circleci",
    "ci",
    "deploy",
    "deployment",
    "terraform",
    "k8s",
    "kubernetes",
    "helm",
    "docker",
];
const DOC_PATTERNS: &[&str] = &["docs", "doc", "documentation"];
const GENERATED_PATTERNS: &[&str] = &[
    "dist",
    "build",
    "out",
    "output",
    ".generated",
    "__generated__",
];
const VENDOR_PATTERNS: &[&str] = &["vendor", "third_party", "thirdparty", "external", "deps"];
const BUILD_ARTIFACT_PATTERNS: &[&str] = &[
    "target",
    "node_modules",
    ".gradle",
    ".cache",
    ".cargo/registry",
];

const SOURCE_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "py", "go", "java"];
const TEST_PATTERNS_FILE: &[&str] = &["_test.", "_spec.", "test_", "spec_", "_test_", "_spec_"];
const CONFIG_EXTENSIONS: &[&str] = &["toml", "yaml", "yml", "json", "env", "ini"];
const DOC_EXTENSIONS: &[&str] = &["md", "rst", "adoc", "html"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DirectoryRole {
    Source,
    Test,
    Config,
    Infrastructure,
    Documentation,
    Generated,
    Vendor,
    BuildArtifact,
    ServiceRoot,
}

impl DirectoryRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            DirectoryRole::Source => "SOURCE",
            DirectoryRole::Test => "TEST",
            DirectoryRole::Config => "CONFIG",
            DirectoryRole::Infrastructure => "INFRASTRUCTURE",
            DirectoryRole::Documentation => "DOCUMENTATION",
            DirectoryRole::Generated => "GENERATED",
            DirectoryRole::Vendor => "VENDOR",
            DirectoryRole::BuildArtifact => "BUILD_ARTIFACT",
            DirectoryRole::ServiceRoot => "SERVICE_ROOT",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "SOURCE" => Some(DirectoryRole::Source),
            "TEST" => Some(DirectoryRole::Test),
            "CONFIG" => Some(DirectoryRole::Config),
            "INFRASTRUCTURE" => Some(DirectoryRole::Infrastructure),
            "DOCUMENTATION" => Some(DirectoryRole::Documentation),
            "GENERATED" => Some(DirectoryRole::Generated),
            "VENDOR" => Some(DirectoryRole::Vendor),
            "BUILD_ARTIFACT" => Some(DirectoryRole::BuildArtifact),
            "SERVICE_ROOT" => Some(DirectoryRole::ServiceRoot),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryClassification {
    pub dir_path: String,
    pub role: DirectoryRole,
    pub confidence: f64,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyIndexStats {
    pub directories_classified: usize,
    pub unclassified: usize,
    pub role_counts: HashMap<DirectoryRole, usize>,
}

/// Classify a directory by its path pattern.
/// Returns (role, confidence) or None if no pattern matches.
pub fn classify_by_path(dir_path: &str) -> Option<(DirectoryRole, f64)> {
    // Normalize: remove trailing slash, use forward slashes
    let normalized = dir_path.trim_end_matches('/').replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();

    // Special-case overrides first
    // .github/workflows is Infrastructure (not Config)
    if normalized == ".github/workflows" || normalized.ends_with("/.github/workflows") {
        return Some((DirectoryRole::Infrastructure, 1.0));
    }
    // src/test or src/tests is Test (not Source)
    if normalized == "src/test"
        || normalized == "src/tests"
        || normalized.ends_with("/src/test")
        || normalized.ends_with("/src/tests")
    {
        return Some((DirectoryRole::Test, 1.0));
    }

    // Exact name matches (last component)
    if let Some(last) = parts.last()
        && let Some((role, conf)) = match_last_component(last)
    {
        return Some((role, conf));
    }

    // Parent inheritance: check if any parent directory matches
    for i in 1..parts.len() {
        let parent = parts[..i].join("/");
        if let Some((parent_role, parent_conf)) = classify_by_path(&parent) {
            let nesting_depth = parts.len() - i;
            let confidence = (parent_conf - 0.1 * nesting_depth as f64).max(0.5);
            return Some((parent_role, confidence));
        }
    }

    None
}

fn match_last_component(name: &str) -> Option<(DirectoryRole, f64)> {
    // Check patterns in priority order
    if INFRA_PATTERNS.contains(&name) {
        return Some((DirectoryRole::Infrastructure, 1.0));
    }
    if TEST_PATTERNS.contains(&name) {
        return Some((DirectoryRole::Test, 1.0));
    }
    if SOURCE_PATTERNS.contains(&name) {
        return Some((DirectoryRole::Source, 1.0));
    }
    if CONFIG_PATTERNS.contains(&name) {
        return Some((DirectoryRole::Config, 1.0));
    }
    if DOC_PATTERNS.contains(&name) {
        return Some((DirectoryRole::Documentation, 1.0));
    }
    if GENERATED_PATTERNS.contains(&name) {
        return Some((DirectoryRole::Generated, 1.0));
    }
    if VENDOR_PATTERNS.contains(&name) {
        return Some((DirectoryRole::Vendor, 1.0));
    }
    if BUILD_ARTIFACT_PATTERNS.contains(&name) {
        return Some((DirectoryRole::BuildArtifact, 1.0));
    }
    None
}

/// Classify a directory by examining its file contents.
/// Returns (role, confidence) or None if no clear majority.
pub fn classify_by_content(files: &[&str]) -> Option<(DirectoryRole, f64)> {
    if files.is_empty() {
        return None;
    }

    let total = files.len() as f64;
    let test_count = files.iter().filter(|f| is_test_file(f)).count() as f64;
    // Source files exclude test files to avoid misclassifying test dirs as Source
    let source_count = files
        .iter()
        .filter(|f| is_source_file(f) && !is_test_file(f))
        .count() as f64;
    let config_count = files.iter().filter(|f| is_config_file(f)).count() as f64;
    let doc_count = files.iter().filter(|f| is_doc_file(f)).count() as f64;

    let has_infra = files.iter().any(|f| {
        f.ends_with("Dockerfile")
            || f.contains(".github/")
            || f.ends_with(".yml") && f.contains("workflow")
            || f.contains("ci.yml")
    });

    // Check infrastructure indicators first
    if has_infra {
        return Some((DirectoryRole::Infrastructure, 0.7));
    }

    let test_pct = test_count / total;
    let source_pct = source_count / total;
    let config_pct = config_count / total;
    let doc_pct = doc_count / total;

    if test_pct > 0.7 {
        return Some((DirectoryRole::Test, 0.6));
    }
    if source_pct > 0.7 {
        return Some((DirectoryRole::Source, 0.6));
    }
    if config_pct > 0.7 {
        return Some((DirectoryRole::Config, 0.6));
    }
    if doc_pct > 0.7 {
        return Some((DirectoryRole::Documentation, 0.6));
    }

    None
}

pub fn is_source_file(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("");
    SOURCE_EXTENSIONS.contains(&ext)
}

pub fn is_test_file(path: &str) -> bool {
    TEST_PATTERNS_FILE.iter().any(|p| path.contains(p))
}

pub fn is_config_file(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("");
    CONFIG_EXTENSIONS.contains(&ext)
}

pub fn is_doc_file(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("");
    DOC_EXTENSIONS.contains(&ext)
}

/// Combine path pattern and content heuristic to classify a directory.
/// Uses the higher-confidence result. If path pattern returns >= 0.8, skip content heuristic.
pub fn classify_directory(dir_path: &str, files: &[&str]) -> Option<DirectoryClassification> {
    let path_result = classify_by_path(dir_path);

    // If path pattern gives high confidence, use it directly
    if let Some((role, conf)) = &path_result
        && *conf >= 0.8
    {
        return Some(DirectoryClassification {
            dir_path: dir_path.to_string(),
            role: role.clone(),
            confidence: *conf,
            evidence: format!("Path pattern match: {}", dir_path),
        });
    }

    // Try content heuristic
    let content_result = classify_by_content(files);

    // Use the higher-confidence result
    match (path_result, content_result) {
        (Some((pr, pc)), Some((cr, cc))) => {
            if pc >= cc {
                Some(DirectoryClassification {
                    dir_path: dir_path.to_string(),
                    role: pr,
                    confidence: pc,
                    evidence: format!("Path pattern match: {}", dir_path),
                })
            } else {
                Some(DirectoryClassification {
                    dir_path: dir_path.to_string(),
                    role: cr,
                    confidence: cc,
                    evidence: format!("Content heuristic: {} files analyzed", files.len()),
                })
            }
        }
        (Some((r, c)), None) => Some(DirectoryClassification {
            dir_path: dir_path.to_string(),
            role: r,
            confidence: c,
            evidence: format!("Path pattern match: {}", dir_path),
        }),
        (None, Some((r, c))) => Some(DirectoryClassification {
            dir_path: dir_path.to_string(),
            role: r,
            confidence: c,
            evidence: format!("Content heuristic: {} files analyzed", files.len()),
        }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_source_path() {
        assert_eq!(classify_by_path("src").unwrap().0, DirectoryRole::Source);
        assert_eq!(
            classify_by_path("src/utils").unwrap().0,
            DirectoryRole::Source
        );
        assert_eq!(classify_by_path("src/utils").unwrap().1, 0.9);
    }

    #[test]
    fn test_classify_test_path() {
        assert_eq!(classify_by_path("tests").unwrap().0, DirectoryRole::Test);
        assert_eq!(classify_by_path("test").unwrap().0, DirectoryRole::Test);
        assert_eq!(classify_by_path("spec").unwrap().0, DirectoryRole::Test);
    }

    #[test]
    fn test_classify_infrastructure_path() {
        assert_eq!(
            classify_by_path(".github/workflows").unwrap().0,
            DirectoryRole::Infrastructure
        );
        assert_eq!(
            classify_by_path("deploy").unwrap().0,
            DirectoryRole::Infrastructure
        );
    }

    #[test]
    fn test_classify_special_cases() {
        // src/test is Test, not Source
        assert_eq!(classify_by_path("src/test").unwrap().0, DirectoryRole::Test);
        assert_eq!(
            classify_by_path("src/tests").unwrap().0,
            DirectoryRole::Test
        );
    }

    #[test]
    fn test_parent_inheritance() {
        // Nested directory inherits from parent
        let (role, conf) = classify_by_path("src/deep/nested").unwrap();
        assert_eq!(role, DirectoryRole::Source);
        assert!(conf < 0.9); // Confidence reduced
        assert!(conf >= 0.5); // Minimum 0.5
    }

    #[test]
    fn test_unrecognized_path() {
        assert!(classify_by_path("random_dir").is_none());
    }

    #[test]
    fn test_content_heuristic_source() {
        let files = vec!["main.rs", "lib.rs", "mod.rs"];
        let (role, conf) = classify_by_content(&files).unwrap();
        assert_eq!(role, DirectoryRole::Source);
        assert_eq!(conf, 0.6);
    }

    #[test]
    fn test_content_heuristic_test() {
        let files = vec!["main_test.rs", "lib_test.rs", "mod_test.rs"];
        let (role, _conf) = classify_by_content(&files).unwrap();
        assert_eq!(role, DirectoryRole::Test);
    }

    #[test]
    fn test_content_heuristic_empty() {
        let files: Vec<&str> = vec![];
        assert!(classify_by_content(&files).is_none());
    }

    #[test]
    fn test_classify_directory_high_confidence_path() {
        let files = vec!["random.txt"];
        let result = classify_directory("src", &files).unwrap();
        assert_eq!(result.role, DirectoryRole::Source);
        assert_eq!(result.confidence, 1.0);
    }

    #[test]
    fn test_classify_directory_content_fallback() {
        let files = vec!["main.rs", "lib.rs", "mod.rs"];
        let result = classify_directory("custom_dir", &files).unwrap();
        assert_eq!(result.role, DirectoryRole::Source);
        assert_eq!(result.confidence, 0.6);
    }

    #[test]
    fn test_directory_role_serialization() {
        let role = DirectoryRole::Source;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, "\"SOURCE\"");

        let deserialized: DirectoryRole = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, DirectoryRole::Source);
    }

    #[test]
    fn test_is_source_test_config_doc_files() {
        assert!(is_source_file("main.rs"));
        assert!(is_source_file("app.ts"));
        assert!(is_source_file("handler.py"));

        assert!(is_test_file("main_test.rs"));
        assert!(is_test_file("spec_handler.ts"));

        assert!(is_config_file("config.toml"));
        assert!(is_config_file("settings.yaml"));

        assert!(is_doc_file("README.md"));
        assert!(is_doc_file("guide.rst"));
    }
}
