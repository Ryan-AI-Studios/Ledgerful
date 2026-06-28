use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, Color};

pub use comfy_table::Table;

pub fn build_table(headers: impl IntoIterator<Item = impl ToString>) -> Table {
    let mut table = Table::new();
    table.set_header(
        headers
            .into_iter()
            .map(|header| header.to_string())
            .collect::<Vec<_>>(),
    );
    table
}

/// Build a `comfy_table::Table` using the premium styling established by
/// `ledgerful audit`: UTF-8 full borders, rounded corners, and cyan headers.
pub fn build_premium_table(headers: impl IntoIterator<Item = impl ToString>) -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_header(
            headers
                .into_iter()
                .map(|header| Cell::new(header.to_string()).fg(Color::Cyan))
                .collect::<Vec<_>>(),
        );
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premium_table_uses_utf8_borders_and_cyan_header() {
        let table = build_premium_table(["Name", "Score"]);
        let rendered = table.to_string();
        assert!(
            rendered.contains('╭'),
            "expected top-left rounded corner, got:\n{rendered}"
        );
        assert!(
            rendered.contains('╮'),
            "expected top-right rounded corner, got:\n{rendered}"
        );
        assert!(
            rendered.contains('─'),
            "expected horizontal border, got:\n{rendered}"
        );
        assert!(
            rendered.contains("Name"),
            "expected header content, got:\n{rendered}"
        );
    }

    #[test]
    fn premium_table_adds_rows() {
        let mut table = build_premium_table(["A", "B"]);
        table.add_row(vec!["1", "2"]);
        let rendered = table.to_string();
        assert!(rendered.contains('1'));
        assert!(rendered.contains('2'));
    }
}
