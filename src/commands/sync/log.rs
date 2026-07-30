use miette::{Result, miette};
use std::fs::File;
use std::io::{BufRead, BufReader};

pub fn handle(tail: Option<usize>) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;
    let log_path = layout.state_dir.join("sync").join("sync.log");

    if !log_path.exists() {
        println!("No sync log found at {log_path}");
        return Ok(());
    }

    let file = File::open(log_path.as_std_path())
        .map_err(|e| miette!("Failed to open log file: {}", e))?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();

    let limit = tail.unwrap_or(20);
    let start = if lines.len() > limit {
        lines.len() - limit
    } else {
        0
    };

    println!("Recent Sync Logs ({log_path}):");
    for line in &lines[start..] {
        println!("{}", line);
    }

    Ok(())
}
