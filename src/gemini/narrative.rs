use crate::impact::packet::ImpactPacket;

pub struct NarrativeEngine;

impl NarrativeEngine {
    pub fn generate_risk_prompt(packet: &ImpactPacket) -> String {
        let mut prompt = String::new();
        prompt.push_str("Act as a Senior Software Architect. Provide a high-level narrative summary of the following change impact report.\n\n");

        prompt.push_str("## Core Analysis\n");
        prompt.push_str(&format!("- Overall Risk Level: {:?}\n", packet.risk_level));
        prompt.push_str("- Risk Reasons:\n");
        for reason in &packet.risk_reasons {
            prompt.push_str(&format!("  * {}\n", reason));
        }

        prompt.push_str("\n## Changes Summary\n");
        prompt.push_str(&format!(
            "- Total files changed: {}\n",
            packet.changes.len()
        ));
        for file in packet.changes.iter().take(5) {
            prompt.push_str(&format!("  * {} ({})\n", file.path.display(), file.status));
        }
        if packet.changes.len() > 5 {
            prompt.push_str(&format!(
                "  * ... and {} more files\n",
                packet.changes.len() - 5
            ));
        }

        if !packet.hotspots.is_empty() {
            prompt.push_str("\n## Code Hotspots (High Risk Density)\n");
            for hotspot in packet.hotspots.iter().take(3) {
                prompt.push_str(&format!(
                    "  * {}: Score {:.2} (Freq: {}, Complexity: {})\n",
                    hotspot.path.display(),
                    hotspot.score,
                    hotspot.frequency,
                    hotspot.complexity
                ));
            }
        }

        if !packet.temporal_couplings.is_empty() {
            prompt.push_str("\n## Temporal Couplings (Logical Dependencies)\n");
            for coupling in packet.temporal_couplings.iter().take(3) {
                prompt.push_str(&format!(
                    "  * {} <-> {} (Affinity: {:.0}%)\n",
                    coupling.file_a.display(),
                    coupling.file_b.display(),
                    coupling.score * 100.0
                ));
            }
        }

        prompt.push_str("\n## Task\n");
        prompt.push_str("Explain the 'Butterfly Effect' of these changes. What is the most likely thing to break that is NOT in the changed files? What should the reviewer focus on most?");

        prompt
    }
}
