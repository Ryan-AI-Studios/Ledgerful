use crate::docs::types::{DocTemplate, query_symbol_table, write_file};
use crate::state::storage_cozo::CozoStorage;
use camino::{Utf8Path, Utf8PathBuf};
use miette::Result;

pub struct SymbolTableTemplate;
pub struct SymbolIndexTemplate;
pub struct ApiContractIndexTemplate;

impl DocTemplate for SymbolTableTemplate {
    fn name(&self) -> &'static str {
        "symbol_table"
    }

    fn description(&self) -> &'static str {
        "Markdown table of indexed symbols"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let rows = query_symbol_table(storage)?;
        let mut lines = vec!["# Symbol Table".to_string(), String::new()];

        let mut current_file: Option<&str> = None;
        const MAX_ROWS: usize = 10_000;

        for (count, row) in rows.iter().enumerate() {
            if count >= MAX_ROWS {
                break;
            }

            let file = row.file_path.as_str();
            if current_file != Some(file) {
                current_file = Some(file);
                lines.push(format!("### {file}"));
                lines.push(String::new());
                lines.push(
                    "| Qualified Name | Symbol Name | Kind | Line Range | Public |".to_string(),
                );
                lines.push("|---|---|---|---|---|".to_string());
            }

            let line_range = format!("{}-{}", row.line_start, row.line_end);
            let public_str = if row.is_public { "Yes" } else { "No" };
            lines.push(format!(
                "| {} | {} | {} | {} | {} |",
                row.qualified_name, row.symbol_name, row.symbol_kind, line_range, public_str
            ));
        }

        if rows.len() > MAX_ROWS {
            lines.push(String::new());
            lines.push("> ... truncated".to_string());
        }

        if rows.is_empty() {
            lines.push("*No symbols indexed.*".to_string());
        }

        let content = lines.join("\n") + "\n";
        let path = output_dir.join("symbol_table.md");
        write_file(&path, &content)?;
        Ok(path)
    }
}

impl DocTemplate for SymbolIndexTemplate {
    fn name(&self) -> &'static str {
        "symbol_index"
    }

    fn description(&self) -> &'static str {
        "Comprehensive index of all symbols"
    }

    fn generate(&self, storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        let rows = query_symbol_table(storage)?;
        let mut lines = vec![
            "# Symbol Index".to_string(),
            String::new(),
            "| Qualified Name | Symbol Name | Kind | File Path | Line Start | Line End | Public |"
                .to_string(),
            "|---|---|---|---|---|---|---|".to_string(),
        ];

        for row in rows {
            let public_str = if row.is_public { "Yes" } else { "No" };
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} | {} |",
                row.qualified_name,
                row.symbol_name,
                row.symbol_kind,
                row.file_path,
                row.line_start,
                row.line_end,
                public_str
            ));
        }

        let content = lines.join("\n") + "\n";
        let path = output_dir.join("symbol_index.md");
        write_file(&path, &content)?;
        Ok(path)
    }
}

impl DocTemplate for ApiContractIndexTemplate {
    fn name(&self) -> &'static str {
        "api_contract"
    }

    fn description(&self) -> &'static str {
        "Index of API contracts and endpoints"
    }

    fn generate(&self, _storage: &CozoStorage, output_dir: &Utf8Path) -> Result<Utf8PathBuf> {
        // Mocking metadata extraction for now
        let content =
            "# API Contract Index\n\n*API documentation pending metadata extraction logic.*\n";
        let path = output_dir.join("api_contract_index.md");
        write_file(&path, content)?;
        Ok(path)
    }
}
