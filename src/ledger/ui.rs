use crate::ledger::types::{Category, ChangeType};
use owo_colors::{OwoColorize, Stream};

pub enum LedgerStatus {
    Pending,
    Committed,
    Stale,
    Federated,
}

pub fn get_status_icon(status: LedgerStatus) -> String {
    match status {
        LedgerStatus::Pending => "󱐋"
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string(),
        LedgerStatus::Committed => "󰄬"
            .if_supports_color(Stream::Stdout, |s| s.green())
            .to_string(),
        LedgerStatus::Stale => "󰀦"
            .if_supports_color(Stream::Stdout, |s| s.red())
            .to_string(),
        LedgerStatus::Federated => "󰛄"
            .if_supports_color(Stream::Stdout, |s| s.magenta())
            .to_string(),
    }
}

pub fn get_category_icon(category: &Category) -> String {
    match category {
        Category::Architecture => "󰙅"
            .if_supports_color(Stream::Stdout, |s| s.blue())
            .to_string(),
        Category::Feature => "󰄬"
            .if_supports_color(Stream::Stdout, |s| s.green())
            .to_string(),
        Category::Bugfix => "󰀦"
            .if_supports_color(Stream::Stdout, |s| s.red())
            .to_string(),
        Category::Refactor => "󰛄"
            .if_supports_color(Stream::Stdout, |s| s.blue())
            .to_string(),
        Category::Infra => "󱇙"
            .if_supports_color(Stream::Stdout, |s| s.cyan())
            .to_string(),
        Category::Security => "󰒓"
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string(),
        Category::Tooling => "󰒓"
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string(),
        Category::Docs => "󰛄"
            .if_supports_color(Stream::Stdout, |s| s.magenta())
            .to_string(),
        Category::Chore => "󱐋"
            .if_supports_color(Stream::Stdout, |s| s.dimmed())
            .to_string(),
    }
}

pub fn get_change_type_icon(change_type: &ChangeType) -> String {
    match change_type {
        ChangeType::Create => "󰐕"
            .if_supports_color(Stream::Stdout, |s| s.green())
            .to_string(),
        ChangeType::Modify => "󰷉"
            .if_supports_color(Stream::Stdout, |s| s.yellow())
            .to_string(),
        ChangeType::Delete => "󰆴"
            .if_supports_color(Stream::Stdout, |s| s.red())
            .to_string(),
        ChangeType::Deprecate => "󰀦"
            .if_supports_color(Stream::Stdout, |s| s.magenta())
            .to_string(),
    }
}

pub fn breaking_icon() -> String {
    "󰀦"
        .if_supports_color(Stream::Stdout, |s| s.red())
        .to_string()
}
