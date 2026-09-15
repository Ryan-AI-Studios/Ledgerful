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
