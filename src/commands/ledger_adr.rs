use camino::Utf8PathBuf;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use std::fs;

use crate::cli::AdrSubcommands;
use crate::commands::helpers::{get_layout, load_ledger_config};
use crate::ledger::adr::{generate_madr_content, slugify_summary};
use crate::ledger::transaction::TransactionManager;
use crate::ledger::types::{AdrMetadataUpdate, AdrStatus};
use crate::output::json;
use crate::output::table::build_table;
use crate::state::storage::StorageManager;
use serde::Serialize;

pub fn execute_ledger_adr(subcommand: AdrSubcommands) -> Result<()> {
    let layout = get_layout()?;
    let mut storage = StorageManager::init_with_layout(&layout)?;
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
        AdrSubcommands::List { json, status } => execute_ledger_adr_list(&storage, json, status),
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
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LedgerAdrListJson {
    schema_version: u32,
    kind: &'static str,
    result_count: usize,
    items: Vec<LedgerAdrListItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LedgerAdrListItem {
    id: i64,
    tx_id: String,
    entity: String,
    status: String,
    title: String,
    committed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    supersedes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
}

struct AdrListRow {
    id: i64,
    tx_id: String,
    entity: String,
    status: String,
    title: String,
    committed_at: String,
    supersedes: Option<String>,
    superseded_by: Option<String>,
}

fn execute_ledger_adr_list(
    storage: &StorageManager,
    json_mode: bool,
    status_filter: Option<AdrStatus>,
) -> Result<()> {
    let rows = load_adr_list_rows(storage, status_filter)?;

    if json_mode {
        return json::emit(&LedgerAdrListJson {
            schema_version: 1,
            kind: "ledgerAdr",
            result_count: rows.len(),
            items: rows.iter().map(adr_list_item).collect(),
        });
    }

    if rows.is_empty() {
        println!(
            "{}",
            "No ADRs found.".if_supports_color(Stream::Stdout, |s| s.yellow())
        );
        return Ok(());
    }

    let mut table = build_table(vec!["ID", "Entity", "Status", "Title", "Created"]);
    for row in &rows {
        table.add_row(vec![
            row.id
                .to_string()
                .if_supports_color(Stream::Stdout, |s| s.yellow())
                .to_string(),
            row.entity
                .if_supports_color(Stream::Stdout, |s| s.cyan())
                .to_string(),
            row.status.clone(),
            row.title.clone(),
            row.committed_at
                .if_supports_color(Stream::Stdout, |s| s.dimmed())
                .to_string(),
        ]);
    }
    println!("{}", table);
    Ok(())
}

fn load_adr_list_rows(
    storage: &StorageManager,
    status_filter: Option<AdrStatus>,
) -> Result<Vec<AdrListRow>> {
    let conn = storage.get_connection();
    let mut stmt = conn
        .prepare(
            "SELECT le.id, le.tx_id, le.entity, COALESCE(am.status, 'proposed'), le.summary,
                    le.committed_at, am.supersedes, am.superseded_by
             FROM ledger_entries le
             LEFT JOIN adr_metadata am ON le.tx_id = am.adr_id
             WHERE le.entry_type = 'ARCHITECTURE' OR le.is_breaking = 1
             ORDER BY le.committed_at DESC, le.tx_id DESC",
        )
        .map_err(|e| miette::miette!("Failed to query ADRs: {}", e))?;

    let mut rows = stmt
        .query_map([], |row| {
            Ok(AdrListRow {
                id: row.get(0)?,
                tx_id: row.get(1)?,
                entity: row.get(2)?,
                status: row.get(3)?,
                title: row.get(4)?,
                committed_at: row.get(5)?,
                supersedes: row.get(6)?,
                superseded_by: row.get(7)?,
            })
        })
        .map_err(|e| miette::miette!("Failed to read ADRs: {}", e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| miette::miette!("Failed to collect ADRs: {}", e))?;

    if let Some(wanted) = status_filter {
        let wanted = serde_json::to_string(&wanted)
            .map_err(|e| miette::miette!("Failed to serialize ADR status: {e}"))?
            .trim_matches('"')
            .to_string();
        rows.retain(|row| row.status == wanted);
    }
    Ok(rows)
}

fn adr_list_item(row: &AdrListRow) -> LedgerAdrListItem {
    LedgerAdrListItem {
        id: row.id,
        tx_id: row.tx_id.clone(),
        entity: row.entity.clone(),
        status: row.status.clone(),
        title: row.title.clone(),
        committed_at: row.committed_at.clone(),
        supersedes: nonempty_opt(&row.supersedes),
        superseded_by: nonempty_opt(&row.superseded_by),
    }
}

fn nonempty_opt(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr_list_json_kind_ledger_adr() {
        let item = adr_list_item(&AdrListRow {
            id: 7,
            tx_id: "aabbccdd".into(),
            entity: "docs/arch.md".into(),
            status: "proposed".into(),
            title: "Decide".into(),
            committed_at: "2026-01-01T00:00:00Z".into(),
            supersedes: Some("older".into()),
            superseded_by: None,
        });
        let body = serde_json::to_value(&LedgerAdrListJson {
            schema_version: 1,
            kind: "ledgerAdr",
            result_count: 1,
            items: vec![item],
        })
        .expect("json");
        assert_eq!(body["schemaVersion"], 1);
        assert_eq!(body["kind"], "ledgerAdr");
        assert_eq!(body["resultCount"], 1);
        assert_eq!(body["items"][0]["id"], 7);
        assert!(body["items"][0]["id"].is_number());
        assert_eq!(body["items"][0]["txId"], "aabbccdd");
        assert_eq!(body["items"][0]["supersedes"], "older");
        assert!(body["items"][0].get("supersededBy").is_none());
    }

    #[test]
    fn adr_list_omits_supersedes_when_unlinked() {
        let item = adr_list_item(&AdrListRow {
            id: 1,
            tx_id: "tx".into(),
            entity: "docs/arch.md".into(),
            status: "accepted".into(),
            title: "Decide".into(),
            committed_at: "2026-01-01T00:00:00Z".into(),
            supersedes: Some("  ".into()),
            superseded_by: None,
        });
        let value = serde_json::to_value(item).expect("item");
        assert!(value.get("supersedes").is_none());
        assert!(value.get("supersededBy").is_none());
    }

    #[test]
    fn adr_list_status_filter_proposed() {
        let wanted = serde_json::to_string(&AdrStatus::Proposed)
            .expect("status")
            .trim_matches('"')
            .to_string();
        assert_eq!(wanted, "proposed");
        let rows = [
            AdrListRow {
                id: 1,
                tx_id: "a".into(),
                entity: "docs/a.md".into(),
                status: "proposed".into(),
                title: "A".into(),
                committed_at: "2026-01-02T00:00:00Z".into(),
                supersedes: None,
                superseded_by: None,
            },
            AdrListRow {
                id: 2,
                tx_id: "b".into(),
                entity: "docs/b.md".into(),
                status: "accepted".into(),
                title: "B".into(),
                committed_at: "2026-01-01T00:00:00Z".into(),
                supersedes: None,
                superseded_by: None,
            },
        ];
        let filtered: Vec<_> = rows
            .into_iter()
            .filter(|row| row.status == wanted)
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, 1);
    }
}
