use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[value(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuleType {
    TechStack,
    Validator,
    Mapping,
    Watcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[value(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ValidationLevel {
    #[default]
    Error,
    Warning,
}

fn default_general() -> String {
    "GENERAL".to_string()
}
fn default_unnamed() -> String {
    "UNNAMED".to_string()
}
fn default_active() -> String {
    "ACTIVE".to_string()
}
fn default_file() -> String {
    "FILE".to_string()
}
fn default_echo() -> String {
    "echo".to_string()
}
fn default_all() -> String {
    "ALL".to_string()
}
fn default_timeout() -> i32 {
    5000
}
fn default_db() -> String {
    "DB".to_string()
}
fn default_watcher_glob() -> String {
    "**/*".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechStackRule {
    #[serde(default = "default_general")]
    pub category: String, // e.g., DATABASE, BACKEND_LANG
    #[serde(default = "default_unnamed")]
    pub name: String, // e.g., SQLite, Rust
    pub version_constraint: Option<String>,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub locked: bool,
    #[serde(default = "default_active")]
    pub status: String, // ACTIVE, DEPRECATED, PROPOSED
    #[serde(default = "default_file")]
    pub entity_type: String, // FILE, ABSTRACT
    #[serde(default)]
    pub registered_at: String,
}

impl Default for TechStackRule {
    fn default() -> Self {
        Self {
            category: default_general(),
            name: default_unnamed(),
            version_constraint: None,
            rules: Vec::new(),
            locked: false,
            status: default_active(),
            entity_type: default_file(),
            registered_at: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitValidator {
    pub id: Option<i64>,
    #[serde(default = "default_all")]
    pub category: String, // Which transaction categories this applies to
    #[serde(default = "default_unnamed")]
    pub name: String,
    pub description: Option<String>,
    #[serde(default = "default_echo")]
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: i32,
    pub glob: Option<String>,
    #[serde(default)]
    pub validation_level: ValidationLevel,
    #[serde(default = "serde_true")]
    pub enabled: bool,
}

impl Default for CommitValidator {
    fn default() -> Self {
        Self {
            id: None,
            category: default_all(),
            name: default_unnamed(),
            description: None,
            executable: default_echo(),
            args: Vec::new(),
            timeout_ms: default_timeout(),
            glob: None,
            validation_level: ValidationLevel::default(),
            enabled: true,
        }
    }
}

fn serde_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryStackMapping {
    pub id: Option<i64>,
    #[serde(default = "default_general")]
    pub ledger_category: String,
    #[serde(default = "default_general")]
    pub stack_category: String,
    pub glob: Option<String>,
    pub description: Option<String>,
}

impl Default for CategoryStackMapping {
    fn default() -> Self {
        Self {
            id: None,
            ledger_category: default_general(),
            stack_category: default_general(),
            glob: None,
            description: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherPattern {
    pub id: Option<i64>,
    #[serde(default = "default_watcher_glob")]
    pub glob: String,
    #[serde(default = "default_general")]
    pub category: String,
    #[serde(default = "default_db")]
    pub source: String, // CONFIG, DB, DEFAULT
    pub description: Option<String>,
}

impl Default for WatcherPattern {
    fn default() -> Self {
        Self {
            id: None,
            glob: default_watcher_glob(),
            category: default_general(),
            source: default_db(),
            description: None,
        }
    }
}
