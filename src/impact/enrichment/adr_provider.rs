use crate::impact::enrichment::{EnrichmentContext, EnrichmentProvider};
use crate::impact::packet::ImpactPacket;
use crate::platform::urn::build_urn;
use crate::state::graph_kinds::{EdgeKind, NodeKind};
use miette::Result;

pub struct AdrProvider;

impl EnrichmentProvider for AdrProvider {
    fn name(&self) -> &'static str {
        "ADR"
    }

    fn enrich(&self, context: &EnrichmentContext, packet: &mut ImpactPacket) -> Result<()> {
        let Some(cozo) = &context.storage.cozo else {
            return Ok(());
        };

        for changed_file in &packet.changes {
            let file_path = changed_file.path.to_string_lossy();
            let file_urn = build_urn(NodeKind::File, &file_path);

            // Find ADRs that govern this file
            let query = format!(
                "?[adr_id, summary, status] := *edge{{source: adr_urn, target: '{}', relation: '{}'}}, \
                 *node{{id: adr_urn, label: label, category: 'adr', metadata: meta}}, \
                 adr_id = concat('', substr(adr_urn, 17)), \
                 summary = substr(label, 5), \
                 status = get(meta, 'status')",
                file_urn,
                EdgeKind::Governs
            );

            if let Ok(res) = cozo.run_script(&query) {
                for row in res.rows {
                    if let (
                        Some(cozo::DataValue::Str(adr_id)),
                        Some(cozo::DataValue::Str(summary)),
                        Some(cozo::DataValue::Str(status)),
                    ) = (row.first(), row.get(1), row.get(2))
                    {
                        packet.risk_reasons.push(format!(
                            "Change touches entity governed by ADR {}: {} (Status: {})",
                            adr_id, summary, status
                        ));

                        if status == "deprecated" || status == "superseded" {
                            packet.risk_reasons.push(format!(
                                "WARNING: ADR {} is {}, review if this change aligns with the new architecture.",
                                adr_id, status
                            ));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
