use super::classify::{classify_pins, expected_scoop_url};
use super::emit::exit_code_for;
use super::fetch::{fetch_latest_pins, remotes_from_fetch, user_agent};
use super::parse::{
    parse_homebrew_formula, parse_mcp_package, parse_release_pins, parse_scoop_manifest,
};
use super::types::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod env_guard {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/integration/common/env_guard.rs"
    ));
}
use env_guard::TempEnv;

const HASH_DARWIN_ARM: &str = "550cbc61bde812017a5fc19d61e00dac7cd59ac14fed0a81bf7dda5ce22d29de";
const HASH_DARWIN_X64: &str = "149f14faf2f153c1682505e32ca49cca6a35f2375547cb3cef4de8fa5810a614";
const HASH_LINUX_X64: &str = "817debe3fa56db93aeb1273b1648a2b6370a50f3c69150caf8cdc423d9c1930d";
const HASH_WINDOWS: &str = "99f6e6bb23f93cd46cdb47a1a196b68d154e7d86e3ab226d3b1f0d4ddedf48ca";
const HASH_SIDECAR: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LATEST_TAG: &str = "v0.2.10";
const LATEST_SHA: &str = "c4a2308fe98548899105e33ff38232dfb229ec02";
const MCP_VERSION: &str = "0.1.19";

fn scoop_url_for(tag: &str) -> String {
    expected_scoop_url(tag)
}

fn published_latest() -> LatestPins {
    let mut archives = BTreeMap::new();
    archives.insert(ARCHIVE_DARWIN_ARM.to_string(), HASH_DARWIN_ARM.to_string());
    archives.insert(ARCHIVE_DARWIN_X64.to_string(), HASH_DARWIN_X64.to_string());
    archives.insert(ARCHIVE_LINUX_X64.to_string(), HASH_LINUX_X64.to_string());
    archives.insert(ARCHIVE_WINDOWS.to_string(), HASH_WINDOWS.to_string());
    LatestPins {
        tag: LATEST_TAG.to_string(),
        sha: Some(LATEST_SHA.to_string()),
        version: "0.2.10".to_string(),
        archives,
    }
}

fn matching_homebrew() -> HomebrewPin {
    let mut hashes = BTreeMap::new();
    hashes.insert(ARCHIVE_DARWIN_ARM.to_string(), HASH_DARWIN_ARM.to_string());
    hashes.insert(ARCHIVE_DARWIN_X64.to_string(), HASH_DARWIN_X64.to_string());
    hashes.insert(ARCHIVE_LINUX_X64.to_string(), HASH_LINUX_X64.to_string());
    HomebrewPin {
        version: Some("0.2.10".to_string()),
        hashes,
    }
}

fn matching_scoop() -> ScoopPin {
    ScoopPin {
        version: Some("0.2.10".to_string()),
        url: Some(scoop_url_for(LATEST_TAG)),
        hash: Some(HASH_WINDOWS.to_string()),
    }
}

fn matching_mcp() -> McpPin {
    McpPin {
        version: Some(MCP_VERSION.to_string()),
        ledgerful_engine_tag: Some(LATEST_TAG.to_string()),
    }
}

fn matching_locals() -> LocalPins {
    LocalPins {
        homebrew: Some(matching_homebrew()),
        scoop: Some(matching_scoop()),
        mcp: Some(matching_mcp()),
    }
}

fn matching_remotes() -> RemotePins {
    RemotePins {
        homebrew_tap: RemoteFact::Value(matching_homebrew()),
        scoop_bucket: RemoteFact::Value(matching_scoop()),
        npm: RemoteFact::Value(matching_mcp()),
    }
}

fn classify_engine(
    latest: Option<&LatestPins>,
    locals: &LocalPins,
    remotes: &RemotePins,
    fetch_error: bool,
    advisory: Option<AdvisoryInput>,
) -> ReleasePinsEnvelope {
    classify_pins(ClassifyPinsInput {
        is_engine: true,
        latest,
        fetch_error,
        locals,
        remotes,
        advisory,
    })
}

