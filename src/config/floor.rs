//! Internal-state ignore floor for the effective `watch.ignore_patterns`.
//!
//! An explicit `watch.ignore_patterns` list (project config or user template)
//! replaces the built-in list wholesale, so an older template that predates the
//! current state directory can make internal state files read as user-source
//! dirt. This module guarantees the product-owned state entries are always
//! present in the effective list.
//!
//! The floor is a pure, in-memory, additive transformation: it never removes,
//! reorders or deduplicates a user entry and never writes a file. The retired
//! state-directory name is derived from [`crate::state::layout`], never spelled
//! out here, so no seed or template can materialize it.

use crate::config::model::Config;
use crate::state::layout::{LEGACY_STATE_DIR, STATE_DIR};

/// The four fixed floor entries, in append order.
pub(crate) fn internal_state_ignore_entries() -> [String; 4] {
    [
        STATE_DIR.to_string(),
        format!("{STATE_DIR}/**"),
        LEGACY_STATE_DIR.to_string(),
        format!("{LEGACY_STATE_DIR}/**"),
    ]
}

/// Append every missing internal-state entry to `config.watch.ignore_patterns`.
///
/// Additive and idempotent: an entry is appended only when the exact string is
/// absent, and the existing list keeps its order and duplicates.
pub(crate) fn apply_internal_state_ignore_floor(config: &mut Config) {
    for entry in internal_state_ignore_entries() {
        if !config.watch.ignore_patterns.iter().any(|p| p == &entry) {
            config.watch.ignore_patterns.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::defaults::DEFAULT_CONFIG;
    use crate::state::layout::LEGACY_STATE_DIR;

    fn config_with(patterns: &[&str]) -> Config {
        let mut config = Config::default();
        config.watch.ignore_patterns = patterns.iter().map(|p| p.to_string()).collect();
        config
    }

    fn retired_pair() -> [String; 2] {
        [
            LEGACY_STATE_DIR.to_string(),
            format!("{LEGACY_STATE_DIR}/**"),
        ]
    }

    #[test]
    fn internal_state_floor_appends_when_missing() {
        let mut config = config_with(&["target/**", "dist/**"]);
        apply_internal_state_ignore_floor(&mut config);
        let expected_tail = internal_state_ignore_entries();
        assert_eq!(config.watch.ignore_patterns.len(), 2 + expected_tail.len());
        assert_eq!(config.watch.ignore_patterns[..2], ["target/**", "dist/**"]);
        assert_eq!(&config.watch.ignore_patterns[2..], &expected_tail);
    }

    #[test]
    fn internal_state_floor_preserves_user_order() {
        let mut config = config_with(&["dist/**", "target/**", "dist/**"]);
        let original = config.watch.ignore_patterns.clone();
        apply_internal_state_ignore_floor(&mut config);
        assert_eq!(&config.watch.ignore_patterns[..original.len()], &original);
        assert_eq!(
            config.watch.ignore_patterns[original.len()..].to_vec(),
            internal_state_ignore_entries().to_vec()
        );
    }

    #[test]
    fn internal_state_floor_is_idempotent() {
        let mut once = config_with(&["target/**"]);
        apply_internal_state_ignore_floor(&mut once);
        let mut twice = once.clone();
        apply_internal_state_ignore_floor(&mut twice);
        assert_eq!(once.watch.ignore_patterns, twice.watch.ignore_patterns);
    }

    #[test]
    fn internal_state_floor_keeps_builtin_prefix_unchanged() {
        let parsed: Config = toml::from_str(DEFAULT_CONFIG).expect("built-in TOML");
        let builtin = parsed.watch.ignore_patterns.clone();
        assert_eq!(builtin.len(), 16);
        let mut config = parsed;
        apply_internal_state_ignore_floor(&mut config);
        assert_eq!(&config.watch.ignore_patterns[..16], &builtin);
        assert_eq!(
            config.watch.ignore_patterns[16..].to_vec(),
            retired_pair().to_vec(),
            "only the retired pair is appended to the built-in list"
        );
    }

    #[test]
    fn internal_state_floor_empty_list_yields_only_internal_state() {
        let mut config = config_with(&[]);
        apply_internal_state_ignore_floor(&mut config);
        assert_eq!(
            config.watch.ignore_patterns,
            internal_state_ignore_entries().to_vec()
        );
    }

    #[test]
    fn internal_state_floor_includes_retired_state_dir() {
        // Start with the live pair present: the retired pair still lands.
        let mut config = config_with(&[STATE_DIR, &format!("{STATE_DIR}/**")]);
        apply_internal_state_ignore_floor(&mut config);
        let patterns = &config.watch.ignore_patterns;
        for entry in retired_pair() {
            assert!(
                patterns.iter().any(|p| p == &entry),
                "missing retired entry {entry:?}: {patterns:?}"
            );
        }
        assert_eq!(
            patterns[2..].to_vec(),
            internal_state_ignore_entries()[2..].to_vec()
        );
    }

    #[test]
    fn internal_state_floor_entries_are_valid_globs() {
        for entry in internal_state_ignore_entries() {
            globset::Glob::new(&entry).expect("floor entry must be a valid glob");
        }
    }
}
