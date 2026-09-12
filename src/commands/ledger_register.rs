use crate::cli::ValidatorSubcommands;
use crate::commands::helpers::get_layout;
use crate::ledger::db::LedgerDb;
use crate::output::table::Table;
use crate::state::storage::StorageManager;
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
