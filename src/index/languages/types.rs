use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
    /// C and C++ (single grammar: tree-sitter-cpp parses both).
    Cpp,
    /// Java (tree-sitter-java; `.java` only).
    Java,
    Markdown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Language::Rust),
            "ts" | "tsx" | "js" | "jsx" => Some(Language::TypeScript),
            "py" => Some(Language::Python),
            "go" => Some(Language::Go),
            "java" => Some(Language::Java),
            // D2: C/C++ extensions share Language::Cpp (tree-sitter-cpp superset).
            "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "h++" => Some(Language::Cpp),
            "md" => Some(Language::Markdown),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_extension_recognizes_go() {
        assert_eq!(Language::from_extension("go"), Some(Language::Go));
    }

    #[test]
    fn from_extension_recognizes_all_cpp_d2_extensions() {
        for ext in ["c", "h", "cpp", "cc", "cxx", "hpp", "hh", "hxx", "h++"] {
            assert_eq!(
                Language::from_extension(ext),
                Some(Language::Cpp),
                "extension .{ext}"
            );
        }
    }

    #[test]
    fn from_extension_recognizes_java() {
        assert_eq!(Language::from_extension("java"), Some(Language::Java));
        assert_eq!(format!("{:?}", Language::Java), "Java");
    }

    #[test]
    fn from_extension_rejects_java_binaries_and_kotlin() {
        assert_eq!(Language::from_extension("class"), None);
        assert_eq!(Language::from_extension("jar"), None);
        assert_eq!(Language::from_extension("kt"), None);
    }

    #[test]
    fn from_extension_rejects_cuda_and_objc() {
        assert_eq!(Language::from_extension("cu"), None);
        assert_eq!(Language::from_extension("m"), None);
        assert_eq!(Language::from_extension("inl"), None);
    }
}