fn surface_status(env: &ReleasePinsEnvelope, id: &str) -> PinStatus {
    env.surfaces
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.status)
        .unwrap_or_else(|| panic!("missing surface {id}"))
}

fn ids(env: &ReleasePinsEnvelope) -> Vec<&str> {
    env.surfaces.iter().map(|s| s.id.as_str()).collect()
}

#[test]
fn classify_t0_consumer_skipped() {
    let latest = published_latest();
    let env = classify_pins(ClassifyPinsInput {
        is_engine: false,
        latest: Some(&latest),
        fetch_error: false,
        locals: &matching_locals(),
        remotes: &matching_remotes(),
        advisory: None,
    });
    assert_eq!(env.status, PinStatus::Skipped);
    assert_eq!(exit_code_for(env.status), 2);
    assert!(env.surfaces.is_empty());
    let v = serde_json::to_value(&env).expect("json");
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["kind"], "releasePins");
    assert_eq!(v["status"], "skipped");
    assert!(v.get("latest").is_none());
    assert_eq!(v["surfaces"].as_array().map(Vec::len), Some(0));
    assert!(v.get("advisory").is_none());
}

#[test]
fn classify_t1_no_network_unverified() {
    let env = classify_engine(
        None,
        &matching_locals(),
        &RemotePins::unverified(),
        true,
        None,
    );
    assert_eq!(env.status, PinStatus::Unverified);
    assert_eq!(exit_code_for(env.status), 2);
    let v = serde_json::to_value(&env).expect("json");
    assert_eq!(v["status"], "unverified");
    assert!(v.get("latest").is_none());
    assert_eq!(ids(&env).len(), 6);
    for s in &env.surfaces {
        assert_eq!(s.status, PinStatus::Unverified);
    }
}

#[test]
fn classify_t2_invalid_latest_unverified() {
    let env = classify_engine(
        None,
        &matching_locals(),
        &RemotePins::unverified(),
        true,
        None,
    );
    assert_eq!(env.status, PinStatus::Unverified);
    assert_eq!(exit_code_for(env.status), 2);
    assert_ne!(env.status, PinStatus::Match);
    let v = serde_json::to_value(&env).expect("json");
    assert!(v.get("latest").is_none());
}

#[test]
fn classify_t3_all_match() {
    let latest = published_latest();
    let env = classify_engine(
        Some(&latest),
        &matching_locals(),
        &matching_remotes(),
        false,
        None,
    );
    assert_eq!(env.status, PinStatus::Match);
    assert_eq!(exit_code_for(env.status), 0);
    assert_eq!(ids(&env).len(), 6);
    for s in &env.surfaces {
        assert_eq!(s.status, PinStatus::Match, "{}", s.id);
    }
    let v = serde_json::to_value(&env).expect("json");
    assert_eq!(v["latest"]["tag"], LATEST_TAG);
    assert_eq!(v["latest"]["sha"], "c4a2308fe985");
}

#[test]
fn classify_t4_homebrew_template_lag_is_drift() {
    let latest = published_latest();
    let mut locals = matching_locals();
    if let Some(hb) = locals.homebrew.as_mut() {
        hb.version = Some("0.2.9".to_string());
    }
    let env = classify_engine(Some(&latest), &locals, &matching_remotes(), false, None);
    assert_eq!(env.status, PinStatus::Drift);
    assert_eq!(exit_code_for(env.status), 1);
    assert_eq!(
        surface_status(&env, ID_PACKAGING_HOMEBREW),
        PinStatus::Drift
    );
}

