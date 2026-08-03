use owo_colors::{OwoColorize, Stream, Style};

pub const HEADER_WIDTH: usize = 60;

pub fn print_header(title: &str) {
    println!(
        "\n{}",
        title.if_supports_color(Stream::Stdout, |s| s
            .style(Style::new().bold().bright_cyan()))
    );
    println!(
        "{}",
        "=".repeat(title.len().max(HEADER_WIDTH))
            .if_supports_color(Stream::Stdout, |s| s.cyan())
    );
}

pub fn success_marker() -> String {
    "SUCCESS"
        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
        .to_string()
}

pub fn failure_marker() -> String {
    "FAILED"
        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
        .to_string()
}

pub fn warning_marker() -> String {
    "WARNING"
        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
        .to_string()
}

pub fn info_marker() -> String {
    "INFO"
        .if_supports_color(Stream::Stdout, |s| s.style(Style::new().blue().bold()))
        .to_string()
}

pub fn error_banner(message: &str) {
    println!(
        "\n{}",
        "ERROR".if_supports_color(Stream::Stdout, |s| s.style(Style::new().red().bold()))
    );
    println!(
        "{}",
        "=".repeat(40)
            .if_supports_color(Stream::Stdout, |s| s.red())
    );
    println!("{}", message.if_supports_color(Stream::Stdout, |s| s.red()));
}

pub fn warning_banner(message: &str) {
    println!(
        "\n{}",
        "WARNING".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
    );
    println!(
        "{}",
        "=".repeat(40)
            .if_supports_color(Stream::Stdout, |s| s.yellow())
    );
    println!(
        "{}",
        message.if_supports_color(Stream::Stdout, |s| s.yellow())
    );
}
