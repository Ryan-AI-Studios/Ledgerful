use regex::Regex;
use std::sync::LazyLock;

// --- Rust Patterns ---
pub static RUST_ENV_VAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:std::)?env::var\("([^"]+)"\)"#).unwrap());
pub static RUST_ENV_VAR_OS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:std::)?env::var_os\("([^"]+)"\)"#).unwrap());
pub static RUST_ENV_MACRO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"env!\("([^"]+)"\)"#).unwrap());
pub static RUST_OPTION_ENV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"option_env!\("([^"]+)"\)"#).unwrap());
pub static RUST_ENV_VAR_DEFAULT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:std::)?env::var\("([^"]+)"\)\.(?:unwrap_or|unwrap_or_else)"#).unwrap()
});
pub static RUST_SET_ENV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:std::)?env::set_var\("([^"]+)""#).unwrap());

// --- TS/JS Patterns ---
pub static TS_ENV_DOT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"process\.env\.([A-Z_][A-Z0-9_]*)"#).unwrap());
pub static TS_ENV_INDEXED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"process\.env\[['"]([^'"]+)['"]\]"#).unwrap());
pub static TS_IMPORT_META_ENV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"import\.meta\.env\.([A-Z_][A-Z0-9_]*)"#).unwrap());
pub static TS_ENV_DESTRUCTURING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"const\s+\{\s*([A-Z_][A-Z0-9_]*)\s*\}\s*=\s*process\.env"#).unwrap()
});
pub static TS_ENV_DEFAULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"process\.env\.([A-Z_][A-Z0-9_]*)\s*\|\|"#).unwrap());
pub static TS_SET_ENV: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"process\.env\[?\.?([A-Z_][A-Z0-9_]*)\]?\s*="#).unwrap());

// --- Python Patterns ---
pub static PY_ENV_GET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"os\.(?:environ\.get|getenv)\(['"]([^'"]+)['"]\)"#).unwrap());
pub static PY_ENVIRON_GET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\benviron\.get\(['"]([^'"]+)['"]\)"#).unwrap());
pub static PY_ENV_GET_DEFAULT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"os\.(?:environ\.get|getenv)\(['"]([^'"]+)['"]\s*,\s*"#).unwrap()
});
pub static PY_ENV_INDEXED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"os\.environ\[['"]([^'"]+)['"]\]"#).unwrap());

// --- Runtime Hints (consolidated) ---
pub static CONFIG_HINTS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"\bdotenv\b").unwrap(),
        Regex::new(r"\bconfig\.from_env\b").unwrap(),
        Regex::new(r"\bos\.getenv\b").unwrap(),
    ]
});
