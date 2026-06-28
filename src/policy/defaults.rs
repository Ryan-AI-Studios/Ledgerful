pub const DEFAULT_RULES: &str = r#"[global]
mode = "analyze"
required_verifications = []

protected_paths = [
    "Cargo.lock",
    ".github/workflows/**",
    ".ledgerful/**"
]
"#;

pub const LEGACY_DEFAULT_RULES: &str = r#"[global]
mode = "analyze"
required_verifications = []

[[overrides]]
pattern = "Cargo.toml"
mode = "review"
required_verifications = ["build"]

[[overrides]]
pattern = "src/**/*.rs"
required_verifications = ["lint", "test"]

protected_paths = [
    "Cargo.lock",
    ".github/workflows/**",
    ".ledgerful/**"
]
"#;