#[test]
fn classify_t5_scoop_hash_mismatch_is_drift() {
    let latest = published_latest();
    let mut locals = matching_locals();
    if let Some(sc) = locals.scoop.as_mut() {
        sc.hash = Some(HASH_SIDECAR.to_string());
    }
    let env = classify_engine(Some(&latest), &locals, &matching_remotes(), false, None);
    assert_eq!(env.status, PinStatus::Drift);
    assert_eq!(exit_code_for(env.status), 1);
    assert_eq!(surface_status(&env, ID_PACKAGING_SCOOP), PinStatus::Drift);
}

#[test]
fn classify_t6_mcp_intree_tag_lag_is_drift() {
    let latest = published_latest();
    let mut locals = matching_locals();
    if let Some(mcp) = locals.mcp.as_mut() {
        mcp.ledgerful_engine_tag = Some("v0.2.9".to_string());
    }
    let env = classify_engine(Some(&latest), &locals, &matching_remotes(), false, None);
    assert_eq!(env.status, PinStatus::Drift);
    assert_eq!(surface_status(&env, ID_MCP_INTREE), PinStatus::Drift);
}

#[test]
fn classify_t7_tap_remote_lag_is_drift() {
    let latest = published_latest();
    let mut remotes = matching_remotes();
    let mut tap = matching_homebrew();
    tap.version = Some("0.2.9".to_string());
    remotes.homebrew_tap = RemoteFact::Value(tap);
    let env = classify_engine(Some(&latest), &matching_locals(), &remotes, false, None);
    assert_eq!(env.status, PinStatus::Drift);
    assert_eq!(surface_status(&env, ID_REMOTE_TAP), PinStatus::Drift);
    assert_eq!(
        surface_status(&env, ID_PACKAGING_HOMEBREW),
        PinStatus::Match
    );
}

#[test]
fn classify_t8_bucket_remote_lag_is_drift() {
    let latest = published_latest();
    let mut remotes = matching_remotes();
    let mut bucket = matching_scoop();
    bucket.version = Some("0.2.9".to_string());
    remotes.scoop_bucket = RemoteFact::Value(bucket);
    let env = classify_engine(Some(&latest), &matching_locals(), &remotes, false, None);
    assert_eq!(env.status, PinStatus::Drift);
    assert_eq!(surface_status(&env, ID_REMOTE_BUCKET), PinStatus::Drift);
}

#[test]
fn classify_t9_npm_engine_tag_lag_is_drift() {
    let latest = published_latest();
    let mut remotes = matching_remotes();
    remotes.npm = RemoteFact::Value(McpPin {
        version: Some(MCP_VERSION.to_string()),
        ledgerful_engine_tag: Some("v0.2.9".to_string()),
    });
    let env = classify_engine(Some(&latest), &matching_locals(), &remotes, false, None);
    assert_eq!(env.status, PinStatus::Drift);
    assert_eq!(exit_code_for(env.status), 1);
    assert_eq!(surface_status(&env, ID_MCP_NPM), PinStatus::Drift);
}

fn release_json_with_sidecar() -> Value {
    serde_json::from_str(&format!(
        r#"{{
            "tag_name": "v0.2.10",
            "target_commitish": "main",
            "assets": [
                {{
                    "name": "{ARCHIVE_DARWIN_ARM}",
                    "digest": "sha256:{HASH_DARWIN_ARM}"
                }},
                {{
                    "name": "{ARCHIVE_DARWIN_ARM}.sha256",
                    "digest": "sha256:{HASH_SIDECAR}"
                }},
                {{
                    "name": "{ARCHIVE_DARWIN_ARM}.bundle",
                    "digest": "sha256:{HASH_SIDECAR}"
                }},
                {{
                    "name": "{ARCHIVE_DARWIN_X64}",
                    "digest": "sha256:{HASH_DARWIN_X64}"
                }},
                {{
                    "name": "{ARCHIVE_LINUX_X64}",
                    "digest": "sha256:{HASH_LINUX_X64}"
                }},
                {{
                    "name": "{ARCHIVE_WINDOWS}",
                    "digest": "sha256:{HASH_WINDOWS}"
                }}
            ]
        }}"#
    ))
    .expect("fixture json")
}

