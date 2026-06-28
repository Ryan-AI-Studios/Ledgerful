use crate::commands::helpers::get_layout;
use crate::output::table::Table;
use crate::state::storage::StorageManager;
use clap::{Args, Subcommand};
use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use serde::Serialize;

#[derive(Args, Debug)]
pub struct DependenciesArgs {
    #[command(subcommand)]
    pub command: DependencySubcommands,
}

#[derive(Subcommand, Debug)]
pub enum DependencySubcommands {
    /// List all project dependencies and their versions
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show detailed output including external dependencies
        #[arg(short, long)]
        verbose: bool,
    },
    /// Audit dependencies for known vulnerabilities (requires OSV-Scanner JSON)
    Audit {
        /// Path to OSV-Scanner JSON output
        #[arg(short, long)]
        input: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn execute_dependencies(args: DependenciesArgs) -> Result<()> {
    let layout = get_layout()?;

    match args.command {
        DependencySubcommands::List { json, verbose } => {
            let storage = StorageManager::open_read_only(&layout.root)?;
            let cozo = storage
                .cozo
                .as_ref()
                .ok_or_else(|| miette::miette!("CozoDB storage is unavailable"))?;
            let res = cozo.run_script("?[id, name, metadata] := *node{id: id, label: name, category: 'package', metadata: metadata}")?;

            #[derive(Serialize, Clone, Debug)]
            struct ListedDep {
                name: String,
                version: String,
                ecosystem: String,
                source: Option<String>,
                is_local: bool,
            }

            use std::collections::HashMap;

            // Bucket by (name, version, ecosystem) only. Two ingestion paths populate
            // package nodes for the same real-world package with different metadata
            // completeness: Cargo.lock ingestion (graph_loader) always records the
            // lockfile's `source`, while the OSV advisory importer creates nodes with
            // no `source` field at all. That metadata gap must not be mistaken for a
            // genuine identity difference, so buckets are reconciled below: entries
            // are merged unless two of them carry distinct, both-populated sources
            // (the real local-override-vs-registry collision the dedup key must
            // protect against).
            let mut buckets: HashMap<(String, String, String), Vec<ListedDep>> = HashMap::new();

            for row in res.rows {
                if let (
                    Some(cozo::DataValue::Str(_id)),
                    Some(cozo::DataValue::Str(label)),
                    Some(cozo::DataValue::Json(meta)),
                ) = (row.first(), row.get(1), row.get(2))
                {
                    let clean_name = meta
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            if let Some(idx) = label.rfind('@') {
                                if idx > 0 {
                                    label[..idx].to_string()
                                } else {
                                    label.to_string()
                                }
                            } else {
                                label.to_string()
                            }
                        });

                    let version = meta
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-")
                        .to_string();
                    let ecosystem = meta
                        .get("ecosystem")
                        .and_then(|v| v.as_str())
                        .unwrap_or("-")
                        .to_string();
                    let source = meta
                        .get("source")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let is_local = source.is_none();

                    let key = (clean_name.clone(), version.clone(), ecosystem.clone());
                    let current_dep = ListedDep {
                        name: clean_name,
                        version,
                        ecosystem,
                        source,
                        is_local,
                    };

                    buckets.entry(key).or_default().push(current_dep);
                }
            }

            // Reconcile each (name, version, ecosystem) bucket: entries merge together
            // as long as they don't carry two different non-null sources. `None` is
            // treated as compatible with anything (a metadata-completeness gap, not a
            // distinct identity), so the first known source in a compatible group wins
            // and that group collapses to a single row. Only when a bucket contains
            // genuinely different non-null sources do we emit more than one row, each
            // preserving its own distinct source.
            let mut deps_list: Vec<ListedDep> = Vec::new();
            for (_key, entries) in buckets {
                let mut groups: Vec<ListedDep> = Vec::new();
                for entry in entries {
                    let compatible = groups.iter_mut().find(|g: &&mut ListedDep| {
                        g.source.is_none() || entry.source.is_none() || g.source == entry.source
                    });
                    match compatible {
                        Some(group) => {
                            // Prefer a known source over a missing one; once any
                            // member of the group has a source, the merged row is no
                            // longer local.
                            if group.source.is_none() && entry.source.is_some() {
                                group.source = entry.source;
                                group.is_local = false;
                            }
                        }
                        None => groups.push(entry),
                    }
                }
                deps_list.extend(groups);
            }
            deps_list.sort_by(|a, b| {
                let local_cmp = b.is_local.cmp(&a.is_local);
                if local_cmp != std::cmp::Ordering::Equal {
                    return local_cmp;
                }
                let name_cmp = a.name.cmp(&b.name);
                if name_cmp != std::cmp::Ordering::Equal {
                    return name_cmp;
                }
                let version_cmp = a.version.cmp(&b.version);
                if version_cmp != std::cmp::Ordering::Equal {
                    return version_cmp;
                }
                a.ecosystem.cmp(&b.ecosystem)
            });

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&deps_list).into_diagnostic()?
                );
            } else {
                println!(
                    "{}",
                    "Project Dependencies (from Knowledge Graph)".bold().green()
                );

                let (local_deps, external_deps): (Vec<&ListedDep>, Vec<&ListedDep>) =
                    deps_list.iter().partition(|d| d.is_local);

                println!("\nLocal Dependencies:");
                let mut local_table = Table::new();
                local_table.set_header(vec!["Package", "Version", "Ecosystem"]);
                for dep in &local_deps {
                    local_table.add_row(vec![
                        dep.name.clone(),
                        dep.version.clone(),
                        dep.ecosystem.clone(),
                    ]);
                }
                println!("{}", local_table);

                if verbose {
                    println!("\nExternal Dependencies:");
                    let mut ext_table = Table::new();
                    ext_table.set_header(vec!["Package", "Version", "Ecosystem", "Source"]);
                    for dep in &external_deps {
                        let source_str = dep.source.as_deref().unwrap_or("-").to_string();
                        ext_table.add_row(vec![
                            dep.name.clone(),
                            dep.version.clone(),
                            dep.ecosystem.clone(),
                            source_str,
                        ]);
                    }
                    println!("{}", ext_table);
                } else {
                    println!("\nExternal dependencies count: {}", external_deps.len());
                }
            }
        }
        DependencySubcommands::Audit { input, json } => {
            let path = std::path::Path::new(&input);
            if !path.exists() {
                return Err(miette::miette!("Input file not found: {}", input));
            }

            let result = crate::index::advisories::OsvImporter::import_from_json(path)?;

            // Open writeable storage to populate KG
            let db_path = layout.state_subdir().join("ledger.db");
            let storage = StorageManager::init(db_path.as_std_path())?;
            if let Some(cozo) = &storage.cozo {
                crate::index::advisories::OsvImporter::populate_kg(cozo, &result, "audit-tx")?;
            }

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).into_diagnostic()?
                );
            } else {
                println!("{}", "Security Advisory Audit (OSV)".bold().red());
                let mut table = Table::new();
                table.set_header(vec!["Package", "Version", "Vulnerability", "Summary"]);

                for src_res in &result.results {
                    for pkg_res in &src_res.packages {
                        if let Some(vulns) = &pkg_res.vulnerabilities {
                            for vuln in vulns {
                                table.add_row(vec![
                                    pkg_res.package.name.clone(),
                                    pkg_res.package.version.clone(),
                                    vuln.id.red().to_string(),
                                    vuln.summary.as_deref().unwrap_or("-").to_string(),
                                ]);
                            }
                        }
                    }
                }
                println!("{}", table);
            }
        }
    }

    Ok(())
}
