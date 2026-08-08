//! Existing-community join verification copied from Buzz Desktop's relay membership flow.
//!
//! Desktop applies the selected relay using the persisted Buzz identity, reads the relay's
//! NIP-11 information document, and only queries the NIP-43 membership snapshot (kind 13534)
//! when the relay advertises NIP-43. Open relays therefore do not require a membership record.

use std::{fs, path::PathBuf, time::Duration};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use nostr::{Event, EventBuilder, JsonUtil, Keys, Kind, Tag};
use reqwest::{blocking::Client, Method, StatusCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::CommunityConfig;

const KIND_NIP43_MEMBERSHIP_LIST: u16 = 13_534;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum CommunityJoinError {
    #[error("relay unreachable: {0}")]
    Unreachable(&'static str),
    #[error("this Buzz Server identity is not a member of the community")]
    MembershipDenied,
    #[error("relay returned an invalid membership response")]
    InvalidResponse,
}

#[derive(Deserialize)]
struct RelayInformationDocument {
    #[serde(default)]
    supported_nips: Vec<u16>,
}

#[derive(Clone)]
pub struct DesktopCommunityJoinVerifier {
    client: Client,
    custody_root: PathBuf,
}

impl DesktopCommunityJoinVerifier {
    pub fn new(custody_root: PathBuf) -> Result<Self, CommunityJoinError> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| CommunityJoinError::Unreachable("could not initialize relay client"))?;
        Ok(Self {
            client,
            custody_root,
        })
    }

    /// Verify an existing community using the same membership semantics as Buzz Desktop.
    ///
    /// 1. GET `/info` and inspect `supported_nips`.
    /// 2. If NIP-43 is not advertised, treat the relay as open and allow the join.
    /// 3. If NIP-43 is advertised, POST `/query` for kind 13534 using NIP-98 signed by the
    ///    persisted Buzz Server identity.
    /// 4. A returned snapshot that omits our pubkey denies the join. As on Desktop, a relay
    ///    advertising NIP-43 but returning no snapshot is not treated as a hard denial.
    pub fn verify(&self, community: &CommunityConfig) -> Result<(), CommunityJoinError> {
        let pubkey = community
            .identity_pubkey
            .as_deref()
            .ok_or(CommunityJoinError::InvalidResponse)?;
        let secret_path = self.custody_root.join(format!("{pubkey}.secret"));
        let secret =
            fs::read_to_string(secret_path).map_err(|_| CommunityJoinError::InvalidResponse)?;
        let keys = Keys::parse(secret.trim()).map_err(|_| CommunityJoinError::InvalidResponse)?;
        if !keys.public_key().to_hex().eq_ignore_ascii_case(pubkey) {
            return Err(CommunityJoinError::InvalidResponse);
        }
        let api_base = relay_http_base_url(&community.relay_url);
        let info_url = format!("{api_base}/info");
        let info_response = self
            .client
            .get(&info_url)
            .header("Accept", "application/nostr+json")
            .send()
            .map_err(classify_request_error)?;
        if !info_response.status().is_success() {
            return Err(classify_status(info_response.status()));
        }
        let info: RelayInformationDocument = info_response
            .json()
            .map_err(|_| CommunityJoinError::InvalidResponse)?;
        if !info.supported_nips.contains(&43) {
            return Ok(());
        }

        let query_url = format!("{api_base}/query");
        let body = serde_json::to_vec(&[serde_json::json!({
            "kinds": [KIND_NIP43_MEMBERSHIP_LIST],
            "limit": 1
        })])
        .map_err(|_| CommunityJoinError::InvalidResponse)?;
        let auth = build_nip98_auth_header(&keys, &Method::POST, &query_url, &body)?;
        let response = self
            .client
            .post(&query_url)
            .header("Authorization", auth)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .map_err(classify_request_error)?;
        if !response.status().is_success() {
            return Err(classify_status(response.status()));
        }
        let events: Vec<Event> = response
            .json()
            .map_err(|_| CommunityJoinError::InvalidResponse)?;
        let Some(snapshot) = events.first() else {
            // Desktop deliberately does not hard-fail when a relay advertises NIP-43 but its
            // membership snapshot is missing; the UI warns instead. Preserve that behavior.
            return Ok(());
        };
        let my_pubkey = keys.public_key().to_hex();
        if membership_snapshot_contains(snapshot, &my_pubkey) {
            Ok(())
        } else {
            Err(CommunityJoinError::MembershipDenied)
        }
    }
}

