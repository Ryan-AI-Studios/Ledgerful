use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::git::commit::{DEFAULT_COMMIT_MESSAGE_TEMPLATE, format_commit_message, git_commit};
use crate::ledger::*;
use crate::state::storage::StorageManager;
use clap::ValueEnum;
use miette::Result;
use owo_colors::OwoColorize;

fn resolve_start_category(input: &str) -> Result<Category> {
    if let Ok(category) = Category::from_str(input, true) {
        return Ok(category);
    }

    let suggestions = Category::suggestions_for(input);
    if crate::util::term::is_interactive() && !suggestions.is_empty() {
        let choice = inquire::Select::new(
            &format!("Unknown ledger category '{input}'. Select a category:"),
            suggestions,
        )
        .prompt()
        .map_err(|e| miette::miette!("Category selection failed: {e}"))?;
        return Ok(choice);
    }

    if let Some(category) = suggestions.first().copied() {
        eprintln!(
            "{}",
            format!("Unknown ledger category '{input}', using closest match: {category}").yellow()
        );
        return Ok(category);
    }

    Err(miette::miette!(
        "Unknown ledger category '{input}'. Valid categories: ARCHITECTURE, FEATURE, BUGFIX, REFACTOR, INFRA, TOOLING, DOCS, CHORE"
    ))
}

#[derive(Debug, Clone, Default)]
pub struct LedgerCommitGitOptions {
    pub with_git: bool,
    pub git_message: Option<String>,
    pub signoff: bool,
    pub dry_run: bool,
}

pub fn execute_ledger_start(entity: String, category: &str, message: &str) -> Result<()> {
    let category = resolve_start_category(category)?;
    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category,
            entity,
            planned_action: Some(message.to_string()),
            ..Default::default()
        })
        .map_err(|e| miette::miette!("{}", e))?;

    println!("Transaction started: {}", tx_id.cyan());
    Ok(())
}

pub fn execute_ledger_commit(
    tx_id: Option<String>,
    summary: &str,
    reason: &str,
    breaking: bool,
    force: bool,
    git_options: LedgerCommitGitOptions,
) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;

    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config.clone());

    let resolved_id = if let Some(id) = tx_id {
        tx_mgr
            .resolve_tx_id(&id)
            .map_err(|e| miette::miette!("{}", e))?
    } else {
        tx_mgr
            .get_all_pending()
            .map_err(|e| miette::miette!("{}", e))?
            .first()
            .map(|t| t.tx_id.clone())
            .ok_or_else(|| miette::miette!("No active transaction found to commit"))?
    };

    let tx_category = tx_mgr
        .get_transaction(&resolved_id)
        .map_err(|e| miette::miette!("{}", e))?
        .ok_or_else(|| miette::miette!("Transaction not found: {resolved_id}"))?
        .category
        .to_string();

    tx_mgr
        .commit_change(
            resolved_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: summary.to_string(),
                reason: reason.to_string(),
                is_breaking: breaking,
                ..Default::default()
            },
            force,
        )
        .map_err(|e| miette::miette!("{}", e))?;

    println!("{}", "Transaction committed.".green().bold());

    if git_options.with_git {
        execute_git_commit(
            &config.ledger.git_commit_template,
            &tx_category,
            summary,
            &resolved_id,
            git_options,
        );
    }

    Ok(())
}

fn execute_git_commit(
    configured_template: &Option<String>,
    category: &str,
    summary: &str,
    tx_id: &str,
    options: LedgerCommitGitOptions,
) {
    let message = options.git_message.unwrap_or_else(|| {
        let template = configured_template
            .as_deref()
            .unwrap_or(DEFAULT_COMMIT_MESSAGE_TEMPLATE);
        format_commit_message(template, category, summary, tx_id)
    });

    if options.dry_run {
        println!(
            "Dry run: {}",
            display_git_commit_command(&message, options.signoff)
        );
        return;
    }

    match crate::git::commit::can_commit() {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "{}",
                "Warning: Git commit skipped because no files are staged. Ledger commit is complete. Stage files and retry git manually.".yellow()
            );
            return;
        }
        Err(err) => {
            eprintln!(
                "{}",
                format!(
                    "Warning: Git commit skipped: {err}. Ledger commit is complete. Resolve git state and retry manually."
                )
                .yellow()
            );
            return;
        }
    }

    match git_commit(&message, options.signoff) {
        Ok(()) => println!("{}", "Git commit created.".green().bold()),
        Err(err) => {
            eprintln!(
                "{}",
                format!(
                    "Warning: Git commit failed: {err}. Ledger commit is complete. Retry with: {}",
                    display_git_commit_command(&message, options.signoff)
                )
                .yellow()
            );
        }
    }
}

fn display_git_commit_command(message: &str, signoff: bool) -> String {
    let escaped_message = message.replace('"', "\\\"");
    let mut command = format!("git commit -m \"{escaped_message}\"");
    if signoff {
        command.push_str(" --signoff");
    }
    command
}

