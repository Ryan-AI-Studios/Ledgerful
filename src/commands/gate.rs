use crate::state::layout::Layout;

pub(crate) const GATE_MODE_SHOW_OBSERVE_HINT: &str =
    "observe warns and does not block; set with `ledgerful gate mode enforce`.";
pub(crate) const GATE_MODE_SHOW_ENFORCE_HINT: &str =
    "enforce blocks; set with `ledgerful gate mode observe`.";
pub(crate) const GATE_MODE_SHOW_UNKNOWN_HINT: &str = "observe warns and does not block; enforce blocks; set with `ledgerful gate mode observe|enforce`.";

pub(crate) fn gate_mode_show_hint(mode: &str) -> &'static str {
    if mode.eq_ignore_ascii_case("observe") {
        GATE_MODE_SHOW_OBSERVE_HINT
    } else if mode.eq_ignore_ascii_case("enforce") {
        GATE_MODE_SHOW_ENFORCE_HINT
    } else {
        GATE_MODE_SHOW_UNKNOWN_HINT
    }
}

pub(crate) fn print_gate_mode_show(mode: &str) {
    println!("Gate mode: {mode}");
    println!("{}", gate_mode_show_hint(mode));
}

pub fn write_mode_transition_entry(
    layout: &Layout,
    old_mode: &str,
    new_mode: &str,
) -> miette::Result<()> {
    use crate::ledger::{
        Category, ChangeType, CommitRequest, EntryType, TransactionManager, TransactionRequest,
    };
    use crate::state::storage::StorageManager;

    let mut storage = StorageManager::init_with_layout(layout)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_mode_show_hint_observe_warns() {
        assert_eq!(gate_mode_show_hint("observe"), GATE_MODE_SHOW_OBSERVE_HINT);
        assert_eq!(gate_mode_show_hint("OBSERVE"), GATE_MODE_SHOW_OBSERVE_HINT);
    }

    #[test]
    fn gate_mode_show_hint_enforce_blocks() {
        assert_eq!(gate_mode_show_hint("enforce"), GATE_MODE_SHOW_ENFORCE_HINT);
        assert_eq!(gate_mode_show_hint("ENFORCE"), GATE_MODE_SHOW_ENFORCE_HINT);
    }

    #[test]
    fn gate_mode_show_hint_unknown_is_generic() {
        assert_eq!(gate_mode_show_hint(""), GATE_MODE_SHOW_UNKNOWN_HINT);
        assert_eq!(gate_mode_show_hint("weird"), GATE_MODE_SHOW_UNKNOWN_HINT);
    }
}
