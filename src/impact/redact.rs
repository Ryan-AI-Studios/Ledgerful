use crate::impact::packet::ImpactPacket;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Redaction {
    pub path: String,
    pub pattern_name: String,
}

pub const DEFAULT_MAX_BYTES: usize = 256 * 1024;

struct SecretPattern {
    name: &'static str,
    regex: Regex,
}

macro_rules! secret_pattern {
    ($name:expr, $pattern:expr) => {
        SecretPattern {
            name: $name,
            regex: Regex::new($pattern).expect(concat!("invalid regex: ", $pattern)),
        }
    };
}

static SECRET_PATTERNS: LazyLock<Vec<SecretPattern>> = LazyLock::new(|| {
    vec![
        secret_pattern!("AWS Access Key", r"AKIA[0-9A-Z]{16}"),
        secret_pattern!("GitHub Token (ghp)", r"ghp_[A-Za-z0-9_]{36,}"),
        secret_pattern!("GitHub Token (gho)", r"gho_[A-Za-z0-9_]{36,}"),
        secret_pattern!("GitHub Token (ghu)", r"ghu_[A-Za-z0-9_]{36,}"),
        secret_pattern!("GitHub Token (ghs)", r"ghs_[A-Za-z0-9_]{36,}"),
        secret_pattern!("Google API Key", r"AIza[0-9A-Za-z_-]{30,}"),
        secret_pattern!("OpenAI Key (sk-)", r"sk-[A-Za-z0-9]{20,}T3BlbkFJ"),
        secret_pattern!("OpenAI Key (sk-proj-)", r"sk-proj-[A-Za-z0-9]{48,}"),
        secret_pattern!("Private Key Block", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
        secret_pattern!(
            "Generic Secret Assignment",
            r#"(?m)(?:secret|password|token|api_key|apikey|access_key)\s*[:=]\s*['"]?[A-Za-z0-9+/=_-]{20,}['"]?"#
        ),
    ]
});

static ENV_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)(?:secret|password|token|api_key|apikey)\s*[:=]\s*['"]?([A-Za-z0-9+/=_-]{20,})['"]?"#)
        .expect("invalid env secret regex")
});

fn is_secret_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy().to_lowercase();
    let filename = path_str.rsplit('/').next().unwrap_or(&path_str);
    let filename = filename.rsplit('\\').next().unwrap_or(filename);

    filename.starts_with(".env")
        || filename.starts_with("credentials")
        || filename.ends_with(".pem")
        || filename.ends_with(".key")
        || filename.ends_with(".p12")
        || filename.ends_with(".jks")
}

fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut freq = [0usize; 256];
    for b in s.bytes() {
        freq[b as usize] += 1;
    }
    let len = s.len() as f64;
    let mut entropy = 0.0;
    for &count in &freq {
        if count > 0 {
            let p = count as f64 / len;
            entropy -= p * p.log2();
        }
    }
    entropy
}

fn looks_like_high_entropy_secret(value: &str) -> bool {
    value.len() >= 20 && shannon_entropy(value) >= 4.5
}

pub fn redact_secrets(packet: &mut ImpactPacket) -> Vec<Redaction> {
    let mut redactions = Vec::new();

    for file in &mut packet.changes {
        let is_secret_file = is_secret_path(&file.path);

        // Redact file content patterns in the status field isn't useful,
        // but for secret-path files we should note it
        if is_secret_file {
            redactions.push(Redaction {
                path: file.path.to_string_lossy().to_string(),
                pattern_name: "Secret-path file".to_string(),
            });
        }

        // Check symbols for potential secrets
        if let Some(ref mut symbols) = file.symbols {
            for symbol in symbols.iter_mut() {
                let name = symbol.name.clone();
                for pattern in SECRET_PATTERNS.iter() {
                    if pattern.regex.is_match(&name) {
                        redactions.push(Redaction {
                            path: file.path.to_string_lossy().to_string(),
                            pattern_name: pattern.name.to_string(),
                        });
                        symbol.name = format!("[REDACTED:{}]", pattern.name);
                        break;
                    }
                }
                // Entropy check for env-file symbols (only if not already redacted)
                if is_secret_file
                    && looks_like_high_entropy_secret(&name)
                    && !symbol.name.starts_with("[REDACTED:")
                {
                    redactions.push(Redaction {
                        path: file.path.to_string_lossy().to_string(),
                        pattern_name: "High-entropy string".to_string(),
                    });
                    symbol.name = "[REDACTED:high-entropy]".to_string();
                }
            }
        }
    }

    redactions.sort_unstable_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.pattern_name.cmp(&b.pattern_name))
    });
    redactions.dedup();
    redactions
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SanitizeResult {
    pub sanitized: String,
    pub redactions: Vec<Redaction>,
    pub truncated: bool,
    pub original_bytes: usize,
}