#[test]
fn parse_release_ignores_target_commitish_main() {
    let v = release_json_with_sidecar();
    let pins = parse_release_pins(&v).expect("tag_name");
    assert_eq!(pins.tag, LATEST_TAG);
    assert_ne!(pins.tag, "main");
    assert_eq!(v["target_commitish"], "main");
    assert!(pins.sha.is_none());
    assert_ne!(pins.version, "main");
}

#[test]
fn parse_assets_uses_archive_digest_not_sidecar() {
    let v = release_json_with_sidecar();
    let pins = parse_release_pins(&v).expect("assets");
    assert_eq!(
        pins.archives.get(ARCHIVE_DARWIN_ARM).map(String::as_str),
        Some(HASH_DARWIN_ARM)
    );
    assert!(
        !pins
            .archives
            .contains_key(&format!("{ARCHIVE_DARWIN_ARM}.sha256"))
    );
    assert!(!pins.archives.values().any(|h| h == HASH_SIDECAR));
    assert_ne!(
        pins.archives.get(ARCHIVE_DARWIN_ARM).map(String::as_str),
        Some(HASH_SIDECAR)
    );
}

#[test]
fn classify_remote_homebrew_invalid_body_is_unverified() {
    let latest = published_latest();
    let scoop_body = format!(
        r#"{{"version":"0.2.10","architecture":{{"64bit":{{"url":"{}","hash":"{HASH_WINDOWS}"}}}}}}"#,
        scoop_url_for(LATEST_TAG)
    );
    let npm_body = json!({
        "version": MCP_VERSION,
        "ledgerfulEngineTag": LATEST_TAG,
    });
    for body in ["", "not a formula"] {
        let remotes = remotes_from_fetch(&PinFetchBundle {
            latest: Ok(latest.clone()),
            tap: Ok(body.to_string()),
            bucket: Ok(scoop_body.clone()),
            npm: Ok(npm_body.clone()),
        });
        assert!(
            matches!(remotes.homebrew_tap, RemoteFact::Unverified),
            "unparseable tap must be unverified: {body:?}"
        );
        let env = classify_engine(Some(&latest), &matching_locals(), &remotes, false, None);
        assert_eq!(env.status, PinStatus::Unverified);
        assert_eq!(exit_code_for(env.status), 2);
        assert_eq!(surface_status(&env, ID_REMOTE_TAP), PinStatus::Unverified);
        assert_eq!(
            surface_status(&env, ID_PACKAGING_HOMEBREW),
            PinStatus::Match
        );
        assert_eq!(surface_status(&env, ID_REMOTE_BUCKET), PinStatus::Match);
        assert_eq!(surface_status(&env, ID_MCP_NPM), PinStatus::Match);
    }
}

#[test]
fn classify_t12_npm_unverified_no_drift_is_unverified() {
    let latest = published_latest();
    let mut remotes = matching_remotes();
    remotes.npm = RemoteFact::Unverified;
    let env = classify_engine(Some(&latest), &matching_locals(), &remotes, false, None);
    assert_eq!(env.status, PinStatus::Unverified);
    assert_eq!(exit_code_for(env.status), 2);
    assert_eq!(surface_status(&env, ID_MCP_NPM), PinStatus::Unverified);
    assert_eq!(
        surface_status(&env, ID_PACKAGING_HOMEBREW),
        PinStatus::Match
    );
}

#[test]
fn classify_t13_drift_wins_over_unverified() {
    let latest = published_latest();
    let mut locals = matching_locals();
    if let Some(hb) = locals.homebrew.as_mut() {
        hb.version = Some("0.2.9".to_string());
    }
    let mut remotes = matching_remotes();
    remotes.npm = RemoteFact::Unverified;
    let env = classify_engine(Some(&latest), &locals, &remotes, false, None);
    assert_eq!(env.status, PinStatus::Drift);
    assert_eq!(exit_code_for(env.status), 1);
    assert_eq!(
        surface_status(&env, ID_PACKAGING_HOMEBREW),
        PinStatus::Drift
    );
    assert_eq!(surface_status(&env, ID_MCP_NPM), PinStatus::Unverified);
}

