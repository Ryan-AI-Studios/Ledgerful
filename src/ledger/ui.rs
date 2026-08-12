use crate::ledger::types::{Category, ChangeType};
use crate::output::table::icons_use_nerd_glyphs;
use owo_colors::{OwoColorize, Stream};

#[derive(Debug, Clone, Copy)]
pub enum LedgerStatus {
    Pending,
    Committed,
    Stale,
    Federated,
}

/// Format `icon` + `label` without a leading space when icon is empty (ASCII mode).
pub fn with_icon(icon: &str, label: impl std::fmt::Display) -> String {
    if icon.is_empty() {
        format!("{label}")
    } else {
        format!("{icon} {label}")
    }
}

fn status_icon_raw(status: LedgerStatus, use_nerd: bool) -> String {
    if !use_nerd {
        return match status {
            LedgerStatus::Pending => "*".to_string(),
            LedgerStatus::Committed => "+".to_string(),
            LedgerStatus::Stale => "!".to_string(),
            LedgerStatus::Federated => "~".to_string(),
        };
    }
    match status {
        LedgerStatus::Pending => "󱐋".to_string(),
        LedgerStatus::Committed => "󰄬".to_string(),
        LedgerStatus::Stale => "󰀦".to_string(),
        LedgerStatus::Federated => "󰛄".to_string(),
    }
}

fn category_icon_raw(category: &Category, use_nerd: bool) -> String {
    if !use_nerd {
        // Empty under Ascii — label text remains; no PUA tofu (0181-G).
        return String::new();
    }
    match category {
        Category::Architecture => "󰙅".to_string(),
        Category::Feature => "󰄬".to_string(),
        Category::Bugfix => "󰀦".to_string(),
        Category::Refactor => "󰛄".to_string(),
        Category::Infra => "󱇙".to_string(),
        Category::Security | Category::Tooling => "󰒓".to_string(),
        Category::Docs => "󰛄".to_string(),
        Category::Chore => "󱐋".to_string(),
    }
}

fn change_type_icon_raw(change_type: &ChangeType, use_nerd: bool) -> String {
    if !use_nerd {
        return match change_type {
            ChangeType::Create => "+".to_string(),
            ChangeType::Modify => "~".to_string(),
            ChangeType::Delete => "-".to_string(),
            ChangeType::Deprecate => "!".to_string(),
        };
    }
    match change_type {
        ChangeType::Create => "󰐕".to_string(),
        ChangeType::Modify => "󰷉".to_string(),
        ChangeType::Delete => "󰆴".to_string(),
        ChangeType::Deprecate => "󰀦".to_string(),
    }
}

fn breaking_icon_raw(use_nerd: bool) -> String {
    if !use_nerd {
        return "!".to_string();
    }
    "󰀦".to_string()
}

fn colorize_status(status: LedgerStatus, raw: String) -> String {
    match status {
        LedgerStatus::Pending => raw
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string(),
        LedgerStatus::Committed => raw
            .if_supports_color(Stream::Stdout, |s| s.green())
            .to_string(),
        LedgerStatus::Stale => raw
            .if_supports_color(Stream::Stdout, |s| s.red())
            .to_string(),
        LedgerStatus::Federated => raw
            .if_supports_color(Stream::Stdout, |s| s.magenta())
            .to_string(),
    }
}

fn colorize_category(category: &Category, raw: String) -> String {
    if raw.is_empty() {
        return raw;
    }
    match category {
        Category::Architecture => raw
            .if_supports_color(Stream::Stdout, |s| s.blue())
            .to_string(),
        Category::Feature => raw
            .if_supports_color(Stream::Stdout, |s| s.green())
            .to_string(),
        Category::Bugfix => raw
            .if_supports_color(Stream::Stdout, |s| s.red())
            .to_string(),
        Category::Refactor => raw
            .if_supports_color(Stream::Stdout, |s| s.blue())
            .to_string(),
        Category::Infra => raw
            .if_supports_color(Stream::Stdout, |s| s.cyan())
            .to_string(),
        Category::Security | Category::Tooling => raw
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string(),
        Category::Docs => raw
            .if_supports_color(Stream::Stdout, |s| s.magenta())
            .to_string(),
        Category::Chore => raw
            .if_supports_color(Stream::Stdout, |s| s.dimmed())
            .to_string(),
    }
}

