#[cfg(feature = "mcp")]
mod tests {
    use crate::common::setup_git_repo;
    use camino::Utf8Path;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use tempfile::tempdir;

    fn get_mcp_binary() -> PathBuf {
        let mut path = std::env::current_exe().unwrap();
        path.pop();
        if path.ends_with("deps") {
            path.pop();
        }
        path.push(format!("ledgerful{}", std::env::consts::EXE_SUFFIX));
        path
    }

    #[allow(dead_code)]
    /// Poll the child stdout for the first MCP message frame. Once the server has
    /// written a valid `Content-Length:` header it is ready to process requests.
    fn wait_for_mcp_ready(reader: &mut BufReader<impl Read>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if line.trim().starts_with("Content-Length: ") {
                        return;
                    }
                }
                Err(_) => break,
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
        }
    }

    #[test]
    fn test_tools_list_returns_ten_tools() {
        let tmp = tempdir().unwrap();
        let _root = Utf8Path::from_path(tmp.path()).unwrap();
        setup_git_repo(tmp.path());

        let tool_count = ledgerful::commands::mcp::get_tool_count();
        assert_eq!(tool_count, 10, "Expected 10 MCP tools in the manifest");
    }

    #[test]
    fn test_initialize_round_trip() {
        let bin_path = get_mcp_binary();

        let mut child = Command::new(bin_path)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to spawn ledgerful mcp");

        let stdin = child.stdin.as_mut().expect("Failed to open stdin");

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            }
        });

        let req_str = request.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{}", req_str.len(), req_str);
        stdin
            .write_all(payload.as_bytes())
            .expect("Failed to write to stdin");

        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout);

        let mut line = String::new();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).expect("Failed to read header");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                length = len_str.parse().unwrap();
            }
        }

        let mut buf = vec![0; length];
        reader.read_exact(&mut buf).expect("Failed to read body");
        let response_str = String::from_utf8_lossy(&buf);

        let _ = child.kill();
        let _ = child.wait();

        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("Failed to parse JSON response");

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["serverInfo"]["name"], "ledgerful");
    }

    #[test]
    fn test_search_round_trip_and_no_stdout_pollution() {
        // Since we wired up `ledgerful mcp`, we test through the main CLI.
        let bin_path = get_mcp_binary();

        let mut child = Command::new(bin_path)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to spawn ledgerful mcp");

        let stdin = child.stdin.as_mut().expect("Failed to open stdin");

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "search",
                "arguments": {
                    "query": "pub fn run_server",
                    "limit": 5
                }
            }
        });

        let req_str = request.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{}", req_str.len(), req_str);
        stdin
            .write_all(payload.as_bytes())
            .expect("Failed to write to stdin");

        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout);

        let mut line = String::new();
        let mut length = 0;
        let mut first_line = true;
        loop {
            line.clear();
            reader.read_line(&mut line).expect("Failed to read header");
            let trimmed = line.trim();
            if first_line {
                assert!(
                    trimmed.starts_with("Content-Length: "),
                    "Expected Content-Length, but got pollution: {}",
                    trimmed
                );
                first_line = false;
            }
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                length = len_str.parse().unwrap();
            }
        }

        let mut buf = vec![0; length];
        reader.read_exact(&mut buf).expect("Failed to read body");
        let response_str = String::from_utf8_lossy(&buf);

        let _ = child.kill();
        let _ = child.wait();

        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("Failed to parse JSON response");
        assert_eq!(response["id"], 2);

        assert!(
            !response["result"]["isError"].as_bool().unwrap_or(false),
            "Tool returned error"
        );
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("Expected text content");
        println!("Search results: {}", text);
        // We just assert that the search tool returned successfully and gave us text.
        // It's brittle to assert on specific index contents like "run_server" because the global index might not be up-to-date.
        assert!(!text.is_empty(), "Search results should not be empty");
    }

    #[test]
    fn test_tools_call_standalone() {
        let bin_path = get_mcp_binary();

        let mut child = Command::new(bin_path)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to spawn ledgerful mcp");

        let stdin = child.stdin.as_mut().expect("Failed to open stdin");

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "search",
                "arguments": {
                    "query": "pub fn run_server",
                    "limit": 5
                }
            }
        });

        let req_str = request.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{}", req_str.len(), req_str);
        stdin
            .write_all(payload.as_bytes())
            .expect("Failed to write to stdin");

        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout);

        let mut line = String::new();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).expect("Failed to read header");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                length = len_str.parse().unwrap();
            }
        }

        let mut buf = vec![0; length];
        reader.read_exact(&mut buf).expect("Failed to read body");
        let response_str = String::from_utf8_lossy(&buf);

        let _ = child.kill();
        let _ = child.wait();

        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("Failed to parse JSON response");
        assert_eq!(response["id"], 3);

        // The search tool may return an error or empty results if no index
        // exists in the current working directory (e.g., on a fresh CI
        // checkout). We only assert that the tool responded with a valid
        // JSON-RPC result, not that it found anything.
        assert!(
            response["result"].is_object(),
            "Expected result object in response, got: {response}"
        );
    }

    #[test]
    fn test_ping_round_trip() {
        let bin_path = get_mcp_binary();

        let mut child = Command::new(bin_path)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to spawn ledgerful mcp");

        let stdin = child.stdin.as_mut().expect("Failed to open stdin");

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "ping"
        });

        let req_str = request.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{}", req_str.len(), req_str);
        stdin
            .write_all(payload.as_bytes())
            .expect("Failed to write to stdin");

        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout);

        let mut line = String::new();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).expect("Failed to read header");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                length = len_str.parse().unwrap();
            }
        }

        let mut buf = vec![0; length];
        reader.read_exact(&mut buf).expect("Failed to read body");
        let response_str = String::from_utf8_lossy(&buf);

        let _ = child.kill();
        let _ = child.wait();

        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("Failed to parse JSON response");
        assert_eq!(response["id"], 4);
        assert!(response.get("error").is_none());
    }

    #[test]
    fn test_malformed_json() {
        let bin_path = get_mcp_binary();

        let mut child = Command::new(bin_path)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to spawn ledgerful mcp");

        let stdin = child.stdin.as_mut().expect("Failed to open stdin");

        let req_str = "{\"jsonrpc\":\"2.0\",\"method\":\"invali"; // Malformed JSON
        let payload = format!("Content-Length: {}\r\n\r\n{}", req_str.len(), req_str);
        stdin
            .write_all(payload.as_bytes())
            .expect("Failed to write to stdin");

        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout);

        let mut line = String::new();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).expect("Failed to read header");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                length = len_str.parse().unwrap();
            }
        }

        let mut buf = vec![0; length];
        reader.read_exact(&mut buf).expect("Failed to read body");
        let response_str = String::from_utf8_lossy(&buf);

        let _ = child.kill();
        let _ = child.wait();

        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("Failed to parse JSON response");
        assert_eq!(response["error"]["code"], -32700);
    }

    #[test]
    fn test_rejects_oversized_content_length() {
        let bin_path = get_mcp_binary();

        let mut child = Command::new(bin_path)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to spawn ledgerful mcp");

        let stdin = child.stdin.as_mut().expect("Failed to open stdin");

        // Content-Length one byte above the 16 MiB cap; body is intentionally short.
        let payload = format!("Content-Length: {}\r\n\r\n{{}}", 16 * 1024 * 1024 + 1);
        stdin
            .write_all(payload.as_bytes())
            .expect("Failed to write to stdin");

        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout);

        // The server should emit no valid JSON-RPC frame because it rejects the length before
        // allocating. It should exit with an error since the message cannot be parsed.
        let response: serde_json::Value = loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break serde_json::Value::Null,
                Ok(_) => {
                    if line.trim().starts_with("Content-Length: ") {
                        let len: usize = line
                            .trim()
                            .strip_prefix("Content-Length: ")
                            .unwrap()
                            .parse()
                            .unwrap();
                        let mut buf = vec![0u8; len];
                        reader.read_exact(&mut buf).unwrap();
                        break serde_json::from_str(&String::from_utf8_lossy(&buf)).unwrap();
                    }
                }
                Err(_) => break serde_json::Value::Null,
            }
        };

        let _ = child.kill();
        let _ = child.wait();

        // If the server processed anything, it should be an error response (-32700 or similar).
        assert!(
            response.get("error").is_some() || response == serde_json::Value::Null,
            "server should reject oversized Content-Length before normal processing"
        );
    }

    #[test]
    fn test_exits_cleanly_after_trailing_newline_at_eof() {
        let bin_path = get_mcp_binary();

        let mut child = Command::new(bin_path)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to spawn ledgerful mcp");

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "ping"
        });
        let req_str = request.to_string();
        // Mimic a client (e.g. PowerShell piping a string into stdin) that
        // leaves a trailing newline after the last frame's body before
        // closing the stream. This previously made the server misread the
        // dangling newline as a header-less message and exit non-zero.
        let payload = format!("Content-Length: {}\r\n\r\n{}\r\n", req_str.len(), req_str);

        {
            let stdin = child.stdin.as_mut().expect("Failed to open stdin");
            stdin
                .write_all(payload.as_bytes())
                .expect("Failed to write to stdin");
        }
        child.stdin.take(); // close stdin so the server sees EOF

        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout);

        let mut line = String::new();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).expect("Failed to read header");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                length = len_str.parse().unwrap();
            }
        }
        let mut buf = vec![0; length];
        reader.read_exact(&mut buf).expect("Failed to read body");
        let response: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&buf)).expect("Failed to parse JSON");
        assert_eq!(response["id"], 6);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().expect("Failed to poll child status") {
                assert!(
                    status.success(),
                    "expected clean exit after trailing newline at EOF, got {:?}",
                    status
                );
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                panic!("ledgerful mcp did not exit after stdin EOF within 10s");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn test_scan_no_repo() {
        // Create an empty temp directory WITH NO REPO STATE
        let tmp = tempdir().unwrap();
        let _root = Utf8Path::from_path(tmp.path()).unwrap();

        let bin_path = get_mcp_binary();

        let mut child = Command::new(bin_path)
            .arg("mcp")
            .current_dir(tmp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Failed to spawn ledgerful mcp");

        // Wait for the server process to be ready before sending the request.
        // The MCP server only writes to stdout in response to a request, so we
        // cannot poll its output without sending one. The process spawn itself is
        // the readiness signal; keep the original minimal sleep capped at 100ms
        // so the stdio pipes have time to settle without a flaky sleep-for-async.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let stdin = child.stdin.as_mut().expect("Failed to open stdin");

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "scan",
                "arguments": {}
            }
        });

        let req_str = request.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{}", req_str.len(), req_str);
        stdin
            .write_all(payload.as_bytes())
            .expect("Failed to write to stdin");

        let stdout = child.stdout.take().expect("Failed to open stdout");
        let mut reader = BufReader::new(stdout);

        let mut line = String::new();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).expect("Failed to read header");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                length = len_str.parse().unwrap();
            }
        }

        let mut buf = vec![0; length];
        reader.read_exact(&mut buf).expect("Failed to read body");
        let response_str = String::from_utf8_lossy(&buf);

        let _ = child.kill();
        let _ = child.wait();

        let response: serde_json::Value =
            serde_json::from_str(&response_str).expect("Failed to parse JSON response");
        assert_eq!(response["id"], 5);
        assert!(
            response["result"]["isError"].as_bool().unwrap_or(false),
            "Tool should return error"
        );
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("Expected text content");
        assert!(
            text.contains("Failed to get layout")
                || text.contains("Failed to discover git repository"),
            "Expected 'Failed to get layout' or 'Failed to discover git repository' in error message"
        );
        assert!(
            text.contains("run `ledgerful init`"),
            "Expected hint in error message"
        );
    }
}
