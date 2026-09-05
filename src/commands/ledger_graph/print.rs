use super::LedgerGraphArgs;
use super::assemble::LedgerGraphData;
use crate::output::table::Table;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};

pub(super) fn print_ledger_graph(
    args: &LedgerGraphArgs,
    full_id: &str,
    data: &LedgerGraphData,
) -> Result<()> {
    if args.json {
        println!("{}", serde_json::to_string_pretty(data).into_diagnostic()?);
    } else {
        println!(
            "{} {}",
            "Graph neighborhood for transaction:".if_supports_color(Stream::Stdout, |s| s.bold()),
            full_id.if_supports_color(Stream::Stdout, |s| s.cyan())
        );

        println!(
            "\n{}",
            "Exact Relations"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
        );
        if data.exact.is_empty() {
            println!("  None.");
        } else {
            let mut table = Table::new();
            table.set_header(vec![
                "Entity ID",
                "Label",
                "Category",
                "Relation",
                "Attribution Source",
            ]);
            for r in &data.exact {
                table.add_row(vec![
                    r.entity_id.clone(),
                    r.label.clone(),
                    r.category.clone(),
                    r.relation.clone(),
                    r.attribution_source.clone(),
                ]);
            }
            println!("{}", table);
        }

        println!(
            "\n{}",
            "Derived Relations (Transitive / Structural Neighborhood)"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
        );
        if data.derived.is_empty() {
            println!("  None.");
        } else {
            let mut table = Table::new();
            table.set_header(vec![
                "Entity ID",
                "Label",
                "Category",
                "Relation",
                "Attribution Source",
            ]);
            for r in &data.derived {
                table.add_row(vec![
                    r.entity_id.clone(),
                    r.label.clone(),
                    r.category.clone(),
                    r.relation.clone(),
                    r.attribution_source.clone(),
                ]);
            }
            println!("{}", table);
        }

        println!(
            "\n{}",
            "Heuristic Fallbacks"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
        );
        if data.heuristic.is_empty() {
            println!("  None.");
        } else {
            let mut table = Table::new();
            table.set_header(vec![
                "Entity ID",
                "Label",
                "Category",
                "Relation",
                "Attribution Source",
            ]);
            for r in &data.heuristic {
                table.add_row(vec![
                    r.entity_id.clone(),
                    r.label.clone(),
                    r.category.clone(),
                    r.relation.clone(),
                    r.attribution_source.clone(),
                ]);
            }
            println!("{}", table);
        }
    }

    Ok(())
}
