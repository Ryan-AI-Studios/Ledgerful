use crate::gemini::modes::{GeminiMode, build_user_prompt};
use crate::impact::packet::ImpactPacket;

pub fn build_architect_prompt(packet: &ImpactPacket, query: &str) -> String {
    build_user_prompt(GeminiMode::Narrative, packet, query, None)
}

pub fn build_suggest_prompt(packet: &ImpactPacket, query: &str) -> String {
    build_user_prompt(GeminiMode::Suggest, packet, query, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::packet::ImpactPacket;

    #[test]
    fn test_prompt_construction() {
        let packet = ImpactPacket::default();
        let query = "What is the risk?";
        let prompt = build_user_prompt(GeminiMode::Analyze, &packet, query, None);

        assert!(prompt.contains("Impact Packet:"));
        assert!(prompt.contains(query));
        assert!(prompt.contains("v1") || prompt.contains("1.0")); // schema version
    }
}
