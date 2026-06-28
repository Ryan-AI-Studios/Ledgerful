use serde::{Deserialize, Serialize};
use strum::{Display, EnumString};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum NodeKind {
    Symbol,
    Endpoint,
    Service,
    DataModel,
    Migration,
    ConfigKey,
    DeploySurface,
    CiWorkflow,
    CiJob,
    CiEnvironment,
    CiArtifact,
    Dependency,
    Package,
    Advisory,
    Test,
    ObservabilitySignal,
    Metric,
    Slo,
    Alert,
    Adr,
    LedgerTransaction,
    Hotspot,
    TemporalCoupling,
    SecurityBoundary,
    Table,
    Column,
    File,
    Role,
    Scope,
    Policy,
    Principal,
    Action,
    Resource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum EdgeKind {
    Owns,
    Handles,
    Calls,
    Covers,
    Governs,
    Supersedes,
    Deploys,
    DependsOn,
    Emits,
    Monitors,
    AlertsOn,
    ChangedWith,
    Validates,
    Verifies,
    Authenticates,
    Authorizes,
    Consumes,
    TouchesSecret,
    Migrates,
    MapsTo,
    Affects,
    Configures,
    ProtectedBy,
}