#[test]
fn classify_t14_template_ahead_of_latest_is_drift() {
    let latest = published_latest();
    let mut locals = matching_locals();
    if let Some(hb) = locals.homebrew.as_mut() {
        hb.version = Some("0.2.11".to_string());
    }
    let env = classify_engine(Some(&latest), &locals, &matching_remotes(), false, None);
    assert_eq!(env.status, PinStatus::Drift);
    assert_eq!(exit_code_for(env.status), 1);
    assert_eq!(
        surface_status(&env, ID_PACKAGING_HOMEBREW),
        PinStatus::Drift
    );
}

#[test]
fn classify_t15_advisory_mismatch_does_not_fail() {
    let latest = published_latest();
    let env = classify_engine(
        Some(&latest),
        &matching_locals(),
        &matching_remotes(),
        false,
        Some(AdvisoryInput {
            launch_facts_path: "../ledgerful-web/src/lib/content/launch-facts.ts".to_string(),
            release_tag: Some("v0.2.9".to_string()),
            mcp_engine_tag: Some(LATEST_TAG.to_string()),
        }),
    );
    assert_eq!(env.status, PinStatus::Match);
    assert_eq!(exit_code_for(env.status), 0);
    let adv = env.advisory.as_ref().expect("advisory");
    assert_eq!(adv.status, PinStatus::Drift);
    assert_eq!(adv.release_tag.as_deref(), Some("v0.2.9"));
}

#[test]
fn json_surfaces_sorted_by_id_stable() {
    let latest = published_latest();
    let env = classify_engine(
        Some(&latest),
        &matching_locals(),
        &matching_remotes(),
        false,
        None,
    );
    let a = serde_json::to_string(&env).expect("ser a");
    let b = serde_json::to_string(&env).expect("ser b");
    assert_eq!(a, b);
    let expected = [
        ID_MCP_INTREE,
        ID_MCP_NPM,
        ID_PACKAGING_HOMEBREW,
        ID_PACKAGING_SCOOP,
        ID_REMOTE_TAP,
        ID_REMOTE_BUCKET,
    ];
    assert_eq!(ids(&env), expected);
    let v: Value = serde_json::from_str(&a).expect("parse");
    let got: Vec<&str> = v["surfaces"]
        .as_array()
        .expect("surfaces")
        .iter()
        .map(|s| s["id"].as_str().expect("id"))
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn parse_homebrew_pairs_url_with_sha256() {
    let body = r#"
class Ledgerful < Formula
  version "0.2.10"
  sha256 "should-not-pair-without-url"
  on_macos do
on_arm do
  url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.10/ledgerful-aarch64-apple-darwin.tar.gz"
  sha256 "550cbc61bde812017a5fc19d61e00dac7cd59ac14fed0a81bf7dda5ce22d29de"
end
on_intel do
  url "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.10/ledgerful-x86_64-apple-darwin.tar.gz"
  sha256 "149f14faf2f153c1682505e32ca49cca6a35f2375547cb3cef4de8fa5810a614"
end
  end
  url "https://example.com/unpaired.tar.gz"
end
"#;
    let pin = parse_homebrew_formula(body);
    assert_eq!(pin.version.as_deref(), Some("0.2.10"));
    assert_eq!(
        pin.hashes.get(ARCHIVE_DARWIN_ARM).map(String::as_str),
        Some(HASH_DARWIN_ARM)
    );
    assert_eq!(
        pin.hashes.get(ARCHIVE_DARWIN_X64).map(String::as_str),
        Some(HASH_DARWIN_X64)
    );
    assert!(!pin.hashes.contains_key("unpaired.tar.gz"));
    assert!(
        !pin.hashes
            .values()
            .any(|h| h == "should-not-pair-without-url")
    );
}

#[test]
fn parse_scoop_version_and_hash() {
    let body = r#"{
        "version": "0.2.10",
        "architecture": {
            "64bit": {
                "url": "https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.10/ledgerful-x86_64-pc-windows-msvc.zip",
                "hash": "99f6e6bb23f93cd46cdb47a1a196b68d154e7d86e3ab226d3b1f0d4ddedf48ca"
            }
        }
    }"#;
    let pin = parse_scoop_manifest(body).expect("scoop");
    assert_eq!(pin.version.as_deref(), Some("0.2.10"));
    assert_eq!(pin.hash.as_deref(), Some(HASH_WINDOWS));
    assert!(
        pin.url
            .as_deref()
            .is_some_and(|u| u.ends_with(ARCHIVE_WINDOWS))
    );
}

