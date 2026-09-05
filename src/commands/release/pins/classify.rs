use super::parse::{normalize_digest, strip_leading_v};
use super::types::{
    ARCHIVE_WINDOWS, Advisory, AdvisoryInput, ClassifyPinsInput, GITHUB_OWNER_REPO,
    HOMEBREW_ARCHIVES, HomebrewPin, ID_MCP_INTREE, ID_MCP_NPM, ID_PACKAGING_HOMEBREW,
    ID_PACKAGING_SCOOP, ID_REMOTE_BUCKET, ID_REMOTE_TAP, KIND, LatestJson, LatestPins, McpPin,
    PinStatus, ReleasePinsEnvelope, RemoteFact, SCHEMA_VERSION, ScoopPin, Surface,
};
use crate::commands::doctor::shorten_sha_for_display;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn version_eq(a: &str, b: &str) -> bool {
    strip_leading_v(a.trim()) == strip_leading_v(b.trim())
}

fn hash_eq(a: &str, b: &str) -> bool {
    let a = normalize_digest(a);
    let b = normalize_digest(b);
    !a.is_empty() && a == b
}

fn homebrew_json(pin: &HomebrewPin) -> Value {
    let hashes: BTreeMap<&str, &str> = pin
        .hashes
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    json!({
        "version": pin.version,
        "hashes": hashes,
    })
}

fn expected_homebrew_json(latest: &LatestPins) -> Value {
    let mut hashes = BTreeMap::new();
    for name in HOMEBREW_ARCHIVES {
        if let Some(h) = latest.archives.get(*name) {
            hashes.insert(*name, h.as_str());
        }
    }
    json!({
        "version": latest.version,
        "hashes": hashes,
    })
}

fn scoop_json(pin: &ScoopPin) -> Value {
    json!({
        "version": pin.version,
        "url": pin.url,
        "hash": pin.hash,
    })
}

pub(super) fn expected_scoop_url(tag: &str) -> String {
    format!("https://github.com/{GITHUB_OWNER_REPO}/releases/download/{tag}/{ARCHIVE_WINDOWS}")
}

fn expected_scoop_json(latest: &LatestPins) -> Value {
    json!({
        "version": latest.version,
        "url": expected_scoop_url(&latest.tag),
        "hash": latest.archives.get(ARCHIVE_WINDOWS),
    })
}

fn mcp_json(pin: &McpPin) -> Value {
    json!({
        "version": pin.version,
        "ledgerfulEngineTag": pin.ledgerful_engine_tag,
    })
}

fn homebrew_expected_complete(latest: &LatestPins) -> bool {
    HOMEBREW_ARCHIVES
        .iter()
        .all(|name| latest.archives.contains_key(*name))
}

fn homebrew_matches(local: &HomebrewPin, latest: &LatestPins) -> bool {
    let Some(ver) = local.version.as_deref() else {
        return false;
    };
    if !version_eq(ver, &latest.version) {
        return false;
    }
    HOMEBREW_ARCHIVES.iter().all(|name| {
        match (
            local.hashes.get(*name).map(String::as_str),
            latest.archives.get(*name).map(String::as_str),
        ) {
            (Some(a), Some(b)) => hash_eq(a, b),
            _ => false,
        }
    })
}

fn scoop_matches(local: &ScoopPin, latest: &LatestPins) -> bool {
    let Some(ver) = local.version.as_deref() else {
        return false;
    };
    if !version_eq(ver, &latest.version) {
        return false;
    }
    let Some(expected_hash) = latest.archives.get(ARCHIVE_WINDOWS) else {
        return false;
    };
    let Some(local_hash) = local.hash.as_deref() else {
        return false;
    };
    if !hash_eq(local_hash, expected_hash) {
        return false;
    }
    match local.url.as_deref() {
        Some(url) => url == expected_scoop_url(&latest.tag),
        None => false,
    }
}

fn mcp_tag_matches(pin: &McpPin, latest: &LatestPins) -> bool {
    pin.ledgerful_engine_tag
        .as_deref()
        .is_some_and(|t| t == latest.tag)
}

fn surface(id: &str, status: PinStatus, local: Value, expected: Value, remote: Value) -> Surface {
    Surface {
        id: id.to_string(),
        status,
        local,
        expected,
        remote,
    }
}