pub fn execute_ledger_rollback(tx_id: Option<String>, reason: String) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    let resolved_id = if let Some(id) = tx_id {
        tx_mgr
            .resolve_tx_id(&id)
            .map_err(|e| miette::miette!("{}", e))?
    } else {
        tx_mgr
            .get_all_pending()
            .map_err(|e| miette::miette!("{}", e))?
            .first()
            .map(|t| t.tx_id.clone())
            .ok_or_else(|| miette::miette!("No active transaction found to rollback"))?
    };

    tx_mgr
        .rollback_change(resolved_id, reason)
        .map_err(|e| miette::miette!("{}", e))?;

    println!("Transaction rolled back.");
    Ok(())
}

pub fn execute_ledger_atomic(
    entity: &str,
    category: &str,
    summary: &str,
    reason: &str,
    force: bool,
) -> Result<()> {
    let category = Category::from_str(category, true).map_err(|e| miette::miette!("{}", e))?;
    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    tx_mgr
        .atomic_change(
            TransactionRequest {
                category,
                entity: entity.to_string(),
                ..Default::default()
            },
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: summary.to_string(),
                reason: reason.to_string(),
                ..Default::default()
            },
            force,
        )
        .map_err(|e| miette::miette!("{}", e))?;

    println!("{}", "Atomic change committed.".green().bold());
    Ok(())
}

pub fn execute_ledger_resume(tx_id: Option<String>) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    if let Some(id) = tx_id {
        let full_id = tx_mgr
            .resolve_tx_id(&id)
            .map_err(|e| miette::miette!("{}", e))?;
        println!("Resumed transaction: {}", full_id.yellow());
    } else {
        println!("Searching for most recent pending transaction in current context...");
        let pending = tx_mgr
            .get_all_pending()
            .map_err(|e| miette::miette!("{}", e))?;
        if let Some(latest) = pending.first() {
            println!(
                "Resumed most recent: {} ({})",
                latest.tx_id.yellow(),
                latest.entity.cyan()
            );
        } else {
            println!("No pending transactions found to resume.");
        }
    }
    Ok(())
}

pub fn execute_ledger_note(
    entity: &str,
    note: Option<String>,
    message: Option<String>,
) -> Result<()> {
    let final_message = resolve_note_message(note, message)?;

    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    tx_mgr
        .atomic_change(
            TransactionRequest {
                category: Category::Chore,
                entity: entity.to_string(),
                ..Default::default()
            },
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: final_message,
                reason: "Lightweight note".to_string(),
                ..Default::default()
            },
            false,
        )
        .map_err(|e| miette::miette!("{}", e))?;

    println!("{}", "Note recorded.".green().bold());
    Ok(())
}

fn resolve_note_message(note: Option<String>, message: Option<String>) -> Result<String> {
    message
        .or(note)
        .ok_or_else(|| miette::miette!("A note or message must be provided."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Category;

    #[test]
    fn test_note_precedence_behavior() {
        // Both provided -> message (flag) wins
        let res = resolve_note_message(Some("note".into()), Some("message".into())).unwrap();
        assert_eq!(res, "message");

        // Only note provided -> note wins
        let res = resolve_note_message(Some("note".into()), None).unwrap();
        assert_eq!(res, "note");

        // Only message provided -> message wins
        let res = resolve_note_message(None, Some("message".into())).unwrap();
        assert_eq!(res, "message");

        // Neither provided -> error
        let res = resolve_note_message(None, None);
        assert!(res.is_err());
    }

    #[test]
    fn test_resolve_start_category_valid() {
        let result = resolve_start_category("REFACTOR");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Category::Refactor);

        let result = resolve_start_category("FEATURE");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Category::Feature);

        let result = resolve_start_category("BUGFIX");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Category::Bugfix);

        let result = resolve_start_category("ARCHITECTURE");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Category::Architecture);

        let result = resolve_start_category("INFRA");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Category::Infra);

        let result = resolve_start_category("TOOLING");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Category::Tooling);

        let result = resolve_start_category("DOCS");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Category::Docs);

        let result = resolve_start_category("CHORE");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Category::Chore);
    }

    #[test]
    fn test_resolve_start_category_invalid() {
        // When not interactive and no suggestions, should return an error
        let result = resolve_start_category("NOT_A_CATEGORY");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unknown ledger category")
        );
    }

    #[test]
    fn test_display_git_commit_command_without_signoff() {
        let result = display_git_commit_command("feat: add new feature", false);
        assert_eq!(result, "git commit -m \"feat: add new feature\"");
    }

    #[test]
    fn test_display_git_commit_command_with_signoff() {
        let result = display_git_commit_command("fix: resolve bug", true);
        assert_eq!(result, "git commit -m \"fix: resolve bug\" --signoff");
    }

    #[test]
    fn test_display_git_commit_command_escapes_double_quotes() {
        let result = display_git_commit_command("feat: add \"important\" feature", false);
        assert_eq!(
            result,
            "git commit -m \"feat: add \\\"important\\\" feature\""
        );
    }
}
