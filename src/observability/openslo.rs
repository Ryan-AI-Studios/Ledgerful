use crate::platform::urn::build_urn;
use crate::state::graph_kinds::NodeKind;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSloMetadata {
    pub name: String,
    pub display_name: Option<String>,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SloSpec {
    pub service: Option<String>,
    pub indicator: Option<OpenSloIndicator>,
    pub sli_ref: Option<String>,
    pub alert_policies: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SliSpec {
    pub service: Option<String>,
    pub indicator: Option<OpenSloIndicator>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataSourceSpec {
    #[serde(rename = "type")]
    pub source_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertPolicySpec {
    pub alert_conditions: Option<Vec<serde_yaml::Value>>,
    pub notification_targets: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertConditionSpec {
    pub alert_policy: Option<String>,
    pub severity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertNotificationTargetSpec {
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSloIndicator {
    pub threshold_metric: Option<OpenSloMetric>,
    pub ratio_metric: Option<OpenSloRatioMetric>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSloMetric {
    pub metric_source: Option<OpenSloMetricSource>,
    pub metric_query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSloRatioMetric {
    pub good: OpenSloMetric,
    pub total: OpenSloMetric,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSloMetricSource {
    pub metric_source_ref: Option<String>,
    #[serde(rename = "type")]
    pub source_type: Option<String>,
}

pub struct ParsedSlo {
    pub kind: String,
    pub urn: String,
    pub name: String,
    pub service_name: Option<String>,
    pub metrics: Vec<ParsedMetric>,
    pub alerts: Vec<String>,
    pub owner: Option<String>,
    pub indicator: Option<OpenSloIndicator>,
    pub metadata: serde_json::Value,
}

pub struct ParsedMetric {
    pub urn: String,
    pub name: String,
    pub query: String,
    pub source: String,
}

fn extract_metrics_from_indicator(
    name: &str,
    indicator: &OpenSloIndicator,
    metrics: &mut Vec<ParsedMetric>,
) {
    if let Some(tm) = &indicator.threshold_metric {
        let src_type = tm
            .metric_source
            .as_ref()
            .and_then(|s| s.source_type.clone())
            .unwrap_or_else(|| "prometheus".to_string());
        metrics.push(ParsedMetric {
            urn: build_urn(NodeKind::Metric, &format!("{}-threshold", name)),
            name: format!("{}-threshold", name),
            query: tm.metric_query.clone(),
            source: src_type,
        });
    }
    if let Some(rm) = &indicator.ratio_metric {
        let good_src = rm
            .good
            .metric_source
            .as_ref()
            .and_then(|s| s.source_type.clone())
            .unwrap_or_else(|| "prometheus".to_string());
        let total_src = rm
            .total
            .metric_source
            .as_ref()
            .and_then(|s| s.source_type.clone())
            .unwrap_or_else(|| "prometheus".to_string());

        metrics.push(ParsedMetric {
            urn: build_urn(NodeKind::Metric, &format!("{}-good", name)),
            name: format!("{}-good", name),
            query: rm.good.metric_query.clone(),
            source: good_src,
        });
        metrics.push(ParsedMetric {
            urn: build_urn(NodeKind::Metric, &format!("{}-total", name)),
            name: format!("{}-total", name),
            query: rm.total.metric_query.clone(),
            source: total_src,
        });
    }
}

pub fn parse_openslo(yaml: &str) -> Result<Vec<ParsedSlo>, String> {
    let mut entities = Vec::new();

    for document in serde_yaml::Deserializer::from_str(yaml) {
        let value = serde_yaml::Value::deserialize(document).map_err(|e| e.to_string())?;

        #[derive(Debug, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawFile {
            api_version: String,
            kind: String,
            metadata: OpenSloMetadata,
            spec: serde_yaml::Value,
        }

        let raw: RawFile = serde_yaml::from_value(value).map_err(|e| e.to_string())?;
        let kind = raw.kind.clone();
        let name = raw.metadata.name.clone();

        match kind.as_str() {
            "Service" => {
                let urn = build_urn(NodeKind::Service, &name);
                entities.push(ParsedSlo {
                    kind,
                    urn,
                    name,
                    service_name: None,
                    metrics: Vec::new(),
                    alerts: Vec::new(),
                    owner: raw.metadata.owner.clone(),
                    indicator: None,
                    metadata: json!({
                        "displayName": raw.metadata.display_name,
                        "apiVersion": raw.api_version,
                        "owner": raw.metadata.owner,
                    }),
                });
            }
            "SLI" => {
                let spec: SliSpec = serde_yaml::from_value(raw.spec).map_err(|e| e.to_string())?;
                let urn = build_urn(NodeKind::Metric, &name);
                let mut metrics = Vec::new();
                if let Some(indicator) = &spec.indicator {
                    extract_metrics_from_indicator(&name, indicator, &mut metrics);
                }
                entities.push(ParsedSlo {
                    kind,
                    urn,
                    name,
                    service_name: spec.service,
                    metrics,
                    alerts: Vec::new(),
                    owner: raw.metadata.owner.clone(),
                    indicator: spec.indicator,
                    metadata: json!({
                        "displayName": raw.metadata.display_name,
                        "apiVersion": raw.api_version,
                        "owner": raw.metadata.owner,
                    }),
                });
            }
            "SLO" => {
                let spec: SloSpec = serde_yaml::from_value(raw.spec).map_err(|e| e.to_string())?;
                let urn = build_urn(NodeKind::Slo, &name);
                let mut metrics = Vec::new();
                if let Some(indicator) = &spec.indicator {
                    extract_metrics_from_indicator(&name, indicator, &mut metrics);
                }
                if let Some(sli_ref) = &spec.sli_ref {
                    metrics.push(ParsedMetric {
                        urn: build_urn(NodeKind::Metric, sli_ref),
                        name: sli_ref.clone(),
                        query: "".to_string(),
                        source: "".to_string(),
                    });
                }
                entities.push(ParsedSlo {
                    kind,
                    urn,
                    name,
                    service_name: spec.service,
                    metrics,
                    alerts: spec.alert_policies.unwrap_or_default(),
                    owner: raw.metadata.owner.clone(),
                    indicator: spec.indicator.clone(),
                    metadata: json!({
                        "displayName": raw.metadata.display_name,
                        "apiVersion": raw.api_version,
                        "owner": raw.metadata.owner,
                        "sliRef": spec.sli_ref,
                    }),
                });
            }
            "DataSource" => {
                let spec: DataSourceSpec =
                    serde_yaml::from_value(raw.spec).map_err(|e| e.to_string())?;
                let urn = build_urn(NodeKind::ObservabilitySignal, &name);
                entities.push(ParsedSlo {
                    kind,
                    urn,
                    name,
                    service_name: None,
                    metrics: Vec::new(),
                    alerts: Vec::new(),
                    owner: raw.metadata.owner.clone(),
                    indicator: None,
                    metadata: json!({
                        "displayName": raw.metadata.display_name,
                        "apiVersion": raw.api_version,
                        "type": spec.source_type,
                    }),
                });
            }
            "AlertPolicy" => {
                let spec: AlertPolicySpec =
                    serde_yaml::from_value(raw.spec).map_err(|e| e.to_string())?;
                let urn = build_urn(NodeKind::Alert, &name);
                let alerts = spec.notification_targets.unwrap_or_default();
                entities.push(ParsedSlo {
                    kind,
                    urn,
                    name,
                    service_name: None,
                    metrics: Vec::new(),
                    alerts,
                    owner: raw.metadata.owner.clone(),
                    indicator: None,
                    metadata: json!({
                        "displayName": raw.metadata.display_name,
                        "apiVersion": raw.api_version,
                    }),
                });
            }
            "AlertCondition" => {
                let spec: AlertConditionSpec =
                    serde_yaml::from_value(raw.spec).map_err(|e| e.to_string())?;
                let urn = build_urn(NodeKind::Alert, &name);
                let alerts = spec.alert_policy.map(|p| vec![p]).unwrap_or_default();
                entities.push(ParsedSlo {
                    kind,
                    urn,
                    name,
                    service_name: None,
                    metrics: Vec::new(),
                    alerts,
                    owner: raw.metadata.owner.clone(),
                    indicator: None,
                    metadata: json!({
                        "displayName": raw.metadata.display_name,
                        "apiVersion": raw.api_version,
                        "severity": spec.severity,
                    }),
                });
            }
            "AlertNotificationTarget" => {
                let spec: AlertNotificationTargetSpec =
                    serde_yaml::from_value(raw.spec).map_err(|e| e.to_string())?;
                let urn = build_urn(NodeKind::Role, &name);
                entities.push(ParsedSlo {
                    kind,
                    urn,
                    name,
                    service_name: None,
                    metrics: Vec::new(),
                    alerts: Vec::new(),
                    owner: raw.metadata.owner.clone(),
                    indicator: None,
                    metadata: json!({
                        "displayName": raw.metadata.display_name,
                        "apiVersion": raw.api_version,
                        "target": spec.target,
                    }),
                });
            }
            _ => {
                tracing::debug!("Skipping unsupported OpenSLO kind: {kind}");
            }
        }
    }

    Ok(entities)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_slo() {
        let yaml = r#"
apiVersion: openslo/v1
kind: SLO
metadata:
  name: user-service-availability
  displayName: User Service Availability
spec:
  service: user-service
  indicator:
    thresholdMetric:
      metricSource:
        metricSourceRef: prometheus
        type: prometheus
      metricQuery: sum(rate(http_requests_total{status=~"2.."}[5m]))
  objectives:
    - target: 0.999
  alertPolicies:
    - high-error-rate
"#;
        let slos = parse_openslo(yaml).unwrap();
        assert_eq!(slos.len(), 1);
        let slo = &slos[0];
        assert_eq!(slo.name, "user-service-availability");
        assert_eq!(slo.service_name, Some("user-service".to_string()));
        assert_eq!(slo.metrics.len(), 1);
        assert_eq!(slo.metrics[0].source, "prometheus");
        assert_eq!(slo.alerts.len(), 1);
        assert_eq!(slo.alerts[0], "high-error-rate");
    }

    #[test]
    fn test_parse_ratio_slo() {
        let yaml = r#"
apiVersion: openslo/v1
kind: SLO
metadata:
  name: user-service-reliability
spec:
  service: user-service
  indicator:
    ratioMetric:
      good:
        metricSource:
          metricSourceRef: prom
          type: prometheus
        metricQuery: http_requests_total{status=~"2.."}
      total:
        metricSource:
          metricSourceRef: prom
          type: prometheus
        metricQuery: http_requests_total{}
  objectives:
    - target: 0.99
"#;
        let slos = parse_openslo(yaml).unwrap();
        assert_eq!(slos.len(), 1);
        let slo = &slos[0];
        assert_eq!(slo.metrics.len(), 2);
    }

    /// DX1 round-trip proof: the template emitted by
    /// `crate::commands::dx1_templates::generate_openslo_template()` MUST
    /// deserialize through this repo's own `parse_openslo` without error and
    /// yield at least one parsed metric. This is the contract that makes
    /// `ledgerful observability coverage`'s bootstrap offer actually
    /// populate the surface after `ledgerful index --analyze-graph`.
    #[test]
    fn openslo_template_round_trips_through_parser() {
        let yaml = crate::commands::dx1_templates::generate_openslo_template();
        let slos = parse_openslo(&yaml).expect(
            "generated OpenSLO template must deserialize through parse_openslo without error",
        );
        // Exactly one SLO document in the template.
        assert_eq!(slos.len(), 1, "template must parse to exactly one SLO");

        let slo = &slos[0];
        assert_eq!(slo.kind, "SLO", "parsed entity must be an SLO");
        assert_eq!(
            slo.name, "ledgerful-base-slo",
            "parsed SLO name must match the template"
        );
        assert_eq!(
            slo.service_name.as_deref(),
            Some("ledgerful"),
            "parsed service must match the template"
        );

        // ratioMetric.good + ratioMetric.total → exactly two metrics, both
        // with non-empty queries and a prometheus source.
        assert!(
            !slo.metrics.is_empty(),
            "ParsedSlo.metrics must be non-empty (template round-trips into the graph)"
        );
        assert_eq!(
            slo.metrics.len(),
            2,
            "ratioMetric must yield exactly two metrics (good + total): {}",
            slo.metrics.len()
        );

        let good = slo
            .metrics
            .iter()
            .find(|m| m.name.ends_with("-good"))
            .expect("good metric must be present");
        assert!(
            !good.query.is_empty(),
            "good.metricQuery must deserialize as a non-empty string: {}",
            good.query
        );
        assert_eq!(
            good.source, "prometheus",
            "good.metricSource.type must deserialize as prometheus: {}",
            good.source
        );

        let total = slo
            .metrics
            .iter()
            .find(|m| m.name.ends_with("-total"))
            .expect("total metric must be present");
        assert!(
            !total.query.is_empty(),
            "total.metricQuery must deserialize as a non-empty string: {}",
            total.query
        );
        assert_eq!(
            total.source, "prometheus",
            "total.metricSource.type must deserialize as prometheus: {}",
            total.source
        );

        // alertPolicies is a list of STRING refs.
        assert_eq!(
            slo.alerts,
            vec!["ledgerful-base-burn-rate".to_string()],
            "alertPolicies must parse as a list of string refs: {:?}",
            slo.alerts
        );
    }
}
