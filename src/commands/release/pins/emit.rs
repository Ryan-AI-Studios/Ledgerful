use super::types::{PinStatus, ReleasePinsEnvelope};
use crate::output::table::{Table, apply_table_style, resolve_table_style};
use miette::{IntoDiagnostic, Result};
use serde_json::Value;

pub(crate) fn exit_code_for(status: PinStatus) -> i32 {
    match status {
        PinStatus::Match => 0,
        PinStatus::Drift => 1,
        PinStatus::Skipped | PinStatus::Unverified => 2,
    }
}

fn compact_cell(v: &Value) -> String {
    if v.is_null() {
        return "-".to_string();
    }
    if let Some(tag) = v.get("ledgerfulEngineTag").and_then(Value::as_str) {
        return tag.to_string();
    }
    if let Some(ver) = v.get("version").and_then(Value::as_str) {
        return ver.to_string();
    }
    if let Some(hash) = v.get("hash").and_then(Value::as_str) {
        return hash.chars().take(12).collect();
    }
    if v.as_object().is_some_and(|o| o.is_empty()) {
        return "-".to_string();
    }
    "-".to_string()
}

fn print_human_table(envelope: &ReleasePinsEnvelope) {
    if envelope.status == PinStatus::Skipped {
        println!("Not a Ledgerful engine worktree; release pins is engine-only.");
        println!("Overall: {}", envelope.status.as_str());
        return;
    }
    let mut table = Table::new();
    apply_table_style(&mut table, resolve_table_style());
    table.set_header(vec!["Surface", "Status", "Local", "Expected", "Remote"]);
    for s in &envelope.surfaces {
        table.add_row(vec![
            s.id.clone(),
            s.status.as_str().to_string(),
            compact_cell(&s.local),
            compact_cell(&s.expected),
            compact_cell(&s.remote),
        ]);
    }
    println!("{table}");
    println!("Overall: {}", envelope.status.as_str());
}

pub(crate) fn emit_release_pins(envelope: &ReleasePinsEnvelope, json: bool) -> Result<()> {
    if json {
        let body = serde_json::to_string_pretty(envelope).into_diagnostic()?;
        println!("{body}");
    } else {
        print_human_table(envelope);
    }
    Ok(())
}
