use crate::ui::intent_tui::{IntentState, run_tui};
use miette::{IntoDiagnostic, Result};

pub fn execute_intent_demo() -> Result<()> {
    use std::io::IsTerminal;
    if !crate::util::term::is_interactive() || !std::io::stdout().is_terminal() {
        miette::bail!("Cannot launch intent demo: terminal is non-interactive or not a TTY");
    }

    let mock_state = IntentState::new(
        "Refactor API authentication endpoints".to_string(),
        "Replace custom JWT verification with standard OAuth2 middleware to improve security and audit compliance.".to_string(),
        "MEDIUM".to_string(),
        vec!["SEC-451".to_string(), "ADR-12".to_string()],
        0.45,
    );

    println!("Launching Ledgerful Intent TUI Demo...");
    if let Some(final_state) = run_tui(mock_state).into_diagnostic()? {
        println!("\nAccepted Intent State:");
        println!("WHAT:       {}", final_state.what);
        println!("WHY:        {}", final_state.why);
        println!("RISK:       {}", final_state.risk);
        println!("RELATED:    {:?}", final_state.related);
        println!("CONFIDENCE: {:.2}", final_state.confidence);
    } else {
        println!("\nAborted intent entry.");
    }
    Ok(())
}