pub fn sanitize_prompt(prompt: &str, max_bytes: usize) -> SanitizeResult {
    let original_bytes = prompt.len();
    let mut sanitized = prompt.to_string();
    let mut redactions = Vec::new();

    // Apply pattern-based redaction
    for pattern in SECRET_PATTERNS.iter() {
        let matches: Vec<_> = pattern.regex.find_iter(&sanitized).collect();
        if !matches.is_empty() {
            sanitized = pattern
                .regex
                .replace_all(&sanitized, &format!("[REDACTED:{}]", pattern.name))
                .to_string();
            redactions.push(Redaction {
                path: "<prompt>".to_string(),
                pattern_name: pattern.name.to_string(),
            });
        }
    }

    // Check for high-entropy strings in env-like contexts
    let high_entropy_values: Vec<String> = ENV_SECRET_RE
        .captures_iter(&sanitized)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .filter(|v| looks_like_high_entropy_secret(v))
        .collect();

    for value in &high_entropy_values {
        sanitized = sanitized.replace(value.as_str(), "[REDACTED:high-entropy]");
        redactions.push(Redaction {
            path: "<prompt>".to_string(),
            pattern_name: "High-entropy string".to_string(),
        });
    }

    redactions.sort_unstable_by(|a, b| a.pattern_name.cmp(&b.pattern_name));
    redactions.dedup();

    // Truncate at paragraph boundary if needed
    let truncated = if sanitized.len() > max_bytes {
        // Try paragraph boundary (double newline) within the last 10% of allowed size
        let search_start = (max_bytes as f64 * 0.9) as usize;
        let search_end = max_bytes;
        let search_region =
            &sanitized[search_start.min(sanitized.len())..search_end.min(sanitized.len())];

        if let Some(pos) = search_region.find("\n\n") {
            let cut_point = search_start + pos;
            sanitized.truncate(cut_point);
        } else if let Some(pos) = search_region.rfind('\n') {
            let cut_point = search_start + pos;
            sanitized.truncate(cut_point);
        } else {
            sanitized.truncate(max_bytes);
        }
        sanitized.push_str(&format!(
            "\n\n[TRUNCATED: original was {} bytes, showing first {} bytes]",
            original_bytes,
            sanitized.len()
        ));
        true
    } else {
        false
    };

    SanitizeResult {
        sanitized,
        redactions,
        truncated,
        original_bytes,
    }
}

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::{ChangedFile, FileAnalysisStatus, ImpactPacket};
    use crate::index::symbols::{Symbol, SymbolKind};
    use std::path::PathBuf;

    #[test]
    fn test_redact_aws_key_in_symbols() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/config.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,
            symbols: Some(vec![Symbol {
                name: "AKIAIOSFODNN7EXAMPLE".to_string(),
                kind: SymbolKind::Variable,
                is_public: false,
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: None,
                line_end: None,
                qualified_name: None,
                byte_start: None,
                byte_end: None,
                entrypoint_kind: None,
                metadata: std::collections::BTreeMap::new(),
            }]),
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });

        let redactions = redact_secrets(&mut packet);
        assert!(!redactions.is_empty());
        assert_eq!(redactions[0].pattern_name, "AWS Access Key");
        assert!(
            packet.changes[0].symbols.as_ref().unwrap()[0]
                .name
                .contains("REDACTED")
        );
    }

    #[test]
    fn test_redact_github_token_in_symbols() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/api.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,
            symbols: Some(vec![Symbol {
                name: "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890".to_string(),
                kind: SymbolKind::Variable,
                is_public: false,
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: None,
                line_end: None,
                qualified_name: None,
                byte_start: None,
                byte_end: None,
                entrypoint_kind: None,
                metadata: std::collections::BTreeMap::new(),
            }]),
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });

        let redactions = redact_secrets(&mut packet);
        assert!(
            redactions
                .iter()
                .any(|r| r.pattern_name.contains("GitHub Token"))
        );
    }

    #[test]
    fn test_redact_google_api_key() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/api.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,
            symbols: Some(vec![Symbol {
                name: "AIzaSyA1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q".to_string(),
                kind: SymbolKind::Constant,
                is_public: false,
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: None,
                line_end: None,
                qualified_name: None,
                byte_start: None,
                byte_end: None,
                entrypoint_kind: None,
                metadata: std::collections::BTreeMap::new(),
            }]),
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });

        // First verify the regex itself matches
        let redactions = redact_secrets(&mut packet);
        assert!(
            redactions
                .iter()
                .any(|r| r.pattern_name == "Google API Key"),
            "Expected Google API Key redaction, got: {:?}",
            redactions
        );
    }

    #[test]
    fn test_redact_private_key_block() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("keys/server.key"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,
            symbols: Some(vec![Symbol {
                name: "-----BEGIN RSA PRIVATE KEY-----".to_string(),
                kind: SymbolKind::Constant,
                is_public: false,
                cognitive_complexity: None,
                cyclomatic_complexity: None,
                line_start: None,
                line_end: None,
                qualified_name: None,
                byte_start: None,
                byte_end: None,
                entrypoint_kind: None,
                metadata: std::collections::BTreeMap::new(),
            }]),
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });

        let redactions = redact_secrets(&mut packet);
        assert!(
            redactions
                .iter()
                .any(|r| r.pattern_name == "Private Key Block")
        );
        // Should also flag the path
        assert!(
            redactions
                .iter()
                .any(|r| r.pattern_name == "Secret-path file")
        );
    }

    #[test]
    fn test_secret_path_detection() {
        assert!(is_secret_path(Path::new(".env")));
        assert!(is_secret_path(Path::new(".env.production")));
        assert!(is_secret_path(Path::new("credentials.json")));
        assert!(is_secret_path(Path::new("server.pem")));
        assert!(is_secret_path(Path::new("cert.key")));
        assert!(!is_secret_path(Path::new("src/main.rs")));
        assert!(!is_secret_path(Path::new("README.md")));
    }

    #[test]
    fn test_shannon_entropy() {
        // Low entropy (repeated chars)
        assert!(shannon_entropy("aaaaaaaaaa") < 1.0);
        // High entropy (random-ish)
        assert!(shannon_entropy("a8Kj3mP9xZ2vR7nQ5w") > 3.5);
        // Empty
        assert_eq!(shannon_entropy(""), 0.0);
    }

    #[test]
    fn test_no_redaction_on_normal_symbols() {
        let mut packet = ImpactPacket::default();
        packet.changes.push(ChangedFile {
            path: PathBuf::from("src/api.rs"),
            status: "Modified".to_string(),
            old_path: None,
            is_staged: false,
            symbols: Some(vec![
                Symbol {
                    name: "test".into(),
                    kind: SymbolKind::Function,
                    is_public: true,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: None,
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                },
                Symbol {
                    name: "Config".to_string(),
                    kind: SymbolKind::Struct,
                    is_public: true,
                    cognitive_complexity: None,
                    cyclomatic_complexity: None,
                    line_start: None,
                    line_end: None,
                    qualified_name: None,
                    byte_start: None,
                    byte_end: None,
                    entrypoint_kind: None,
                    metadata: std::collections::BTreeMap::new(),
                },
            ]),
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        });

        let redactions = redact_secrets(&mut packet);
        assert!(redactions.is_empty());
    }

    #[test]
    fn test_sanitize_prompt_removes_secrets() {
        let prompt = r#"
Config:
api_key = "AKIAIOSFODNN7EXAMPLE"
token = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890"
normal_setting = "hello"
"#;
        let result = sanitize_prompt(prompt, DEFAULT_MAX_BYTES);
        assert!(!result.sanitized.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!result.sanitized.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
        assert!(result.sanitized.contains("hello"));
        assert!(!result.truncated);
    }

    #[test]
    fn test_sanitize_prompt_truncation_at_paragraph() {
        let mut prompt = String::new();
        for i in 0..100 {
            prompt.push_str(&format!("Paragraph {} with some content.\n\n", i));
        }
        let result = sanitize_prompt(&prompt, 1000);
        assert!(result.truncated);
        assert!(result.sanitized.contains("[TRUNCATED:"));
        assert!(result.original_bytes > 1000);
    }

    #[test]
    fn test_sanitize_prompt_no_truncation_under_limit() {
        let prompt = "This is a short prompt with no secrets.";
        let result = sanitize_prompt(prompt, DEFAULT_MAX_BYTES);
        assert!(!result.truncated);
        assert_eq!(result.sanitized, prompt);
    }

    #[test]
    fn test_sanitize_prompt_private_key_block() {
        let prompt = "Here is a key:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----\n\nSome other text";
        let result = sanitize_prompt(prompt, DEFAULT_MAX_BYTES);
        assert!(!result.sanitized.contains("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(result.sanitized.contains("[REDACTED:Private Key Block]"));
    }
}
