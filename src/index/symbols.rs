use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    Type,
    Variable,
    Constant,
    Module,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "Function",
            SymbolKind::Method => "Method",
            SymbolKind::Class => "Class",
            SymbolKind::Struct => "Struct",
            SymbolKind::Enum => "Enum",
            SymbolKind::Trait => "Trait",
            SymbolKind::Interface => "Interface",
            SymbolKind::Type => "Type",
            SymbolKind::Variable => "Variable",
            SymbolKind::Constant => "Constant",
            SymbolKind::Module => "Module",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Function" => Some(SymbolKind::Function),
            "Method" => Some(SymbolKind::Method),
            "Class" => Some(SymbolKind::Class),
            "Struct" => Some(SymbolKind::Struct),
            "Enum" => Some(SymbolKind::Enum),
            "Trait" => Some(SymbolKind::Trait),
            "Interface" => Some(SymbolKind::Interface),
            "Type" => Some(SymbolKind::Type),
            "Variable" => Some(SymbolKind::Variable),
            "Constant" => Some(SymbolKind::Constant),
            "Module" => Some(SymbolKind::Module),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub is_public: bool,
    pub cognitive_complexity: Option<i32>,
    pub cyclomatic_complexity: Option<i32>,
    #[serde(default)]
    pub line_start: Option<i32>,
    #[serde(default)]
    pub line_end: Option<i32>,
    #[serde(default)]
    pub qualified_name: Option<String>,
    #[serde(default)]
    pub byte_start: Option<i32>,
    #[serde(default)]
    pub byte_end: Option<i32>,
    #[serde(default)]
    pub entrypoint_kind: Option<String>,
    #[serde(default)]
    pub metadata: std::collections::BTreeMap<String, String>,
}
