//! Pure JSON/TOML merge helpers for MCP host config files.
//!
//! JSON reads use `jsonc_parser` (comments + trailing commas). Writes emit
//! pretty `serde_json` (comments are not preserved on rewrite — acceptable for v1).

use serde_json::{Map, Value};
use toml_edit::{Array, DocumentMut, Item, Table, value};

pub const SERVER_NAME: &str = "ledgerful";

/// Entry written for JSON hosts (Claude/Cursor). Copilot adds `"type":"stdio"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    pub command: String,
    pub args: Vec<String>,
    /// When true, include `"type": "stdio"` (Copilot / VS Code).
    pub include_type_stdio: bool,
}

impl ServerEntry {
    pub fn to_json_value(&self) -> Value {
        let mut map = Map::new();
        if self.include_type_stdio {
            map.insert("type".to_string(), Value::String("stdio".to_string()));
        }
        map.insert("command".to_string(), Value::String(self.command.clone()));
        map.insert(
            "args".to_string(),
            Value::Array(self.args.iter().map(|a| Value::String(a.clone())).collect()),
        );
        Value::Object(map)
    }

    pub fn from_json_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let command = obj.get("command")?.as_str()?.to_string();
        let args = obj
            .get("args")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let include_type_stdio = obj
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t == "stdio");
        Some(Self {
            command,
            args,
            include_type_stdio,
        })
    }

    /// True when command/args match (type flag ignored for equality of launcher shape).
    pub fn same_launcher_shape(&self, other: &Self) -> bool {
        self.command == other.command && self.args == other.args
    }
}

/// Parse host JSON (JSONC-tolerant). Empty/whitespace content → empty object.
/// Top-level `null`, arrays, and scalars are rejected (must be an object).
pub fn parse_jsonc(content: &str) -> Result<Value, String> {
    if content.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    // jsonc-parser 0.32: `parse_to_serde_value` → Result<Value, ParseError>
    // (some older call sites match Option; this crate version returns Value directly).
    match jsonc_parser::parse_to_serde_value(content, &jsonc_parser::ParseOptions::default()) {
        Ok(v) => {
            let v: Value = v;
            if v.is_object() {
                Ok(v)
            } else {
                Err("top-level JSON value must be an object".to_string())
            }
        }
        Err(e) => Err(format!("JSONC parse error: {e}")),
    }
}

