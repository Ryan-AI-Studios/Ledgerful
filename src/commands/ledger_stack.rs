use crate::commands::helpers::get_layout;
use crate::ledger::*;
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::{OwoColorize, Stream, Style};

pub fn execute_ledger_stack(category: Option<String>) -> Result<()> {
    let layout = get_layout()?;
    let storage = StorageManager::open_read_only_sqlite_only(&layout)?;
    let db = LedgerDb::new(storage.get_connection());

    println!(
        "{}",
        "Ledgerful Tech Stack & Validators"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );

    let rules = db
        .get_tech_stack_rules(category.as_deref())
        .map_err(|e| miette::miette!("{}", e))?;
    println!(
        "\n{}",
        "TECH STACK RULES"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
    );
    if rules.is_empty() {
        println!("  None.");
    } else {
        for rule in rules {
            let locked_str = if rule.locked {
                " [LOCKED]"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
                    .to_string()
            } else {
                "".to_string()
            };
            println!(
                "  {} ({}): {}{}",
                rule.category
                    .if_supports_color(Stream::Stdout, |s| s.yellow()),
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
    println!(
        "\n{}",
        "COMMIT VALIDATORS"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().magenta().bold()))
    );
    if validators.is_empty() {
        println!("  None.");
    } else {
        for v in validators {
            let enabled_str = if !v.enabled {
                " [DISABLED]"
                    .if_supports_color(Stream::Stdout, |s| s.dimmed())
                    .to_string()
            } else {
                "".to_string()
            };
            println!(
                "  {} ({:?}): {} {}{}",
                v.name.if_supports_color(Stream::Stdout, |s| s.yellow()),
                v.validation_level,
                v.executable,
                v.args.join(" "),
                enabled_str
            );
            if let Some(desc) = v.description {
                println!(
                    "    Description: {}",
                    desc.if_supports_color(Stream::Stdout, |s| s.dimmed())
                );
            }
            if let Some(glob) = v.glob {
                println!("    Scope: {}", glob);
            }
        }
    }

    let mappings = db
        .get_category_mappings(category.as_deref())
        .map_err(|e| miette::miette!("{}", e))?;
    println!(
        "\n{}",
        "CATEGORY MAPPINGS"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().blue().bold()))
    );
    if mappings.is_empty() {
        println!("  None.");
    } else {
        for m in mappings {
            println!(
                "  {} -> {}",
                m.ledger_category
                    .if_supports_color(Stream::Stdout, |s| s.yellow()),
                m.stack_category
                    .if_supports_color(Stream::Stdout, |s| s.cyan())
            );
            if let Some(desc) = m.description {
                println!(
                    "    {}",
                    desc.if_supports_color(Stream::Stdout, |s| s.dimmed())
                );
            }
        }
    }

    Ok(())
}
