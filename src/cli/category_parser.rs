//! Clap `TypedValueParser` for ledger [`Category`] with alias + case-insensitive support.
//!
//! Shared SoT is [`Category::parse_input`]. Help lists the 9 canonical SCREAMING names;
//! unknown tokens get a full list + alias examples even when clap also appends possible values.

use crate::ledger::types::Category;
use clap::builder::{PossibleValue, TypedValueParser};
use clap::error::ErrorKind;
use clap::{Arg, Command, Error};
use std::ffi::OsStr;

/// Value parser that accepts canonical category names (case-insensitive) and frozen aliases.
#[derive(Clone, Debug, Default)]
pub struct CategoryValueParser;

impl TypedValueParser for CategoryValueParser {
    type Value = Category;

    fn parse_ref(
        &self,
        cmd: &Command,
        arg: Option<&Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, Error> {
        let _ = arg;
        let Some(raw) = value.to_str() else {
            return Err(Error::new(ErrorKind::InvalidUtf8).with_cmd(cmd));
        };

        match Category::parse_input(raw) {
            Ok(category) => Ok(category),
            Err(msg) => {
                // Raw message always embeds full canonical list (incl. SECURITY),
                // alias examples, and top-3 did-you-mean (DoD-3 / 0175-E/G).
                // possible_values() still drives clap help / completions.
                Err(Error::raw(ErrorKind::InvalidValue, format!("{msg}\n")).with_cmd(cmd))
            }
        }
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        Some(Box::new(
            Category::CANONICAL_NAMES
                .into_iter()
                .map(PossibleValue::new),
        ))
    }
}

/// Shared long-help text for ledger category args.
pub const CATEGORY_LONG_HELP: &str = "\
Category of change. Canonical names (case-insensitive): ARCHITECTURE, FEATURE, \
BUGFIX, REFACTOR, INFRA, SECURITY, TOOLING, DOCS, CHORE. Common aliases also \
accepted: feat, fix, ux/ui→FEATURE, doc→DOCS, perf/style→REFACTOR, test→CHORE, \
ci/build→INFRA, dx→TOOLING, … Stored value is always the canonical SCREAMING name.";

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser, Debug)]
    struct Sample {
        #[arg(long, value_parser = CategoryValueParser)]
        category: Category,
    }

    #[test]
    fn parser_accepts_alias_and_lowercase_canonical() {
        let a = Sample::try_parse_from(["sample", "--category", "UX"]).expect("UX");
        assert_eq!(a.category, Category::Feature);

        let b = Sample::try_parse_from(["sample", "--category", "feature"]).expect("feature");
        assert_eq!(b.category, Category::Feature);

        let c = Sample::try_parse_from(["sample", "--category", "doc"]).expect("doc");
        assert_eq!(c.category, Category::Docs);
    }

    #[test]
    fn parser_rejects_unknown_with_security_and_aliases() {
        let err = Sample::try_parse_from(["sample", "--category", "NOT_A_CATEGORY"])
            .expect_err("must reject");
        let text = err.to_string();
        assert!(
            text.contains("SECURITY") || text.contains("security"),
            "must mention SECURITY: {text}"
        );
        assert!(
            text.contains("feat") || text.contains("alias") || text.contains("Unknown"),
            "must mention aliases or unknown phrasing: {text}"
        );
    }
}