/// Upsert `parent_key.ledgerful` with `entry`. Preserves sibling keys.
pub fn merge_json_server(
    root: &mut Value,
    parent_key: &str,
    entry: &ServerEntry,
) -> Result<(), String> {
    let obj = root
        .as_object_mut()
        .ok_or_else(|| "top-level JSON value must be an object".to_string())?;

    let parent = obj
        .entry(parent_key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let parent_obj = parent.as_object_mut().ok_or_else(|| {
        format!("key `{parent_key}` exists but is not an object; refusing to overwrite")
    })?;
    parent_obj.insert(SERVER_NAME.to_string(), entry.to_json_value());
    Ok(())
}

/// Remove only `parent_key.ledgerful`. Returns true if an entry was present.
pub fn remove_json_server(root: &mut Value, parent_key: &str) -> Result<bool, String> {
    let obj = root
        .as_object_mut()
        .ok_or_else(|| "top-level JSON value must be an object".to_string())?;
    let Some(parent) = obj.get_mut(parent_key) else {
        return Ok(false);
    };
    let parent_obj = parent
        .as_object_mut()
        .ok_or_else(|| format!("key `{parent_key}` exists but is not an object"))?;
    Ok(parent_obj.remove(SERVER_NAME).is_some())
}

/// Read existing ledgerful entry under `parent_key`, if any.
pub fn get_json_server(root: &Value, parent_key: &str) -> Option<ServerEntry> {
    let obj = root.as_object()?;
    let parent = obj.get(parent_key)?.as_object()?;
    ServerEntry::from_json_value(parent.get(SERVER_NAME)?)
}

/// Pretty-print JSON for write (deterministic key order not guaranteed by serde_json
/// unless preserve_order feature is on — Cargo already uses preserve_order).
pub fn serialize_json(root: &Value) -> Result<String, String> {
    let mut s = serde_json::to_string_pretty(root).map_err(|e| e.to_string())?;
    s.push('\n');
    Ok(s)
}

// ── TOML (Codex) ────────────────────────────────────────────────────────────

/// Upsert `[mcp_servers.ledgerful]` with command + args array.
pub fn merge_toml_server(doc: &mut DocumentMut, entry: &ServerEntry) -> Result<(), String> {
    let root = doc.as_table_mut();
    let mcp_servers = ensure_table(root, "mcp_servers")?;
    let ledgerful = ensure_table(mcp_servers, SERVER_NAME)?;
    ledgerful.insert("command", value(entry.command.clone()));
    let mut arr = Array::new();
    for a in &entry.args {
        arr.push(a.as_str());
    }
    ledgerful.insert("args", Item::Value(arr.into()));
    Ok(())
}

/// Remove only `[mcp_servers.ledgerful]`. Returns true if present.
pub fn remove_toml_server(doc: &mut DocumentMut) -> bool {
    let Some(Item::Table(mcp_servers)) = doc.as_table_mut().get_mut("mcp_servers") else {
        return false;
    };
    mcp_servers.remove(SERVER_NAME).is_some()
}

/// Read existing `[mcp_servers.ledgerful]` if any.
pub fn get_toml_server(doc: &DocumentMut) -> Option<ServerEntry> {
    let mcp_servers = doc.get("mcp_servers")?.as_table()?;
    let table = mcp_servers.get(SERVER_NAME)?.as_table()?;
    let command = table.get("command")?.as_str()?.to_string();
    let args = table
        .get("args")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Some(ServerEntry {
        command,
        args,
        include_type_stdio: false,
    })
}

fn ensure_table<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table, String> {
    if !parent.contains_key(key) {
        let mut t = Table::new();
        t.set_implicit(false);
        parent.insert(key, Item::Table(t));
    }
    match parent.get_mut(key) {
        Some(Item::Table(t)) => Ok(t),
        Some(_) => Err(format!(
            "key `{key}` exists but is not a table; refusing to overwrite"
        )),
        None => Err(format!("failed to create table `{key}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> ServerEntry {
        ServerEntry {
            command: "/usr/bin/ledgerful".to_string(),
            args: vec!["mcp".to_string()],
            include_type_stdio: false,
        }
    }

    #[test]
    fn json_merge_multi_server_preserves_foreign() {
        let mut root = parse_jsonc(
            r#"{
  "mcpServers": {
    "other": { "command": "foo" },
  }
}"#,
        )
        .expect("jsonc with trailing comma");
        merge_json_server(&mut root, "mcpServers", &sample_entry()).expect("merge");
        let servers = root["mcpServers"].as_object().expect("obj");
        assert!(servers.contains_key("other"));
        assert!(servers.contains_key("ledgerful"));
        assert_eq!(
            servers["ledgerful"]["command"].as_str(),
            Some("/usr/bin/ledgerful")
        );
        assert_eq!(
            servers["ledgerful"]["args"],
            Value::Array(vec![Value::String("mcp".to_string())])
        );
    }

    #[test]
    fn json_merge_jsonc_comment_and_trailing_comma() {
        let src = r#"{
  // comment kept by parser, dropped on rewrite
  "mcpServers": {
    "x": { "command": "x" },
  },
}"#;
        let mut root = parse_jsonc(src).expect("jsonc");
        merge_json_server(&mut root, "mcpServers", &sample_entry()).expect("merge");
        assert!(get_json_server(&root, "mcpServers").is_some());
    }

    #[test]
    fn json_merge_claude_user_top_level_only_not_projects() {
        let mut root = parse_jsonc(
            r#"{
  "projects": {
    "/tmp/repo": {
      "mcpServers": { "local-only": { "command": "local" } }
    }
  },
  "mcpServers": {
    "peer": { "command": "peer" }
  }
}"#,
        )
        .expect("parse");
        merge_json_server(&mut root, "mcpServers", &sample_entry()).expect("merge");
        // Top-level has ledgerful
        assert!(get_json_server(&root, "mcpServers").is_some());
        // projects map unchanged (no ledgerful injected under projects)
        let projects = root["projects"]["/tmp/repo"]["mcpServers"]
            .as_object()
            .expect("projects mcpServers");
        assert!(!projects.contains_key("ledgerful"));
        assert!(projects.contains_key("local-only"));
        assert!(root["mcpServers"].as_object().unwrap().contains_key("peer"));
    }

    #[test]
    fn json_merge_copilot_servers_with_type_stdio() {
        let mut root = Value::Object(Map::new());
        let entry = ServerEntry {
            command: "ledgerful".to_string(),
            args: vec!["mcp".to_string()],
            include_type_stdio: true,
        };
        merge_json_server(&mut root, "servers", &entry).expect("merge");
        assert!(root.get("mcpServers").is_none());
        let lf = &root["servers"]["ledgerful"];
        assert_eq!(lf["type"].as_str(), Some("stdio"));
        assert_eq!(lf["command"].as_str(), Some("ledgerful"));
    }

    #[test]
    fn json_remove_only_ledgerful() {
        let mut root =
            parse_jsonc(r#"{"mcpServers":{"ledgerful":{"command":"x"},"other":{"command":"y"}}}"#)
                .expect("parse");
        assert!(remove_json_server(&mut root, "mcpServers").expect("rm"));
        assert!(!remove_json_server(&mut root, "mcpServers").expect("idempotent"));
        assert!(
            root["mcpServers"]
                .as_object()
                .unwrap()
                .contains_key("other")
        );
        assert!(
            !root["mcpServers"]
                .as_object()
                .unwrap()
                .contains_key("ledgerful")
        );
    }

    #[test]
    fn json_reject_non_object_top_level() {
        let err = parse_jsonc("[1,2,3]").unwrap_err();
        assert!(err.contains("object"));
    }

    #[test]
    fn parse_jsonc_null_is_err_empty_is_object() {
        let err = parse_jsonc("null").unwrap_err();
        assert!(
            err.contains("object"),
            "top-level null must not become {{}}: {err}"
        );
        let empty = parse_jsonc("").expect("empty → object");
        assert!(empty.as_object().is_some_and(|m| m.is_empty()));
        let ws = parse_jsonc("  \n\t  ").expect("whitespace → object");
        assert!(ws.as_object().is_some_and(|m| m.is_empty()));
    }

    #[test]
    fn json_reject_parent_key_not_object() {
        let mut root = parse_jsonc(r#"{"mcpServers":"nope"}"#).expect("parse");
        let err = merge_json_server(&mut root, "mcpServers", &sample_entry()).unwrap_err();
        assert!(err.contains("not an object"));
    }

    #[test]
    fn toml_merge_mcp_servers_ledgerful_table() {
        let mut doc: DocumentMut = r#"
[other]
x = 1

[mcp_servers.peer]
command = "peer"
"#
        .parse()
        .expect("toml");
        merge_toml_server(&mut doc, &sample_entry()).expect("merge");
        let got = get_toml_server(&doc).expect("entry");
        assert_eq!(got.command, "/usr/bin/ledgerful");
        assert_eq!(got.args, vec!["mcp".to_string()]);
        // peer preserved
        assert!(doc["mcp_servers"]["peer"].as_table().is_some());
        assert_eq!(doc["other"]["x"].as_integer(), Some(1));
    }

    #[test]
    fn toml_remove_only_ledgerful() {
        let mut doc: DocumentMut = r#"
[mcp_servers.ledgerful]
command = "x"
args = ["mcp"]

[mcp_servers.peer]
command = "peer"
"#
        .parse()
        .expect("toml");
        assert!(remove_toml_server(&mut doc));
        assert!(!remove_toml_server(&mut doc));
        assert!(get_toml_server(&doc).is_none());
        assert!(doc["mcp_servers"]["peer"].as_table().is_some());
    }
}
