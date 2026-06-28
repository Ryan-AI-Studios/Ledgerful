use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use std::fs;

use crate::cli::AdrSubcommands;
use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::ledger::adr::{generate_madr_content, slugify_summary};
use crate::ledger::transaction::TransactionManager;
use crate::ledger::types::AdrMetadataUpdate;
use crate::output::table::build_table;
use crate::state::storage::StorageManager;

pub fn execute_ledger_adr(subcommand: AdrSubcommands) -> Result<()> {
    let layout = get_layout()?;
    let db_path = layout.state_subdir().join("ledger.db");
    let mut storage = StorageManager::init(db_path.as_std_path())?;
    let config = load_ledger_config(&layout)?;
    let mut manager = TransactionManager::new(&mut storage, layout.root.clone().into(), config);

    match subcommand {
        AdrSubcommands::Export { output, days } => {
            execute_export(&manager, Some(Utf8PathBuf::from(output)), days, &layout)
        }
        AdrSubcommands::UpdateStatus { adr_id, status } => {
            let full_id = manager
                .resolve_tx_id(&adr_id)
                .map_err(|e| miette::miette!("{}", e))?;
            manager
                .update_adr_metadata(
                    &full_id,
                    AdrMetadataUpdate {
                        status: Some(status),
                        ..Default::default()
                    },
                )
                .map_err(|e| miette::miette!("{}", e))?;
            println!("Updated ADR {} status to {:?}", full_id, status);
            Ok(())
        }
        AdrSubcommands::Link { adr_id, supersedes } => {
            let full_id = manager
                .resolve_tx_id(&adr_id)
                .map_err(|e| miette::miette!("{}", e))?;
            let full_supersedes = manager
                .resolve_tx_id(&supersedes)
                .map_err(|e| miette::miette!("{}", e))?;
            manager
                .link_adr_supersedes(&full_id, &full_supersedes)
                .map_err(|e| miette::miette!("{}", e))?;
            println!("Linked ADR {} as superseding {}", full_id, full_supersedes);
            Ok(())
        }
        AdrSubcommands::Review { adr_id, message } => {
            let full_id = manager
                .resolve_tx_id(&adr_id)
                .map_err(|e| miette::miette!("{}", e))?;
            let now = chrono::Utc::now().to_rfc3339();
            manager
                .update_adr_metadata(
                    &full_id,
                    AdrMetadataUpdate {
                        reviewed_at: Some(now.clone()),
                        ..Default::default()
                    },
                )
                .map_err(|e| miette::miette!("{}", e))?;
            println!(
                "Recorded review for ADR {} at {} {}",
                full_id,
                now,
                message.unwrap_or_default()
            );
            Ok(())
        }
        AdrSubcommands::List => execute_ledger_adr_list(&storage),
    }
}

fn execute_export(
    manager: &TransactionManager,
    output_dir: Option<Utf8PathBuf>,
    days: Option<u64>,
    layout: &crate::state::layout::Layout,
) -> Result<()> {
    let entries = manager
        .get_adr_entries(days)
        .map_err(|e| miette::miette!("{}", e))?;

    if entries.is_empty() {
        println!(
            "{}",
            "No architectural decisions found to export."
                .yellow()
                .bold()
        );
        return Ok(());
    }

    let out_dir = output_dir.unwrap_or_else(|| layout.root.join("docs/adr"));

    if !out_dir.exists() {
        fs::create_dir_all(&out_dir).into_diagnostic()?;
    }

    let mut count = 0;
    for entry in entries {
        let slug = slugify_summary(&entry.summary);
        let filename = format!("{:04}-{}.md", entry.id, slug);
        let file_path = out_dir.join(filename);

        let content = generate_madr_content(&entry);
        fs::write(&file_path, content).into_diagnostic()?;
        count += 1;
    }

    println!("Successfully exported {} ADR(s) to {}", count, out_dir);
    Ok(())
}

fn execute_ledger_adr_list(storage: &StorageManager) -> Result<()> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT le.id, le.entity, COALESCE(am.status, 'proposed'), le.summary, le.committed_at
             FROM ledger_entries le
             LEFT JOIN adr_metadata am ON le.tx_id = am.adr_id
             WHERE le.entry_type = 'ARCHITECTURE' OR le.is_breaking = 1
             ORDER BY le.committed_at DESC",
        )
        .map_err(|e| miette::miette!("Failed to query ADRs: {}", e))?;

    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let entity: String = row.get(1)?;
            let status: String = row.get(2)?;
            let title: String = row.get(3)?;
            let created_at: String = row.get(4)?;
            Ok((id.to_string(), entity, status, title, created_at))
        })
        .map_err(|e| miette::miette!("Failed to read ADRs: {}", e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| miette::miette!("Failed to collect ADRs: {}", e))?;

    if rows.is_empty() {
        println!("{}", "No ADRs found.".yellow());
        return Ok(());
    }

    let mut table = build_table(vec!["ID", "Entity", "Status", "Title", "Created"]);
    for (id, entity, status, title, created_at) in &rows {
        table.add_row(vec![
            id.yellow().to_string(),
            entity.cyan().to_string(),
            status.to_string(),
            title.to_string(),
            created_at.dimmed().to_string(),
        ]);
    }
    println!("{}", table);
    Ok(())
}
