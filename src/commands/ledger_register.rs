use crate::cli::ValidatorSubcommands;
use crate::commands::helpers::get_layout;
use crate::ledger::db::LedgerDb;
use crate::ledger::enforcement::{
    CategoryStackMapping, CommitValidator, RuleType, TechStackRule, WatcherPattern,
};
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use chrono::Utc;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};

pub fn execute_validator_lifecycle(subcommand: ValidatorSubcommands) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only(&layout)?;
    let db = LedgerDb::new(storage.get_connection());

    match subcommand {
        ValidatorSubcommands::List { json } => {
            let validators = db
                .get_commit_validators(None)
                .map_err(|e| miette::miette!("{}", e))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&validators).into_diagnostic()?
                );
            } else {
                println!(
                    "{}",
                    "Registered Commit Validators"
                        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
                );
                let mut table = Table::new();
                table.set_header(vec!["Name", "Category", "Executable", "Enabled", "Level"]);
                for v in validators {
                    table.add_row(vec![
                        v.name
                            .if_supports_color(Stream::Stdout, |s| s.bold())
                            .to_string(),
                        v.category,
                        v.executable,
                        if v.enabled {
                            "YES"
                                .if_supports_color(Stream::Stdout, |s| s.green())
                                .to_string()
                        } else {
                            "no".if_supports_color(Stream::Stdout, |s| s.red())
                                .to_string()
                        },
                        format!("{:?}", v.validation_level),
                    ]);
                }
                println!("{}", table);
            }
        }
        ValidatorSubcommands::Enable { name } => {
            db.set_validator_enabled(&name, true)
                .map_err(|e| miette::miette!("{}", e))?;
            println!("Enabled validator: {}", name);
        }
        ValidatorSubcommands::Disable { name } => {
            db.set_validator_enabled(&name, false)
                .map_err(|e| miette::miette!("{}", e))?;
            println!("Disabled validator: {}", name);
        }
        ValidatorSubcommands::Remove { name } => {
            db.remove_validator(&name)
                .map_err(|e| miette::miette!("{}", e))?;
            println!("Removed validator: {}", name);
        }
        ValidatorSubcommands::Doctor => {
            let validators = db
                .get_commit_validators(None)
                .map_err(|e| miette::miette!("{}", e))?;
            println!(
                "\n{}",
                "Commit Validator Doctor Report"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
            );
            let mut all_ok = true;
            for v in validators {
                print!(
                    "  Validator {}: ",
                    v.name.if_supports_color(Stream::Stdout, |s| s.bold())
                );
                let exe = v.executable.trim();
                let exists = if exe.is_empty() {
                    false
                } else {
                    let path = std::path::Path::new(exe);
                    if path.exists() {
                        true
                    } else {
                        crate::util::which::which(exe).is_some()
                    }
                };

                if !v.enabled {
                    println!(
                        "{}",
                        "DISABLED".if_supports_color(Stream::Stdout, |s| s.yellow())
                    );
                } else if exists {
                    println!("{}", "OK".if_supports_color(Stream::Stdout, |s| s.green()));
                } else {
                    println!(
                        "{} (Executable '{}' not found)",
                        "MISSING/ERROR".if_supports_color(Stream::Stdout, |s| s.red()),
                        exe
                    );
                    all_ok = false;
                }
            }
            if all_ok {
                println!(
                    "\n{}",
                    "All enabled validators are healthy!"
                        .if_supports_color(Stream::Stdout, |s| s.green())
                );
            } else {
                println!(
                    "\n{}",
                    "Some enabled validators have issues. Please check the paths."
                        .if_supports_color(Stream::Stdout, |s| s.red())
                );
            }
        }
    }
    Ok(())
}