fn classify_packaging_homebrew(
    latest: Option<&LatestPins>,
    local: Option<&HomebrewPin>,
) -> Surface {
    let local_json = local.map(homebrew_json).unwrap_or(Value::Null);
    let Some(latest) = latest else {
        return surface(
            ID_PACKAGING_HOMEBREW,
            PinStatus::Unverified,
            local_json,
            json!({}),
            Value::Null,
        );
    };
    let expected = expected_homebrew_json(latest);
    let Some(local) = local else {
        return surface(
            ID_PACKAGING_HOMEBREW,
            PinStatus::Drift,
            Value::Null,
            expected,
            Value::Null,
        );
    };
    if !homebrew_expected_complete(latest) {
        return surface(
            ID_PACKAGING_HOMEBREW,
            PinStatus::Unverified,
            homebrew_json(local),
            expected,
            Value::Null,
        );
    }
    let status = if homebrew_matches(local, latest) {
        PinStatus::Match
    } else {
        PinStatus::Drift
    };
    surface(
        ID_PACKAGING_HOMEBREW,
        status,
        homebrew_json(local),
        expected,
        Value::Null,
    )
}

fn classify_packaging_scoop(latest: Option<&LatestPins>, local: Option<&ScoopPin>) -> Surface {
    let local_json = local.map(scoop_json).unwrap_or(Value::Null);
    let Some(latest) = latest else {
        return surface(
            ID_PACKAGING_SCOOP,
            PinStatus::Unverified,
            local_json,
            json!({}),
            Value::Null,
        );
    };
    let expected = expected_scoop_json(latest);
    let Some(local) = local else {
        return surface(
            ID_PACKAGING_SCOOP,
            PinStatus::Drift,
            Value::Null,
            expected,
            Value::Null,
        );
    };
    if !latest.archives.contains_key(ARCHIVE_WINDOWS) {
        return surface(
            ID_PACKAGING_SCOOP,
            PinStatus::Unverified,
            scoop_json(local),
            expected,
            Value::Null,
        );
    }
    let status = if scoop_matches(local, latest) {
        PinStatus::Match
    } else {
        PinStatus::Drift
    };
    surface(
        ID_PACKAGING_SCOOP,
        status,
        scoop_json(local),
        expected,
        Value::Null,
    )
}

fn classify_mcp_intree(latest: Option<&LatestPins>, local: Option<&McpPin>) -> Surface {
    let local_json = local.map(mcp_json).unwrap_or(Value::Null);
    let Some(latest) = latest else {
        return surface(
            ID_MCP_INTREE,
            PinStatus::Unverified,
            local_json,
            json!({}),
            Value::Null,
        );
    };
    let expected = json!({ "ledgerfulEngineTag": latest.tag });
    let Some(local) = local else {
        return surface(
            ID_MCP_INTREE,
            PinStatus::Drift,
            Value::Null,
            expected,
            Value::Null,
        );
    };
    let status = if mcp_tag_matches(local, latest) {
        PinStatus::Match
    } else {
        PinStatus::Drift
    };
    surface(
        ID_MCP_INTREE,
        status,
        mcp_json(local),
        expected,
        Value::Null,
    )
}

fn classify_remote_homebrew(
    latest: Option<&LatestPins>,
    remote: &RemoteFact<HomebrewPin>,
) -> Surface {
    let Some(latest) = latest else {
        return surface(
            ID_REMOTE_TAP,
            PinStatus::Unverified,
            Value::Null,
            json!({}),
            Value::Null,
        );
    };
    let expected = expected_homebrew_json(latest);
    match remote {
        RemoteFact::Unverified => surface(
            ID_REMOTE_TAP,
            PinStatus::Unverified,
            Value::Null,
            expected,
            Value::Null,
        ),
        RemoteFact::Value(pin) => {
            if !homebrew_expected_complete(latest) {
                return surface(
                    ID_REMOTE_TAP,
                    PinStatus::Unverified,
                    Value::Null,
                    expected,
                    homebrew_json(pin),
                );
            }
            let status = if homebrew_matches(pin, latest) {
                PinStatus::Match
            } else {
                PinStatus::Drift
            };
            surface(
                ID_REMOTE_TAP,
                status,
                Value::Null,
                expected,
                homebrew_json(pin),
            )
        }
    }
}

fn classify_remote_scoop(latest: Option<&LatestPins>, remote: &RemoteFact<ScoopPin>) -> Surface {
    let Some(latest) = latest else {
        return surface(
            ID_REMOTE_BUCKET,
            PinStatus::Unverified,
            Value::Null,
            json!({}),
            Value::Null,
        );
    };
    let expected = expected_scoop_json(latest);
    match remote {
        RemoteFact::Unverified => surface(
            ID_REMOTE_BUCKET,
            PinStatus::Unverified,
            Value::Null,
            expected,
            Value::Null,
        ),
        RemoteFact::Value(pin) => {
            if !latest.archives.contains_key(ARCHIVE_WINDOWS) {
                return surface(
                    ID_REMOTE_BUCKET,
                    PinStatus::Unverified,
                    Value::Null,
                    expected,
                    scoop_json(pin),
                );
            }
            let status = if scoop_matches(pin, latest) {
                PinStatus::Match
            } else {
                PinStatus::Drift
            };
            surface(
                ID_REMOTE_BUCKET,
                status,
                Value::Null,
                expected,
                scoop_json(pin),
            )
        }
    }
}

