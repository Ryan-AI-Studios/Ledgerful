use crate::ui::intent_tui::{IntentState, run_tui};
use miette::{IntoDiagnostic, Result};

const INTENT_DEMO_REFUSE: &str =
    "Cannot launch intent demo: terminal is non-interactive or not a TTY";
const INTENT_DEMO_NEXT: &str = "Next: run `ledgerful intent demo` in an interactive terminal. This command is a TUI demo, not a non-interactive workflow.";

pub(crate) fn format_intent_demo_refuse() -> String {
    format!("{INTENT_DEMO_REFUSE}\n{INTENT_DEMO_NEXT}")
}

pub fn execute_intent_demo() -> Result<()> {
    use std::io::IsTerminal;
    if !crate::util::term::is_interactive() || !std::io::stdout().is_terminal() {
        miette::bail!("{}", format_intent_demo_refuse());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_demo_refuse_names_tty_next() {
        let s = format_intent_demo_refuse();
        assert!(s.starts_with(INTENT_DEMO_REFUSE));
        assert!(s.contains('\n'));
        assert!(s.contains(INTENT_DEMO_NEXT));
        assert_eq!(s, format!("{INTENT_DEMO_REFUSE}\n{INTENT_DEMO_NEXT}"));
        assert!(!s.contains(&format!("{INTENT_DEMO_REFUSE} {INTENT_DEMO_NEXT}")));
    }
}
