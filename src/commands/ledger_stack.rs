use crate::commands::helpers::get_layout;
use crate::ledger::*;
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::OwoColorize;

pub fn execute_ledger_stack(category: Option<String>) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only_sqlite_only(&layout.root)?;
    let db = LedgerDb::new(storage.get_connection());

    println!("{}", "Ledgerful Tech Stack & Validators".bold().underline());

    let rules = db
        .get_tech_stack_rules(category.as_deref())
        .map_err(|e| miette::miette!("{}", e))?;
    println!("\n{}", "TECH STACK RULES".cyan().bold());
    if rules.is_empty() {
        println!("  None.");
    } else {
        for rule in rules {
            let locked_str = if rule.locked {
                " [LOCKED]".red().bold().to_string()
            } else {
                "".to_string()
            };
            println!(
                "  {} ({}): {}{}",
                rule.category.yellow(),
                rule.name,
                rule.status,
                locked_str
            );
            for r in rule.rules {
                println!("    - {}", r);
            }
        }
    }

    let validators = db
        .get_commit_validators(category.as_deref())
        .map_err(|e| miette::miette!("{}", e))?;
    println!("\n{}", "COMMIT VALIDATORS".magenta().bold());
    if validators.is_empty() {
        println!("  None.");
    } else {
        for v in validators {
            let enabled_str = if !v.enabled {
                " [DISABLED]".dimmed().to_string()
            } else {
                "".to_string()
            };
            println!(
                "  {} ({:?}): {} {}{}",
                v.name.yellow(),
                v.validation_level,
                v.executable,
                v.args.join(" "),
                enabled_str
            );
            if let Some(desc) = v.description {
                println!("    Description: {}", desc.dimmed());
            }
            if let Some(glob) = v.glob {
                println!("    Scope: {}", glob);
            }
        }
    }

    let mappings = db
        .get_category_mappings(category.as_deref())
        .map_err(|e| miette::miette!("{}", e))?;
    println!("\n{}", "CATEGORY MAPPINGS".blue().bold());
    if mappings.is_empty() {
        println!("  None.");
    } else {
        for m in mappings {
            println!(
                "  {} -> {}",
                m.ledger_category.yellow(),
                m.stack_category.cyan()
            );
            if let Some(desc) = m.description {
                println!("    {}", desc.dimmed());
            }
        }
    }

    Ok(())
}
