use crate::state::layout::Layout;

pub fn write_mode_transition_entry(
    layout: &Layout,
    old_mode: &str,
    new_mode: &str,
) -> miette::Result<()> {
    use crate::ledger::{
        Category, ChangeType, CommitRequest, EntryType, TransactionManager, TransactionRequest,
    };
    use crate::state::storage::StorageManager;

    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path())?;
    let config = crate::commands::helpers::load_ledger_config(layout)?;
    let mut tx_mgr = TransactionManager::new(&mut storage, layout.root.clone().into(), config);

    let tx_id = tx_mgr
        .start_change(TransactionRequest {
            category: Category::Chore,
            entity: "ledgerful/gate-mode".to_string(),
            planned_action: Some(format!(
                "Gate mode transition: {} -> {}",
                old_mode, new_mode
            )),
            ..Default::default()
        })
        .map_err(|e| miette::miette!("{}", e))?;

    tx_mgr
        .commit_change(
            tx_id.clone(),
            CommitRequest {
                change_type: ChangeType::Modify,
                summary: format!("Gate mode changed from {} to {}", old_mode, new_mode),
                reason: "Mode transition via 'ledgerful gate mode'".to_string(),
                entry_type: Some(EntryType::Maintenance),
                ..Default::default()
            },
            false,
        )
        .map_err(|e| miette::miette!("{}", e))?;

    Ok(())
}