#[test]
fn parse_mcp_ledgerful_engine_tag() {
    let body = r#"{
        "name": "@ledgerful/mcp-server",
        "version": "0.1.19",
        "ledgerfulEngineTag": "v0.2.10"
    }"#;
    let pin = parse_mcp_package(body).expect("mcp");
    assert_eq!(pin.version.as_deref(), Some(MCP_VERSION));
    assert_eq!(pin.ledgerful_engine_tag.as_deref(), Some(LATEST_TAG));
}

fn endpoints(server: &httpmock::MockServer) -> PinFetchEndpoints {
    PinFetchEndpoints {
        github_api_base: server.base_url(),
        npm_latest_url: format!("{}/npm/latest", server.base_url()),
    }
}

fn github_json_when(when: httpmock::When, path: &str, ua: &str) -> httpmock::When {
    when.method(httpmock::Method::GET)
        .path(path)
        .header("User-Agent", ua)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
}

fn github_raw_when(when: httpmock::When, path: &str, ua: &str) -> httpmock::When {
    when.method(httpmock::Method::GET)
        .path(path)
        .header("User-Agent", ua)
        .header("Accept", "application/vnd.github.raw+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
}

fn mock_pin_stack(
    server: &httpmock::MockServer,
    peel_status: u16,
) -> (
    httpmock::Mock<'_>,
    httpmock::Mock<'_>,
    httpmock::Mock<'_>,
    httpmock::Mock<'_>,
    httpmock::Mock<'_>,
) {
    let ua = user_agent();
    let latest_body = serde_json::to_string(&release_json_with_sidecar()).expect("body");
    let latest_mock = server.mock(|when, then| {
        github_json_when(
            when,
            "/repos/Ryan-AI-Studios/Ledgerful/releases/latest",
            ua.as_str(),
        );
        then.status(200)
            .header("content-type", "application/json")
            .body(latest_body);
    });
    let peel_mock = server.mock(|when, then| {
        github_json_when(
            when,
            "/repos/Ryan-AI-Studios/Ledgerful/commits/v0.2.10",
            ua.as_str(),
        );
        if peel_status == 200 {
            then.status(200)
                .header("content-type", "application/json")
                .body(format!(r#"{{"sha":"{LATEST_SHA}"}}"#));
        } else {
            then.status(peel_status)
                .header("content-type", "application/json")
                .body(r#"{"message":"Not Found"}"#);
        }
    });
    let tap_mock = server.mock(|when, then| {
        github_raw_when(
            when,
            "/repos/Ryan-AI-Studios/homebrew-tap/contents/ledgerful.rb",
            ua.as_str(),
        );
        then.status(200).body(r#"version "0.2.10""#);
    });
    let bucket_mock = server.mock(|when, then| {
        github_raw_when(
            when,
            "/repos/Ryan-AI-Studios/scoop-bucket/contents/ledgerful.json",
            ua.as_str(),
        );
        then.status(200)
            .body(r#"{"version":"0.2.10","architecture":{"64bit":{"hash":"99f6e6bb23f93cd46cdb47a1a196b68d154e7d86e3ab226d3b1f0d4ddedf48ca","url":"https://github.com/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.10/ledgerful-x86_64-pc-windows-msvc.zip"}}}"#);
    });
    let npm_mock = server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/npm/latest")
            .header("User-Agent", ua.as_str());
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"version":"0.1.19","ledgerfulEngineTag":"v0.2.10"}"#);
    });
    (latest_mock, peel_mock, tap_mock, bucket_mock, npm_mock)
}

#[test]
#[serial_test::serial(env)]
fn fetch_honors_ledgerful_no_network() {
    let server = httpmock::MockServer::start();
    let (latest_mock, peel_mock, tap_mock, bucket_mock, npm_mock) = mock_pin_stack(&server, 200);
    let _g = TempEnv::set(crate::util::network::NO_NETWORK_ENV, "1");
    let result = fetch_latest_pins(&endpoints(&server));
    assert!(
        matches!(&result.latest, Err(PinFetchError::NetworkDisabled)),
        "NO_NETWORK must fail before HTTP: {}",
        result
            .latest
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default()
    );
    assert_eq!(latest_mock.calls(), 0, "zero HTTP hits on releases/latest");
    assert_eq!(peel_mock.calls(), 0, "zero HTTP hits on peel");
    assert_eq!(tap_mock.calls(), 0, "zero HTTP hits on tap");
    assert_eq!(bucket_mock.calls(), 0, "zero HTTP hits on bucket");
    assert_eq!(npm_mock.calls(), 0, "zero HTTP hits on npm");
}

#[test]
#[serial_test::serial(env)]
fn fetch_sets_user_agent() {
    let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
    let server = httpmock::MockServer::start();
    let (latest_mock, peel_mock, tap_mock, bucket_mock, npm_mock) = mock_pin_stack(&server, 200);
    let got = fetch_latest_pins(&endpoints(&server));
    let latest = got.latest.expect("latest");
    assert_eq!(latest.tag, LATEST_TAG);
    assert_eq!(latest.sha.as_deref(), Some(LATEST_SHA));
    assert_eq!(latest_mock.calls(), 1, "releases/latest must match UA");
    assert_eq!(peel_mock.calls(), 1, "peel must match UA");
    assert_eq!(tap_mock.calls(), 1, "tap must match UA");
    assert_eq!(bucket_mock.calls(), 1, "bucket must match UA");
    assert_eq!(npm_mock.calls(), 1, "npm must match UA");
    assert!(got.tap.is_ok());
    assert!(got.npm.is_ok());
}

#[test]
#[serial_test::serial(env)]
fn peel_still_unused_for_pin_keys() {
    let _g = TempEnv::remove(crate::util::network::NO_NETWORK_ENV);
    let server = httpmock::MockServer::start();
    let (latest_mock, peel_mock, _tap, _bucket, _npm) = mock_pin_stack(&server, 404);
    let decoy_main = server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/repos/Ryan-AI-Studios/Ledgerful/commits/main");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#);
    });
    let got = fetch_latest_pins(&endpoints(&server));
    let latest = got.latest.expect("peel 404 still pins");
    assert_eq!(latest.tag, LATEST_TAG);
    assert!(latest.sha.is_none(), "peel 404 omits sha");
    assert_eq!(
        latest.archives.get(ARCHIVE_DARWIN_ARM).map(String::as_str),
        Some(HASH_DARWIN_ARM)
    );
    assert!(!latest.archives.values().any(|h| h == HASH_SIDECAR));
    assert_eq!(latest_mock.calls(), 1);
    assert_eq!(peel_mock.calls(), 1);
    assert_eq!(decoy_main.calls(), 0, "must not GET /commits/main");
    // SHA-required fetch_github_latest would Err on peel 404; pin keys still work.
}
