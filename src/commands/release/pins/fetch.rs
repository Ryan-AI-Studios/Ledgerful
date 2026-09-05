use super::parse::{
    parse_commit_sha, parse_homebrew_formula, parse_npm_document, parse_release_pins,
    parse_scoop_manifest,
};
use super::types::{
    GITHUB_API_VERSION, GITHUB_OWNER_REPO, HOMEBREW_TAP_PATH, HOMEBREW_TAP_REPO, PinFetchBundle,
    PinFetchEndpoints, PinFetchError, RemoteFact, RemotePins, SCOOP_BUCKET_PATH, SCOOP_BUCKET_REPO,
};
use crate::util::network::network_disabled_from_env;
use serde_json::Value;
use std::time::Duration;

const FETCH_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) fn user_agent() -> String {
    format!("ledgerful/{}", env!("CARGO_PKG_VERSION"))
}

fn map_ureq(err: ureq::Error) -> PinFetchError {
    match err {
        ureq::Error::Status(code, _resp) => PinFetchError::Http {
            status: Some(code),
            detail: format!("HTTP {code}"),
        },
        ureq::Error::Transport(inner) => PinFetchError::Http {
            status: None,
            detail: inner.to_string(),
        },
    }
}

fn github_get_json(agent: &ureq::Agent, url: &str) -> Result<Value, PinFetchError> {
    if network_disabled_from_env() {
        return Err(PinFetchError::NetworkDisabled);
    }
    let ua = user_agent();
    let resp = agent
        .get(url)
        .set("User-Agent", &ua)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(map_ureq)?;
    resp.into_json()
        .map_err(|_| PinFetchError::InvalidBody("invalid JSON"))
}

fn github_get_raw(agent: &ureq::Agent, url: &str) -> Result<String, PinFetchError> {
    if network_disabled_from_env() {
        return Err(PinFetchError::NetworkDisabled);
    }
    let ua = user_agent();
    let resp = agent
        .get(url)
        .set("User-Agent", &ua)
        .set("Accept", "application/vnd.github.raw+json")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(map_ureq)?;
    resp.into_string()
        .map_err(|_| PinFetchError::InvalidBody("invalid text body"))
}

fn npm_get_json(agent: &ureq::Agent, url: &str) -> Result<Value, PinFetchError> {
    if network_disabled_from_env() {
        return Err(PinFetchError::NetworkDisabled);
    }
    let ua = user_agent();
    let resp = agent
        .get(url)
        .set("User-Agent", &ua)
        .set("Accept", "application/json")
        .call()
        .map_err(map_ureq)?;
    resp.into_json()
        .map_err(|_| PinFetchError::InvalidBody("invalid JSON"))
}

fn disabled_bundle() -> PinFetchBundle {
    PinFetchBundle {
        latest: Err(PinFetchError::NetworkDisabled),
        tap: Err(PinFetchError::NetworkDisabled),
        bucket: Err(PinFetchError::NetworkDisabled),
        npm: Err(PinFetchError::NetworkDisabled),
    }
}

fn try_peel_sha(agent: &ureq::Agent, base: &str, tag: &str) -> Option<String> {
    if network_disabled_from_env() {
        return None;
    }
    let url = format!("{base}/repos/{GITHUB_OWNER_REPO}/commits/{tag}");
    let json = github_get_json(agent, &url).ok()?;
    parse_commit_sha(&json)
}

