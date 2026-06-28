use crate::ledger::types::{Category, ChangeType};
use owo_colors::OwoColorize;

pub enum LedgerStatus {
    Pending,
    Committed,
    Stale,
    Federated,
}

pub fn get_status_icon(status: LedgerStatus) -> String {
    match status {
        LedgerStatus::Pending => "󱐋".yellow().to_string(),
        LedgerStatus::Committed => "󰄬".green().to_string(),
        LedgerStatus::Stale => "󰀦".red().to_string(),
        LedgerStatus::Federated => "󰛄".magenta().to_string(),
    }
}

pub fn get_category_icon(category: &Category) -> String {
    match category {
        Category::Architecture => "󰙅".blue().to_string(),
        Category::Feature => "󰄬".green().to_string(),
        Category::Bugfix => "󰀦".red().to_string(),
        Category::Refactor => "󰛄".blue().to_string(),
        Category::Infra => "󱇙".cyan().to_string(),
        Category::Security => "󰒓".yellow().to_string(),
        Category::Tooling => "󰒓".yellow().to_string(),
        Category::Docs => "󰛄".magenta().to_string(),
        Category::Chore => "󱐋".dimmed().to_string(),
    }
}

pub fn get_change_type_icon(change_type: &ChangeType) -> String {
    match change_type {
        ChangeType::Create => "󰐕".green().to_string(),
        ChangeType::Modify => "󰷉".yellow().to_string(),
        ChangeType::Delete => "󰆴".red().to_string(),
        ChangeType::Deprecate => "󰀦".magenta().to_string(),
    }
}

pub fn breaking_icon() -> String {
    "󰀦".red().to_string()
}