fn relay_http_base_url(relay_url: &Url) -> String {
    let mut value = relay_url.as_str().trim_end_matches('/').to_owned();
    if let Some(suffix) = value.strip_prefix("wss://") {
        value = format!("https://{suffix}");
    } else if let Some(suffix) = value.strip_prefix("ws://") {
        value = format!("http://{suffix}");
    }
    value
}

fn build_nip98_auth_header(
    keys: &Keys,
    method: &Method,
    url: &str,
    body: &[u8],
) -> Result<String, CommunityJoinError> {
    // This mirrors Desktop's build_nip98_auth_header_for_keys: URL, method, payload hash and a
    // nonce are signed into a kind-27235 HTTP-auth event.
    let payload_hash = format!("{:x}", Sha256::digest(body));
    let nonce = uuid::Uuid::now_v7().to_string();
    let tags = vec![
        Tag::parse(vec!["u", url]).map_err(|_| CommunityJoinError::InvalidResponse)?,
        Tag::parse(vec!["method", method.as_str()])
            .map_err(|_| CommunityJoinError::InvalidResponse)?,
        Tag::parse(vec!["payload", &payload_hash])
            .map_err(|_| CommunityJoinError::InvalidResponse)?,
        Tag::parse(vec!["nonce", &nonce]).map_err(|_| CommunityJoinError::InvalidResponse)?,
    ];
    let event = EventBuilder::new(Kind::HttpAuth, "")
        .tags(tags)
        .sign_with_keys(keys)
        .map_err(|_| CommunityJoinError::InvalidResponse)?;
    Ok(format!(
        "Nostr {}",
        BASE64.encode(event.as_json().as_bytes())
    ))
}

fn membership_snapshot_contains(event: &Event, pubkey: &str) -> bool {
    event.tags.iter().any(|tag| {
        let values = tag.as_slice();
        let Some(name) = values.first().map(String::as_str) else {
            return false;
        };
        if name != "member" && name != "p" {
            return false;
        }
        values
            .get(1)
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(pubkey))
    })
}

fn classify_request_error(error: reqwest::Error) -> CommunityJoinError {
    if error.is_timeout() {
        CommunityJoinError::Unreachable("request timed out")
    } else if error.is_connect() {
        CommunityJoinError::Unreachable("could not connect to relay")
    } else {
        CommunityJoinError::Unreachable("network error")
    }
}

fn classify_status(status: StatusCode) -> CommunityJoinError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        CommunityJoinError::MembershipDenied
    } else {
        CommunityJoinError::InvalidResponse
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_urls_map_to_desktop_http_bridge_urls() {
        assert_eq!(
            relay_http_base_url(&Url::parse("wss://community.example/path/").unwrap()),
            "https://community.example/path"
        );
        assert_eq!(
            relay_http_base_url(&Url::parse("ws://127.0.0.1:3000/").unwrap()),
            "http://127.0.0.1:3000"
        );
    }

    #[test]
    fn membership_snapshot_accepts_member_and_p_tags() {
        let keys = Keys::generate();
        let pubkey = keys.public_key().to_hex();
        let event = EventBuilder::new(Kind::Custom(KIND_NIP43_MEMBERSHIP_LIST), "")
            .tags([Tag::parse(vec!["member", &pubkey, "member"]).unwrap()])
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(membership_snapshot_contains(&event, &pubkey));
    }
}
