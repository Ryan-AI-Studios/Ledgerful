#[cfg(feature = "mcp")]
pub mod manifest;
#[cfg(feature = "mcp")]
pub mod sanitize;
#[cfg(feature = "mcp")]
pub mod server;
#[cfg(feature = "mcp")]
pub mod tools;

#[cfg(feature = "mcp")]
pub use manifest::{INVENTORY, ToolDescriptor, get_tool_count};

#[cfg(feature = "mcp")]
pub fn execute_mcp_server() -> miette::Result<()> {
    // Redirect tracing to stderr to avoid polluting stdout
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .try_init();

    server::run_server()
}

#[cfg(not(feature = "mcp"))]
pub fn execute_mcp_server() -> miette::Result<()> {
    miette::bail!("MCP feature not enabled. Rebuild with --features mcp")
}

#[cfg(not(feature = "mcp"))]
pub fn get_tool_count() -> usize {
    0
}
