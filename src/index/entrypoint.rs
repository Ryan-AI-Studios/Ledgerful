use serde::{Deserialize, Serialize};

mod python;
mod rust;
mod typescript;

pub use python::detect_python_entrypoints;
pub use rust::detect_rust_entrypoints;
pub use typescript::detect_typescript_entrypoints;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[derive(Default)]
pub enum EntrypointKind {
    Entrypoint,
    Handler,
    PublicApi,
    Test,
    Ffi,
    Macro,
    #[default]
    Internal,
}

impl EntrypointKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntrypointKind::Entrypoint => "ENTRYPOINT",
            EntrypointKind::Handler => "HANDLER",
            EntrypointKind::PublicApi => "PUBLIC_API",
            EntrypointKind::Test => "TEST",
            EntrypointKind::Ffi => "FFI",
            EntrypointKind::Macro => "MACRO",
            EntrypointKind::Internal => "INTERNAL",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ENTRYPOINT" => Some(EntrypointKind::Entrypoint),
            "HANDLER" => Some(EntrypointKind::Handler),
            "PUBLIC_API" => Some(EntrypointKind::PublicApi),
            "TEST" => Some(EntrypointKind::Test),
            "FFI" => Some(EntrypointKind::Ffi),
            "MACRO" => Some(EntrypointKind::Macro),
            "INTERNAL" => Some(EntrypointKind::Internal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntrypointStats {
    pub entrypoints: usize,
    pub handlers: usize,
    pub public_apis: usize,
    pub tests: usize,
    pub ffi: usize,
    pub macros: usize,
    pub internal: usize,
}

/// Result of classifying a single symbol's entrypoint kind.
pub struct SymbolClassification {
    pub symbol_name: String,
    pub kind: EntrypointKind,
    pub confidence: f64,
    pub evidence: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entrypoint_kind_serialization() {
        assert_eq!(
            serde_json::to_string(&EntrypointKind::Entrypoint).unwrap(),
            "\"ENTRYPOINT\""
        );
        assert_eq!(
            serde_json::to_string(&EntrypointKind::PublicApi).unwrap(),
            "\"PUBLIC_API\""
        );
        let deserialized: EntrypointKind = serde_json::from_str("\"HANDLER\"").unwrap();
        assert_eq!(deserialized, EntrypointKind::Handler);
    }

    #[test]
    fn test_entrypoint_kind_default() {
        assert_eq!(EntrypointKind::default(), EntrypointKind::Internal);
    }
}
