use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::ledger::{LedgerDb, TransactionManager};
use crate::state::storage::StorageManager;
use miette::Result;
use owo_colors::OwoColorize;

pub fn execute_ledger_register_rule(term: &str, category: &str, reason: &str) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    let db = LedgerDb::new(tx_mgr.get_connection());
    db.register_forbidden_term(term, category, reason)
        .map_err(|e| miette::miette!("{}", e))?;

    println!(
        "Rule registered: NO {} in {}",
        term.red().bold(),
        category.yellow()
    );
    Ok(())
}

pub fn execute_ledger_register_validator(
    name: &str,
    command: &str,
    category: &str,
    timeout: u64,
) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::init(layout.state_subdir().join("ledger.db").as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let tx_mgr = TransactionManager::new(&mut storage, layout.root.into(), config);

    let db = LedgerDb::new(tx_mgr.get_connection());
    db.register_validator(name, command, category, timeout)
        .map_err(|e| miette::miette!("{}", e))?;

    println!(
        "Validator registered: {} for {}",
        name.cyan().bold(),
        category.yellow()
    );
    Ok(())
}
