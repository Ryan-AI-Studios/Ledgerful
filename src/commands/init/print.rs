pub(super) fn print_init_status_block(gate_mode: &str) {
    use owo_colors::{OwoColorize, Stream, Style};

    println!(
        "\n{}",
        "Ledgerful Status"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    println!(
        "  Gate mode: {}",
        gate_mode.if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
    );

    let has_local_model = std::env::var("OLLAMA_API_KEY").is_ok()
        || std::env::var("OLLAMA_CLOUD_API_KEY").is_ok()
        || std::env::var("GEMINI_API_KEY").is_ok();
    let model_line = if has_local_model {
        "cloud env detected"
    } else {
        "none (run 'ledgerful setup ai' or set GEMINI_API_KEY / OLLAMA_CLOUD_API_KEY)"
    };
    println!("  Model:      {}", model_line);

    let keys_dir = crate::ledger::crypto::get_keys_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_else(|_| "~/.ledgerful/keys".to_string());
    println!("  Keys:       {}", keys_dir);
    println!("  Hooks:      commit-msg, post-commit, pre-push (.git/hooks/)");
    println!(
        "  Pending tx: {}",
        "0".if_supports_color(Stream::Stdout, |s| s.green())
    );
    println!(
        "  Drift:      {}",
        "0".if_supports_color(Stream::Stdout, |s| s.green())
    );
    println!("  Timings:    recorded locally (opt out: ledgerful timings --opt-out)");

    println!(
        "\n{}",
        "Next Steps"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().underline()))
    );
    println!(
        "  1. ledgerful index --incremental    # Index changed files (~5-10s for a medium repo)"
    );
    println!(
        "  2. ledgerful web start              # Launch the local dashboard at http://127.0.0.1:52001"
    );
    println!("  3. ledgerful verify --scope fast    # Run scoped verification on changed files");

    if gate_mode == "observe" {
        println!(
            "\n{} commits are recorded and warned, never blocked. Run 'ledgerful gate mode enforce' when ready.",
            "Notice:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().bold().yellow()))
        );
    }
}