/// Internal programmatic API for web/MCP server. Not exposed as CLI — takes raw JSON payload that bypasses clap type-safety. Use `register_rule`/`register_validator` for CLI access.
#[doc(hidden)]
#[allow(dead_code)]
pub(crate) fn execute_ledger_register(
    rule_type: RuleType,
    payload: String,
    force: bool,
) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::init_with_layout(&layout)?;
    let db = LedgerDb::new(storage.get_connection());

    match rule_type {
        RuleType::TechStack => {
            let mut rule: TechStackRule = serde_json::from_str(&payload)
                .map_err(|e| miette::miette!("Invalid JSON payload for TECH_STACK: {}", e))?;

            // Validation
            if rule.category.trim().is_empty() {
                return Err(miette::miette!("Category cannot be empty"));
            }
            if rule.name.trim().is_empty() {
                return Err(miette::miette!("Name cannot be empty"));
            }

            if rule.registered_at.is_empty() {
                rule.registered_at = Utc::now().to_rfc3339();
            }

            let existing = db
                .get_tech_stack_rule(&rule.category)
                .map_err(|e| miette::miette!("{}", e))?;

            if matches!(existing, Some(ref rule_info) if rule_info.locked && !force) {
                return Err(miette::miette!(
                    "Rule for category {} is locked. Use --force to override.",
                    rule.category
                        .if_supports_color(Stream::Stdout, |s| s.yellow())
                ));
            }
            db.insert_tech_stack_rule(&rule)
                .map_err(|e| miette::miette!("{}", e))?;
            println!(
                "Registered tech stack rule for category: {}",
                rule.category
                    .if_supports_color(Stream::Stdout, |s| s.cyan())
            );
        }
        RuleType::Validator => {
            let validator: CommitValidator = serde_json::from_str(&payload)
                .map_err(|e| miette::miette!("Invalid JSON payload for VALIDATOR: {}", e))?;

            // Validation
            if validator.category.trim().is_empty() {
                return Err(miette::miette!("Category cannot be empty"));
            }
            if validator.name.trim().is_empty() {
                return Err(miette::miette!("Validator name cannot be empty"));
            }
            if validator.executable.trim().is_empty() {
                return Err(miette::miette!("Executable cannot be empty"));
            }
            if validator.timeout_ms <= 0 {
                return Err(miette::miette!("timeout_ms must be positive"));
            }

            db.insert_commit_validator(&validator)
                .map_err(|e| miette::miette!("{}", e))?;
            println!(
                "Registered commit validator: {}",
                validator
                    .name
                    .if_supports_color(Stream::Stdout, |s| s.cyan())
            );
        }
        RuleType::Mapping => {
            let mapping: CategoryStackMapping = serde_json::from_str(&payload)
                .map_err(|e| miette::miette!("Invalid JSON payload for MAPPING: {}", e))?;

            // Validation
            if mapping.ledger_category.trim().is_empty() {
                return Err(miette::miette!("ledger_category cannot be empty"));
            }
            if mapping.stack_category.trim().is_empty() {
                return Err(miette::miette!("stack_category cannot be empty"));
            }

            db.insert_category_mapping(&mapping)
                .map_err(|e| miette::miette!("{}", e))?;
            println!(
                "Registered category mapping: {} -> {}",
                mapping
                    .ledger_category
                    .if_supports_color(Stream::Stdout, |s| s.cyan()),
                mapping
                    .stack_category
                    .if_supports_color(Stream::Stdout, |s| s.cyan())
            );
        }
        RuleType::Watcher => {
            let pattern: WatcherPattern = serde_json::from_str(&payload)
                .map_err(|e| miette::miette!("Invalid JSON payload for WATCHER: {}", e))?;

            // Validation
            if pattern.category.trim().is_empty() {
                return Err(miette::miette!("Category cannot be empty"));
            }
            if pattern.glob.trim().is_empty() {
                return Err(miette::miette!("Watcher glob cannot be empty"));
            }

            db.insert_watcher_pattern(&pattern)
                .map_err(|e| miette::miette!("{}", e))?;
            println!(
                "Registered watcher pattern: {}",
                pattern.glob.if_supports_color(Stream::Stdout, |s| s.cyan())
            );
        }
    }

    Ok(())
}
