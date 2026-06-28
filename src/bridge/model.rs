use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRecord {
    pub bridge_version: String,
    pub direction: BridgeDirection,
    pub timestamp: DateTime<Utc>,
    pub parent_hash: Option<String>,
    pub project_id: String,
    pub session_id: Option<String>,
    pub tx_id: Option<String>,
    pub record_kind: String,
    pub payload: BridgePayload,
    pub privacy: Privacy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BridgeDirection {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Privacy {
    #[default]
    #[serde(alias = "CloudOk", alias = "cloudok", alias = "cloud_ok")]
    Public,
    #[serde(alias = "LocalOnly", alias = "localonly", alias = "local_only")]
    ProjectLocal,
    #[serde(alias = "NeverInject", alias = "neverinject", alias = "never_inject")]
    Private,
    #[serde(alias = "Sealed", alias = "sealed")]
    Sealed,
}

impl Privacy {
    pub fn combine(&self, other: Privacy) -> Privacy {
        std::cmp::max(*self, other)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BridgePayload {
    Hotspot {
        path: String,
        score: f64,
        reason: String,
        #[serde(default)]
        temporal_coupling: f64,
        #[serde(default)]
        failure_risk_probability: f64,
    },
    LedgerDelta {
        tx_id: String,
        intent: String,
        files_changed: usize,
    },
    Insight {
        memory_id: String,
        relevance: f64,
        content: String,
    },
    VerifyOutcome(BridgeVerifyOutcome),
    Query {
        text: String,
    },
    Madr {
        title: String,
        context: String,
        decision: String,
        consequences: String,
    },
    RiskAlert {
        coupled_file_a: String,
        coupled_file_b: String,
        coupling_score: f64,
        affected_symbols: Vec<String>,
        suggested_remediation: String,
        risk_level: String,
    },
    Snapshot(Box<SnapshotPayload>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotPayload {
    pub project_id: String,
    pub impact: crate::impact::packet::ImpactPacket,
    pub hotspots: Vec<crate::impact::packet::Hotspot>,
    pub ledger: Vec<crate::ledger::types::LedgerEntry>,
    pub metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeVerifyOutcome {
    pub success: bool,
    pub command: String,
    pub error_snippet: Option<String>,
}

impl BridgeRecord {
    pub const VERSION: &'static str = "0.3";

    pub fn new(
        direction: BridgeDirection,
        project_id: String,
        record_kind: &str,
        payload: BridgePayload,
    ) -> Self {
        Self {
            bridge_version: Self::VERSION.to_string(),
            direction,
            timestamp: Utc::now(),
            parent_hash: None, // Will be set by sync engine if available
            project_id,
            session_id: None,
            tx_id: None,
            record_kind: record_kind.to_string(),
            payload,
            privacy: Privacy::ProjectLocal,
        }
    }
}

pub fn serialize_record(record: &BridgeRecord) -> Result<String, serde_json::Error> {
    serde_json::to_string(record)
}

pub fn calculate_hash(record: &BridgeRecord) -> String {
    use sha2::{Digest, Sha256};
    let json = serde_json::to_string(record).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

pub fn deserialize_record(json: &str) -> Result<BridgeRecord, serde_json::Error> {
    let record: BridgeRecord = serde_json::from_str(json)?;
    if record.bridge_version != BridgeRecord::VERSION {
        return Err(serde::de::Error::custom(format!(
            "Bridge record version mismatch: expected {}, found {}",
            BridgeRecord::VERSION,
            record.bridge_version
        )));
    }
    Ok(record)
}
