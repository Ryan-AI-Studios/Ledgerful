use crate::impact::packet::ImpactPacket;

/// Path patterns that identify shared infrastructure. When any changed file
/// matches one of these, scoped selection is skipped and the full suite runs,
/// because these files can break anything in the project.
const SHARED_INFRA_PATTERNS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "src/cli/args/**",
    "src/cli/dispatch/**",
    "src/cli/mod.rs",
    "src/config/**",
    "src/state/migrations/**",
    "src/state/migrations.rs",
    "src/state/storage/**",
    "src/state/storage_cozo.rs",
    ".ledgerful/**",
    ".github/workflows/**",
    "build.rs",
];

/// Returns true if any changed file in the packet matches a shared
/// infrastructure pattern, meaning the full suite must run.
pub(crate) fn touches_shared_infra(packet: &ImpactPacket) -> bool {
    let matchers: Vec<globset::GlobMatcher> = SHARED_INFRA_PATTERNS
        .iter()
        .filter_map(|p| globset::Glob::new(p).ok())
        .map(|g| g.compile_matcher())
        .collect();
    packet.changes.iter().any(|f| {
        let path_str = f.path.to_string_lossy().replace('\\', "/");
        matchers.iter().any(|m| m.is_match(&path_str))
    })
}
