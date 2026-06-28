use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;

pub fn update_federated_link(
    conn: &Connection,
    sibling_name: &str,
    sibling_path: &str,
    timestamp: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO federated_links (sibling_name, sibling_path, last_scanned_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(sibling_name) DO UPDATE SET
            sibling_path = excluded.sibling_path,
            last_scanned_at = excluded.last_scanned_at",
        (sibling_name, sibling_path, timestamp),
    )
    .into_diagnostic()?;
    Ok(())
}

pub fn save_federated_dependencies(
    conn: &Connection,
    sibling_name: &str,
    local_symbol: &str,
    sibling_symbol: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO federated_dependencies (local_symbol, sibling_name, sibling_symbol)
         VALUES (?1, ?2, ?3)",
        (local_symbol, sibling_name, sibling_symbol),
    )
    .into_diagnostic()?;
    Ok(())
}

pub fn clear_federated_dependencies(conn: &Connection, sibling_name: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM federated_dependencies WHERE sibling_name = ?1",
        [sibling_name],
    )
    .into_diagnostic()?;
    Ok(())
}

pub fn get_federated_links(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn
        .prepare("SELECT sibling_name, sibling_path, last_scanned_at FROM federated_links ORDER BY sibling_name")
        .into_diagnostic()?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .into_diagnostic()?;

    let mut links = Vec::new();
    for link in rows {
        links.push(link.into_diagnostic()?);
    }
    Ok(links)
}

pub fn get_dependencies_for_sibling(
    conn: &Connection,
    sibling_name: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare("SELECT local_symbol, sibling_symbol FROM federated_dependencies WHERE sibling_name = ?1 ORDER BY local_symbol, sibling_symbol")
        .into_diagnostic()?;
    let rows = stmt
        .query_map([sibling_name], |row| Ok((row.get(0)?, row.get(1)?)))
        .into_diagnostic()?;

    let mut deps = Vec::new();
    for dep in rows {
        deps.push(dep.into_diagnostic()?);
    }
    Ok(deps)
}
