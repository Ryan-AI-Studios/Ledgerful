use owo_colors::OwoColorize;

pub const HEADER_WIDTH: usize = 60;

pub fn print_header(title: &str) {
    println!("\n{}", title.bold().bright_cyan());
    println!("{}", "=".repeat(title.len().max(HEADER_WIDTH)).cyan());
}

pub fn success_marker() -> String {
    "SUCCESS".green().bold().to_string()
}

pub fn failure_marker() -> String {
    "FAILED".red().bold().to_string()
}

pub fn warning_marker() -> String {
    "WARNING".yellow().bold().to_string()
}

pub fn info_marker() -> String {
    "INFO".blue().bold().to_string()
}

pub fn error_banner(message: &str) {
    println!("\n{}", "ERROR".red().bold());
    println!("{}", "=".repeat(40).red());
    println!("{}", message.red());
}

pub fn warning_banner(message: &str) {
    println!("\n{}", "WARNING".yellow().bold());
    println!("{}", "=".repeat(40).yellow());
    println!("{}", message.yellow());
}
