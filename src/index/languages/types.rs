use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Markdown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Language::Rust),
            "ts" | "tsx" | "js" | "jsx" => Some(Language::TypeScript),
            "py" => Some(Language::Python),
            "md" => Some(Language::Markdown),
            _ => None,
        }
    }
}
