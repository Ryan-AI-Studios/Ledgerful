use crate::impact::packet::ChangedFile;
use crate::index::normalize::normalize_repo_path;
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;

use crate::index::symbols::SymbolKind;

pub fn get_public_symbols(conn: &Connection) -> Result<Vec<crate::state::storage::StoredSymbol>> {
    let mut stmt = conn
        .prepare(
            "SELECT file_path, symbol_name, symbol_kind, is_public FROM symbols 
             WHERE snapshot_id = (SELECT MAX(id) FROM snapshots) AND is_public = 1
             ORDER BY file_path, symbol_name",
        )
        .into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok(crate::state::storage::StoredSymbol {
                file_path: row.get(0)?,
                name: row.get(1)?,
                kind: match row.get::<_, String>(2)?.as_str() {
                    "Function" => SymbolKind::Function,
                    "Method" => SymbolKind::Method,
                    "Class" => SymbolKind::Class,
                    "Struct" => SymbolKind::Struct,
                    "Enum" => SymbolKind::Enum,
                    "Trait" => SymbolKind::Trait,
                    "Interface" => SymbolKind::Interface,
                    "Type" => SymbolKind::Type,
                    "Variable" => SymbolKind::Variable,
                    "Constant" => SymbolKind::Constant,
                    "Module" => SymbolKind::Module,
                    _ => SymbolKind::Function, // Fallback
                },
                is_public: row.get::<_, i32>(3)? != 0,
            })
        })
        .into_diagnostic()?;

    let mut symbols = Vec::new();
    for symbol in rows {
        symbols.push(symbol.into_diagnostic()?);
    }
    Ok(symbols)
}

pub fn persist_symbols(conn: &Connection, snapshot_id: i64, files: &[ChangedFile]) -> Result<()> {
    for file in files {
        let Some(symbols) = &file.symbols else {
            continue;
        };

        let file_path = normalize_repo_path(&file.path);
        for symbol in symbols {
            let metadata = if symbol.metadata.is_empty() {
                None
            } else {
                serde_json::to_string(&symbol.metadata).ok()
            };

            conn.execute(
                "INSERT INTO symbols (snapshot_id, file_path, symbol_name, symbol_kind, is_public, cognitive_complexity, cyclomatic_complexity, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                (
                    snapshot_id,
                    &file_path,
                    &symbol.name,
                    format!("{:?}", symbol.kind),
                    symbol.is_public as i32,
                    symbol.cognitive_complexity,
                    symbol.cyclomatic_complexity,
                    metadata,
                ),
            )
            .into_diagnostic()?;
        }
    }

    Ok(())
}
