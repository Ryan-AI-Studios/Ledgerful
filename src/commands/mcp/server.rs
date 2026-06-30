use crate::commands::mcp::INVENTORY;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Read, Write};

const MAX_MCP_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    pub id: Option<Value>,
}

pub fn run_server() -> miette::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut reader = stdin.lock();

    loop {
        let mut length = None;
        let mut header = String::new();
        let mut saw_header_line = false;

        // Read headers. Leading blank lines are skipped rather than treated
        // as an empty header block: clients commonly leave a trailing
        // newline after the last frame's body (e.g. PowerShell appends one
        // when piping a string into stdin), and without this, that dangling
        // newline gets misread as a malformed, header-less message instead
        // of harmless padding before EOF.
        loop {
            header.clear();
            let n = reader
                .read_line(&mut header)
                .map_err(|e| miette::miette!(e))?;
            if n == 0 {
                return Ok(()); // EOF
            }
            let trimmed = header.trim();
            if trimmed.is_empty() {
                if saw_header_line {
                    break; // End of headers
                }
                continue; // Skip leading blank/separator line
            }
            saw_header_line = true;
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                length = Some(
                    len_str
                        .parse::<usize>()
                        .map_err(|e| miette::miette!("Invalid Content-Length: {}", e))?,
                );
            }
        }

        let len = match length {
            Some(l) => l,
            None => return Err(miette::miette!("Missing Content-Length header")),
        };

        if len > MAX_MCP_MESSAGE_SIZE {
            return Err(miette::miette!(
                "Content-Length {} exceeds maximum {}",
                len,
                MAX_MCP_MESSAGE_SIZE
            ));
        }

        let mut buf = vec![0; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| miette::miette!(e))?;

        let msg_str = String::from_utf8_lossy(&buf);
        if msg_str.len() > MAX_MCP_MESSAGE_SIZE {
            return Err(miette::miette!(
                "MCP message body {} exceeds maximum {}",
                msg_str.len(),
                MAX_MCP_MESSAGE_SIZE
            ));
        }

        // serde_json default recursion limit of 128 provides depth protection for params.
        let req: JsonRpcRequest = match serde_json::from_str(&msg_str) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Failed to parse request: {}", e);
                let error_resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: None,
                    error: Some(serde_json::json!({
                        "code": -32700,
                        "message": "Parse error"
                    })),
                    id: None,
                };
                let resp_json =
                    serde_json::to_string(&error_resp).map_err(|e| miette::miette!(e))?;
                write!(
                    stdout,
                    "Content-Length: {}\r\n\r\n{}",
                    resp_json.len(),
                    resp_json
                )
                .map_err(|e| miette::miette!(e))?;
                stdout.flush().map_err(|e| miette::miette!(e))?;
                continue;
            }
        };

        if let Some(response) = handle_request(req) {
            let resp_json = serde_json::to_string(&response).map_err(|e| miette::miette!(e))?;
            write!(
                stdout,
                "Content-Length: {}\r\n\r\n{}",
                resp_json.len(),
                resp_json
            )
            .map_err(|e| miette::miette!(e))?;
            stdout.flush().map_err(|e| miette::miette!(e))?;
        }
    }
}

fn handle_request(req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    let result = match req.method.as_str() {
        "initialize" => {
            let protocol_version = req
                .params
                .as_ref()
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or("2024-11-05");

            Some(serde_json::json!({
                "protocolVersion": protocol_version,
                "capabilities": {
                    "tools": {
                        "listChanged": false
                    }
                },
                "serverInfo": {
                    "name": "ledgerful",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }))
        }
        "tools/list" => {
            let tools: Vec<Value> = INVENTORY.iter().map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": serde_json::from_str::<Value>(t.schema_json).unwrap_or(serde_json::json!({}))
                })
            }).collect();
            Some(serde_json::json!({
                "tools": tools
            }))
        }
        "tools/call" => {
            let params = req.params.unwrap_or(serde_json::json!({}));
            let name = params["name"].as_str().unwrap_or("");
            let tool_params = params["arguments"].clone();

            Some(crate::commands::mcp::tools::dispatch_tool(
                name,
                tool_params,
            ))
        }
        "ping" => Some(serde_json::json!({})),
        // Respond to notifications (no ID) with nothing, but MCP expects some responses
        // Actually, JSON-RPC notifications don't get responses.
        "notifications/initialized" => {
            return None;
        }
        _ => None,
    };

    // If it was a notification (no ID), don't return a response
    if req.id.is_none() && req.method != "initialize" {
        return None;
    }

    let error = if result.is_none() && req.method != "notifications/initialized" {
        Some(serde_json::json!({
            "code": -32601,
            "message": format!("Method not found: {}", req.method)
        }))
    } else {
        None
    };

    Some(JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result,
        error,
        id: req.id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize_round_trip() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({})),
            id: Some(serde_json::json!(1)),
        };
        let resp = handle_request(req).unwrap();
        assert_eq!(resp.id, Some(serde_json::json!(1)));
        let result = resp.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], "ledgerful");
    }
}
