//! `ledgerful release pins` — GitHub Latest vs packaging / npm / tap pins (0201).

mod pins;

use miette::Result;

/// Diff GitHub Latest (tag + archive digests) against in-tree packaging
/// templates, live Homebrew/Scoop remotes, and npm `ledgerfulEngineTag`.
///
/// Prints JSON or a human table, then `process::exit` (0 match / 1 drift /
/// 2 skipped or unverified). JSON is written before exit.
pub fn execute_release_pins(json: bool) -> Result<()> {
    let envelope = pins::collect_release_pins()?;
    pins::emit_release_pins(&envelope, json)?;
    std::process::exit(pins::exit_code_for(envelope.status));
}
