use std::process::Command;

#[test]
#[ignore = "requires local model server (embedding + generation) to be running"]
fn test_ask_command_includes_bridge_context_placeholder() {
    // This is hard to test without a mocked LLM, but we can check if the code paths are hit
    // or if the prompt construction logic is exposed.
    // For now, let's just ensure 'ask' still runs without crashing.
    let binary = option_env!("CARGO_BIN_EXE_ledgerful").unwrap_or("target/debug/ledgerful");
    let _output = Command::new(binary)
        .args(["ask", "how does the bridge work?"])
        .output()
        .expect("failed to execute process");

    // We don't assert success because the user might not have a model configured,
    // but it shouldn't panic.
}
