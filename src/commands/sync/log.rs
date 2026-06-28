use miette::{Result, miette};
use std::fs::File;
use std::io::{BufRead, BufReader};

pub fn handle(tail: Option<usize>) -> Result<()> {
    let cg_dir = std::env::current_dir()
        .map_err(|e| miette!("Failed to get current dir: {}", e))?
        .join(".ledgerful");
    let log_path = cg_dir.join("sync").join("sync.log");

    if !log_path.exists() {
        println!("No sync log found at {}", log_path.display());
        return Ok(());
    }

    let file = File::open(&log_path).map_err(|e| miette!("Failed to open log file: {}", e))?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    let limit = tail.unwrap_or(20);
    let start = if lines.len() > limit {
        lines.len() - limit
    } else {
        0
    };

    println!("Recent Sync Logs ({}):", log_path.display());
    for line in &lines[start..] {
        println!("{}", line);
    }

    Ok(())
}
