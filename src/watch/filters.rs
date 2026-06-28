use crate::state::layout::STATE_DIR;
use crate::watch::WatchError;
use camino::Utf8Path;
use globset::{Glob, GlobSet, GlobSetBuilder};

pub struct EventFilter {
    ignore_set: GlobSet,
}

impl EventFilter {
    pub fn new(extra_patterns: &[String]) -> Result<Self, WatchError> {
        let mut builder = GlobSetBuilder::new();

        // Default ignores
        builder.add(Glob::new("**/target/**")?);
        builder.add(Glob::new("**/node_modules/**")?);
        builder.add(Glob::new(&format!("**/{}/**", STATE_DIR))?);
        builder.add(Glob::new(&format!("**/{}*", STATE_DIR))?);

        // Editor temp files
        builder.add(Glob::new("**/*.tmp")?);
        builder.add(Glob::new("**/.*.swp")?);
        builder.add(Glob::new("**/.*.swx")?);
        builder.add(Glob::new("**/~*")?);
        for pattern in extra_patterns {
            builder.add(Glob::new(pattern)?);
        }

        Ok(Self {
            ignore_set: builder.build()?,
        })
    }

    pub fn is_allowed<P: AsRef<Utf8Path>>(&self, path: P) -> bool {
        let path = path.as_ref();
        !self.ignore_set.is_match(path.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_ignores() {
        let filter = EventFilter::new(&[]).unwrap();

        // Should be ignored
        assert!(!filter.is_allowed("target/debug/ledgerful"));
        assert!(!filter.is_allowed("node_modules/some-pkg/index.js"));
        assert!(!filter.is_allowed(".ledgerful/config.toml"));
        assert!(!filter.is_allowed("src/main.rs.tmp"));
        assert!(!filter.is_allowed("src/.main.rs.swp"));

        // Should be allowed
        assert!(filter.is_allowed("src/main.rs"));
        assert!(filter.is_allowed("Cargo.toml"));
        assert!(filter.is_allowed("tests/common/mod.rs"));
    }

    #[test]
    fn test_filter_uses_extra_patterns() {
        let filter = EventFilter::new(&["dist/**".to_string()]).unwrap();
        assert!(!filter.is_allowed("dist/app.js"));
        assert!(filter.is_allowed("src/app.js"));
    }
}