fn classify_mcp_npm(
    latest: Option<&LatestPins>,
    remote: &RemoteFact<McpPin>,
    intree: Option<&McpPin>,
) -> Surface {
    let Some(latest) = latest else {
        return surface(
            ID_MCP_NPM,
            PinStatus::Unverified,
            Value::Null,
            json!({}),
            Value::Null,
        );
    };
    let expected = json!({
        "ledgerfulEngineTag": latest.tag,
        "version": intree.and_then(|m| m.version.as_deref()),
    });
    match remote {
        RemoteFact::Unverified => surface(
            ID_MCP_NPM,
            PinStatus::Unverified,
            Value::Null,
            expected,
            Value::Null,
        ),
        RemoteFact::Value(pin) => {
            let tag_ok = mcp_tag_matches(pin, latest);
            let version_ok = match intree.and_then(|m| m.version.as_deref()) {
                Some(expected_ver) => pin.version.as_deref() == Some(expected_ver),
                None => true,
            };
            let status = if tag_ok && version_ok {
                PinStatus::Match
            } else {
                PinStatus::Drift
            };
            surface(ID_MCP_NPM, status, Value::Null, expected, mcp_json(pin))
        }
    }
}

fn classify_advisory(latest: Option<&LatestPins>, adv: Option<AdvisoryInput>) -> Option<Advisory> {
    let adv = adv?;
    let status = match latest {
        None => PinStatus::Unverified,
        Some(latest) => {
            let tag_ok = adv.release_tag.as_deref() == Some(latest.tag.as_str());
            let mcp_ok = adv.mcp_engine_tag.as_deref() == Some(latest.tag.as_str());
            if tag_ok && mcp_ok {
                PinStatus::Match
            } else {
                PinStatus::Drift
            }
        }
    };
    Some(Advisory {
        launch_facts_path: adv.launch_facts_path,
        release_tag: adv.release_tag,
        mcp_engine_tag: adv.mcp_engine_tag,
        status,
    })
}

fn overall_from_surfaces(surfaces: &[Surface]) -> PinStatus {
    if surfaces.iter().any(|s| s.status == PinStatus::Drift) {
        PinStatus::Drift
    } else if surfaces.iter().any(|s| s.status == PinStatus::Unverified) {
        PinStatus::Unverified
    } else {
        PinStatus::Match
    }
}

fn latest_json(latest: &LatestPins) -> LatestJson {
    LatestJson {
        tag: latest.tag.clone(),
        sha: latest
            .sha
            .as_deref()
            .map(shorten_sha_for_display)
            .filter(|s| !s.is_empty()),
    }
}

/// Pure classifier. Fetch is [`super::fetch::fetch_latest_pins`].
pub(crate) fn classify_pins(input: ClassifyPinsInput<'_>) -> ReleasePinsEnvelope {
    if !input.is_engine {
        return ReleasePinsEnvelope {
            schema_version: SCHEMA_VERSION,
            kind: KIND,
            status: PinStatus::Skipped,
            latest: None,
            surfaces: Vec::new(),
            advisory: None,
        };
    }

    let latest = input.latest.filter(|_| !input.fetch_error);
    let mut surfaces = vec![
        classify_packaging_homebrew(latest, input.locals.homebrew.as_ref()),
        classify_packaging_scoop(latest, input.locals.scoop.as_ref()),
        classify_mcp_intree(latest, input.locals.mcp.as_ref()),
        classify_remote_homebrew(latest, &input.remotes.homebrew_tap),
        classify_remote_scoop(latest, &input.remotes.scoop_bucket),
        classify_mcp_npm(latest, &input.remotes.npm, input.locals.mcp.as_ref()),
    ];
    surfaces.sort_by(|a, b| a.id.cmp(&b.id));

    let status = if latest.is_none() {
        PinStatus::Unverified
    } else {
        overall_from_surfaces(&surfaces)
    };
    let advisory = classify_advisory(latest, input.advisory);

    ReleasePinsEnvelope {
        schema_version: SCHEMA_VERSION,
        kind: KIND,
        status,
        latest: latest.map(latest_json),
        surfaces,
        advisory,
    }
}