fn colorize_change(change_type: &ChangeType, raw: String) -> String {
    match change_type {
        ChangeType::Create => raw
            .if_supports_color(Stream::Stdout, |s| s.green())
            .to_string(),
        ChangeType::Modify => raw
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string(),
        ChangeType::Delete => raw
            .if_supports_color(Stream::Stdout, |s| s.red())
            .to_string(),
        ChangeType::Deprecate => raw
            .if_supports_color(Stream::Stdout, |s| s.magenta())
            .to_string(),
    }
}

pub fn get_status_icon(status: LedgerStatus) -> String {
    let use_nerd = icons_use_nerd_glyphs();
    let raw = status_icon_raw(status, use_nerd);
    if use_nerd {
        colorize_status(status, raw)
    } else {
        raw
    }
}

pub fn get_category_icon(category: &Category) -> String {
    let use_nerd = icons_use_nerd_glyphs();
    let raw = category_icon_raw(category, use_nerd);
    if use_nerd {
        colorize_category(category, raw)
    } else {
        raw
    }
}

pub fn get_change_type_icon(change_type: &ChangeType) -> String {
    let use_nerd = icons_use_nerd_glyphs();
    let raw = change_type_icon_raw(change_type, use_nerd);
    if use_nerd {
        colorize_change(change_type, raw)
    } else {
        raw
    }
}

pub fn breaking_icon() -> String {
    let use_nerd = icons_use_nerd_glyphs();
    let raw = breaking_icon_raw(use_nerd);
    if use_nerd {
        raw.if_supports_color(Stream::Stdout, |s| s.red())
            .to_string()
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// True for BMP PUA and Supplementary PUA-A/B (covers Nerd Font U+F0000+).
    /// Manual ranges — avoids depending on a recent `char::is_private_use` (0181-G).
    fn is_private_use_char(c: char) -> bool {
        matches!(
            c as u32,
            0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
        )
    }

    fn assert_no_private_use(s: &str) {
        for c in s.chars() {
            assert!(
                !is_private_use_char(c),
                "icon must not use private-use char U+{:04X} in: {s:?}",
                c as u32
            );
        }
    }

    #[test]
    fn ascii_icons_have_no_private_use_chars() {
        // Pure raw paths — hermetic, independent of host console CP / env.
        for status in [
            LedgerStatus::Pending,
            LedgerStatus::Committed,
            LedgerStatus::Stale,
            LedgerStatus::Federated,
        ] {
            assert_no_private_use(&status_icon_raw(status, false));
        }
        for cat in [
            Category::Architecture,
            Category::Feature,
            Category::Bugfix,
            Category::Refactor,
            Category::Infra,
            Category::Security,
            Category::Tooling,
            Category::Docs,
            Category::Chore,
        ] {
            assert_no_private_use(&category_icon_raw(&cat, false));
        }
        for ct in [
            ChangeType::Create,
            ChangeType::Modify,
            ChangeType::Delete,
            ChangeType::Deprecate,
        ] {
            assert_no_private_use(&change_type_icon_raw(&ct, false));
        }
        assert_no_private_use(&breaking_icon_raw(false));
    }

    #[test]
    fn with_icon_empty_skips_space() {
        assert_eq!(with_icon("", "Bugfix"), "Bugfix");
        assert_eq!(with_icon("*", "Bugfix"), "* Bugfix");
    }

    #[test]
    fn private_use_detector_covers_pua_a() {
        // Nerd Font Supplementary PUA-A sample must be detected (0181-G).
        let nerd = char::from_u32(0xF0000).expect("valid scalar");
        assert!(is_private_use_char(nerd));
        let bmp = char::from_u32(0xE000).expect("valid scalar");
        assert!(is_private_use_char(bmp));
        assert!(!is_private_use_char('+'));
        assert!(!is_private_use_char('A'));
    }

    #[test]
    fn nerd_icons_may_use_private_use() {
        // Utf8 mode may include PUA; at least one category glyph is private-use.
        let icon = category_icon_raw(&Category::Bugfix, true);
        assert!(
            icon.chars().any(is_private_use_char),
            "expected Nerd PUA glyph for Bugfix, got {icon:?}"
        );
    }
}
