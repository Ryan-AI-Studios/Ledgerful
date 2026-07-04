use crate::index::env_schema::EnvDeclaration;
use crate::index::env_schema::EnvSourceKind;
use crate::output::table::Table;
use crate::state::layout::Layout;
use miette::{IntoDiagnostic, Result};

pub fn execute_config_schema(json: bool) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let storage = crate::state::storage::StorageManager::open_read_only(&layout.root)?;
    let conn = storage.get_connection();

    let mut stmt = conn.prepare(
        "SELECT var_name, source_kind, required, is_secret, default_value_redacted, description, owner, environment 
         FROM env_declarations ORDER BY var_name ASC"
    ).into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok(EnvDeclaration {
                var_name: row.get(0)?,
                source_kind: serde_json::from_str(&format!("\"{}\"", row.get::<_, String>(1)?))
                    .unwrap_or(EnvSourceKind::Config),
                required: row.get::<_, i32>(2)? != 0,
                is_secret: row.get::<_, i32>(3)? != 0,
                default_value_redacted: row.get(4)?,
                description: row.get(5)?,
                owner: row.get(6)?,
                environment: row.get(7)?,
                confidence: 1.0,
            })
        })
        .into_diagnostic()?;

    if json {
        let mut results = Vec::new();
        for row in rows {
            results.push(row.into_diagnostic()?);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&results).into_diagnostic()?
        );
    } else {
        let mut table = Table::new();
        table.set_header(vec!["Variable", "Source", "Req", "Sec", "Default", "Owner"]);

        for row in rows {
            let d = row.into_diagnostic()?;
            table.add_row(vec![
                d.var_name,
                d.source_kind.to_string(),
                if d.required { "YES" } else { "no" }.to_string(),
                if d.is_secret { "🔒" } else { "-" }.to_string(),
                d.default_value_redacted.unwrap_or_else(|| "-".to_string()),
                d.owner.unwrap_or_else(|| "-".to_string()),
            ]);
        }
        println!("{}", table);
    }

    Ok(())
}
