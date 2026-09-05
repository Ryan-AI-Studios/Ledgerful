use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};

pub(super) fn execute_hotspots_budget(
    storage: &StorageManager,
    _config: &crate::config::model::Config,
    json: bool,
) -> Result<()> {
    let conn = storage.get_connection();

    let mut stmt = conn
        .prepare(
            "SELECT file_path, score FROM hotspot_history \
         WHERE timestamp = (SELECT MAX(timestamp) FROM hotspot_history) \
         ORDER BY score DESC",
        )
        .into_diagnostic()?;

    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .into_diagnostic()?;

    let mut violations = Vec::new();
    let threshold = 5.0;

    for row in rows {
        let (path, score) = row.into_diagnostic()?;
        if score > threshold {
            violations.push(serde_json::json!({
                "path": path,
                "score": score,
                "threshold": threshold,
            }));
        }
    }

    if json {
        crate::output::json::emit(&serde_json::json!({
            "status": if violations.is_empty() { "OK" } else { "VIOLATION" },
            "violations": violations,
        }))
        .map_err(|e| miette::miette!("Failed to serialize budget check: {}", e))?;
    } else {
        println!(
            "{}",
            "Hotspot Budget Check"
                .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().cyan()))
        );
        if violations.is_empty() {
            println!(
                "  Status: {}",
                "OK".if_supports_color(Stream::Stdout, |s| s.green())
            );
            println!("  All hotspots within risk budget.");
        } else {
            println!(
                "  Status: {}",
                "VIOLATION"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
            );
            for v in &violations {
                let path = v["path"].as_str().unwrap_or("(unknown)");
                let score = v["score"].as_f64().unwrap_or(0.0);
                let threshold = v["threshold"].as_f64().unwrap_or(5.0);
                println!(
                    "  ! {} exceeds budget: {:.2} > {:.2}",
                    path.if_supports_color(Stream::Stdout, |s| s.yellow()),
                    score,
                    threshold,
                );
            }
        }
    }

    Ok(())
}
