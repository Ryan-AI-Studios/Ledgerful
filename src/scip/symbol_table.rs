use crate::index::symbols::SymbolKind as CGSymbolKind;
use crate::index::types::ProjectSymbol;
use chrono::Utc;
use scip::types::{Occurrence, SymbolInformation, symbol_information::Kind};

pub struct ScipSymbolMapper;

impl ScipSymbolMapper {
    /// Maps a SCIP symbol and occurrence to a Ledgerful ProjectSymbol.
    pub fn map_to_project_symbol(
        file_id: i64,
        symbol_info: &SymbolInformation,
        occurrence: &Occurrence,
    ) -> ProjectSymbol {
        let kind = match symbol_info.kind.enum_value_or_default() {
            Kind::Class => CGSymbolKind::Class,
            Kind::Interface => CGSymbolKind::Interface,
            Kind::Struct => CGSymbolKind::Struct,
            Kind::Enum => CGSymbolKind::Enum,
            Kind::Method => CGSymbolKind::Method,
            Kind::Function => CGSymbolKind::Function,
            Kind::Variable => CGSymbolKind::Variable,
            Kind::Constant => CGSymbolKind::Constant,
            Kind::Module => CGSymbolKind::Module,
            Kind::Trait => CGSymbolKind::Trait,
            _ => CGSymbolKind::Type,
        };

        let (line_start, _col_start, line_end, _col_end) = if occurrence.range.len() == 4 {
            (
                Some(occurrence.range[0]),
                Some(occurrence.range[1]),
                Some(occurrence.range[2]),
                Some(occurrence.range[3]),
            )
        } else if occurrence.range.len() == 3 {
            (
                Some(occurrence.range[0]),
                Some(occurrence.range[1]),
                Some(occurrence.range[0]),
                Some(occurrence.range[2]),
            )
        } else {
            (None, None, None, None)
        };

        // Generate a deterministic signature hash
        let signature_hash = blake3::hash(symbol_info.symbol.as_bytes())
            .to_hex()
            .to_string();

        ProjectSymbol {
            id: None,
            file_id,
            qualified_name: symbol_info.symbol.clone(),
            symbol_name: symbol_info.display_name.clone(),
            symbol_kind: kind.as_str().to_string(),
            visibility: None, // SCIP has accessibility but it's complex to map
            entrypoint_kind: "Internal".to_string(), // Default
            is_public: true,  // SCIP symbols are usually public/exported
            cognitive_complexity: None,
            cyclomatic_complexity: None,
            line_start,
            line_end,
            byte_start: None,
            byte_end: None,
            signature_hash: Some(signature_hash),
            metadata: None,
            confidence: 1.0,
            evidence: Some("scip".to_string()),
            last_indexed_at: Utc::now().to_rfc3339(),
        }
    }
}