fn fetch_remotes(
    agent: &ureq::Agent,
    endpoints: &PinFetchEndpoints,
) -> (
    Result<String, PinFetchError>,
    Result<String, PinFetchError>,
    Result<Value, PinFetchError>,
) {
    let base = endpoints.github_api_base.trim_end_matches('/').to_string();
    let npm_url = endpoints.npm_latest_url.clone();
    let tap_url = format!("{base}/repos/{HOMEBREW_TAP_REPO}/contents/{HOMEBREW_TAP_PATH}");
    let bucket_url = format!("{base}/repos/{SCOOP_BUCKET_REPO}/contents/{SCOOP_BUCKET_PATH}");
    let tap_agent = agent.clone();
    let bucket_agent = agent.clone();
    let npm_agent = agent.clone();
    std::thread::scope(|s| {
        let tap_h = s.spawn(|| github_get_raw(&tap_agent, &tap_url));
        let bucket_h = s.spawn(|| github_get_raw(&bucket_agent, &bucket_url));
        let npm_h = s.spawn(|| npm_get_json(&npm_agent, &npm_url));
        let tap = tap_h.join().unwrap_or_else(|_| {
            Err(PinFetchError::Http {
                status: None,
                detail: "tap thread panicked".to_string(),
            })
        });
        let bucket = bucket_h.join().unwrap_or_else(|_| {
            Err(PinFetchError::Http {
                status: None,
                detail: "bucket thread panicked".to_string(),
            })
        });
        let npm = npm_h.join().unwrap_or_else(|_| {
            Err(PinFetchError::Http {
                status: None,
                detail: "npm thread panicked".to_string(),
            })
        });
        (tap, bucket, npm)
    })
}

/// Fetch GitHub Latest pin keys (tag + archive digests) and remotes.
///
/// Peel SHA is optional: 404/invalid peel still returns tag + digests.
/// Never calls `fetch_github_latest`. Never sends `GITHUB_TOKEN`. No retry.
pub(crate) fn fetch_latest_pins(endpoints: &PinFetchEndpoints) -> PinFetchBundle {
    if network_disabled_from_env() {
        return disabled_bundle();
    }

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(FETCH_TIMEOUT)
        .timeout_read(FETCH_TIMEOUT)
        .build();

    let base = endpoints.github_api_base.trim_end_matches('/');
    let release_url = format!("{base}/repos/{GITHUB_OWNER_REPO}/releases/latest");
    let release_json = match github_get_json(&agent, &release_url) {
        Ok(v) => v,
        Err(e) => {
            return PinFetchBundle {
                latest: Err(e),
                tap: Err(PinFetchError::InvalidBody("latest unavailable")),
                bucket: Err(PinFetchError::InvalidBody("latest unavailable")),
                npm: Err(PinFetchError::InvalidBody("latest unavailable")),
            };
        }
    };
    let mut latest = match parse_release_pins(&release_json) {
        Some(p) => p,
        None => {
            return PinFetchBundle {
                latest: Err(PinFetchError::InvalidBody("empty tag_name")),
                tap: Err(PinFetchError::InvalidBody("latest unavailable")),
                bucket: Err(PinFetchError::InvalidBody("latest unavailable")),
                npm: Err(PinFetchError::InvalidBody("latest unavailable")),
            };
        }
    };
    latest.sha = try_peel_sha(&agent, base, &latest.tag);

    let (tap, bucket, npm) = fetch_remotes(&agent, endpoints);
    PinFetchBundle {
        latest: Ok(latest),
        tap,
        bucket,
        npm,
    }
}

pub(super) fn remotes_from_fetch(fetched: &PinFetchBundle) -> RemotePins {
    let homebrew_tap = match &fetched.tap {
        Ok(body) => {
            let pin = parse_homebrew_formula(body);
            if pin.version.is_none() && pin.hashes.is_empty() {
                RemoteFact::Unverified
            } else {
                RemoteFact::Value(pin)
            }
        }
        Err(_) => RemoteFact::Unverified,
    };
    let scoop_bucket = match &fetched.bucket {
        Ok(body) => match parse_scoop_manifest(body) {
            Some(pin) => RemoteFact::Value(pin),
            None => RemoteFact::Unverified,
        },
        Err(_) => RemoteFact::Unverified,
    };
    let npm = match &fetched.npm {
        Ok(value) => match parse_npm_document(value) {
            Some(pin) => RemoteFact::Value(pin),
            None => RemoteFact::Unverified,
        },
        Err(_) => RemoteFact::Unverified,
    };
    RemotePins {
        homebrew_tap,
        scoop_bucket,
        npm,
    }
}
