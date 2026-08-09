//! Desktop-compatible relay projections for Server-managed agents.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use nostr::{Event, EventBuilder, JsonUtil, Keys, Kind, Tag};
use reqwest::{blocking::Client, Method};
use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{AgentConfigFile, PersonaDefinition, ResolvedAgentConfig};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const KIND_PROFILE: u16 = 0;
const KIND_DELETE: u16 = 5;
const KIND_MANAGED_AGENT: u16 = buzz_core::kind::KIND_MANAGED_AGENT as u16;
const KIND_PERSONA: u16 = buzz_core::kind::KIND_PERSONA as u16;

#[derive(Debug, thiserror::Error)]
pub enum RelayProjectionError {
    #[error("relay projection transport failed: {0}")]
    Transport(String),
    #[error("relay rejected projection: {0}")]
    Rejected(String),
    #[error("relay projection could not be built: {0}")]
    Invalid(String),
}

#[derive(Debug, Serialize)]
struct ManagedAgentEventContent<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona_source_version: Option<&'a str>,
    parallelism: u32,
    respond_to: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    respond_to_allowlist: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PersonaEventContent<'a> {
    display_name: &'a str,
    system_prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    name_pool: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    respond_to: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    respond_to_allowlist: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallelism: Option<u32>,
}

pub fn sync_agent_profile(
    relay_url: &Url,
    owner_keys: &Keys,
    agent_keys: &Keys,
    file: &AgentConfigFile,
) -> Result<(), RelayProjectionError> {
    let client = client()?;
    let api_base = relay_http_base_url(relay_url);
    sync_profile(&client, &api_base, owner_keys, agent_keys, file)
}

pub fn sync_managed_agent_projection(
    relay_url: &Url,
    owner_keys: &Keys,
    agent_keys: &Keys,
    resolved: &ResolvedAgentConfig,
) -> Result<(), RelayProjectionError> {
    let client = client()?;
    let api_base = relay_http_base_url(relay_url);
    sync_managed_agent(&client, &api_base, owner_keys, agent_keys, resolved)
}

fn sync_profile(
    client: &Client,
    api_base: &str,
    owner_keys: &Keys,
    agent_keys: &Keys,
    file: &AgentConfigFile,
) -> Result<(), RelayProjectionError> {
    let content = serde_json::json!({
        "display_name": file.display_name,
        "picture": file.avatar_url,
    });
    let mut profile = content.as_object().cloned().unwrap_or_default();
    profile.retain(|_, value| !value.is_null());
    let content = serde_json::Value::Object(profile).to_string();
    let current = query_latest(
        client,
        api_base,
        owner_keys,
        serde_json::json!({
            "authors": [agent_keys.public_key().to_hex()],
            "kinds": [KIND_PROFILE],
            "limit": 1
        }),
    )?;
    if current
        .as_ref()
        .is_some_and(|event| event.content == content)
    {
        return Ok(());
    }

    let auth_tag =
        buzz_sdk::nip_oa::compute_auth_tag(owner_keys, &agent_keys.public_key(), "kind=0")
            .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let tag = buzz_sdk::nip_oa::parse_auth_tag(&auth_tag)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let event = EventBuilder::new(Kind::Metadata, content)
        .tags([tag])
        .sign_with_keys(agent_keys)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    submit_event(client, api_base, agent_keys, &event, Some(&auth_tag))
}

fn sync_managed_agent(
    client: &Client,
    api_base: &str,
    owner_keys: &Keys,
    agent_keys: &Keys,
    resolved: &ResolvedAgentConfig,
) -> Result<(), RelayProjectionError> {
    let linked = resolved.persona_id.is_some();
    let content = ManagedAgentEventContent {
        name: &resolved.spec.display_name,
        persona_id: resolved.persona_id.as_deref(),
        system_prompt: (!linked && !resolved.spec.system_prompt.is_empty())
            .then_some(resolved.spec.system_prompt.as_str()),
        model: (!linked).then_some(resolved.model.as_deref()).flatten(),
        provider: (!linked).then_some(resolved.provider.as_deref()).flatten(),
        persona_source_version: None,
        parallelism: resolved.parallelism,
        respond_to: resolved.respond_to.as_str(),
        respond_to_allowlist: resolved.respond_to_allowlist.clone(),
    };
    let content = serde_json::to_string(&content)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let agent_pubkey = agent_keys.public_key().to_hex();
    let current = query_latest(
        client,
        api_base,
        owner_keys,
        serde_json::json!({
            "authors": [owner_keys.public_key().to_hex()],
            "kinds": [KIND_MANAGED_AGENT],
            "#d": [agent_pubkey],
            "limit": 1
        }),
    )?;
    if current
        .as_ref()
        .is_some_and(|event| event.content == content)
    {
        return Ok(());
    }
    let d = Tag::parse(["d", agent_pubkey.as_str()])
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let event = EventBuilder::new(Kind::Custom(KIND_MANAGED_AGENT), content)
        .tags([d])
        .sign_with_keys(owner_keys)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    submit_event(client, api_base, owner_keys, &event, None)
}

pub fn sync_persona(
    relay_url: &Url,
    owner_keys: &Keys,
    persona: &PersonaDefinition,
) -> Result<(), RelayProjectionError> {
    let client = client()?;
    let api_base = relay_http_base_url(relay_url);
    let d_tag = persona_d_tag(&persona.id);
    let content = PersonaEventContent {
        display_name: &persona.display_name,
        system_prompt: Some(&persona.system_prompt),
        avatar_url: persona.avatar_url.as_deref(),
        runtime: persona.runtime.as_ref().map(ToString::to_string),
        model: persona.model.as_deref(),
        provider: persona.provider.as_deref(),
        name_pool: persona.name_pool.clone(),
        respond_to: persona.respond_to.map(|v| v.as_str()),
        respond_to_allowlist: persona.respond_to_allowlist.clone(),
        parallelism: persona.parallelism,
    };
    let content = serde_json::to_string(&content)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let current = query_latest(
        &client,
        &api_base,
        owner_keys,
        serde_json::json!({
            "authors": [owner_keys.public_key().to_hex()],
            "kinds": [KIND_PERSONA],
            "#d": [d_tag],
            "limit": 1
        }),
    )?;
    if current.as_ref().is_some_and(|event| {
        let current_shared = event.tags.iter().any(|tag| {
            let values = tag.as_slice();
            values.first().map(String::as_str) == Some("shared")
                && values.get(1).map(String::as_str) == Some("true")
        });
        event.content == content && current_shared == persona.shared
    }) {
        return Ok(());
    }
    let d = Tag::parse(["d", d_tag.as_str()])
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let mut tags = vec![d];
    if persona.shared {
        tags.push(
            Tag::parse(["shared", "true"])
                .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?,
        );
    }
    let event = EventBuilder::new(Kind::Custom(KIND_PERSONA), content)
        .tags(tags)
        .sign_with_keys(owner_keys)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    submit_event(&client, &api_base, owner_keys, &event, None)
}

pub fn tombstone_managed_agent(
    relay_url: &Url,
    owner_keys: &Keys,
    agent_pubkey: &str,
) -> Result<(), RelayProjectionError> {
    tombstone_coordinate(relay_url, owner_keys, KIND_MANAGED_AGENT, agent_pubkey)
}

pub fn archive_identity(
    relay_url: &Url,
    owner_keys: &Keys,
    agent_pubkey: &str,
) -> Result<(), RelayProjectionError> {
    let target = nostr::PublicKey::from_hex(agent_pubkey)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let auth_json = buzz_sdk::nip_oa::compute_auth_tag(owner_keys, &target, "")
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let parts: Vec<String> = serde_json::from_str(&auth_json)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let auth: [String; 4] = parts.try_into().map_err(|_| {
        RelayProjectionError::Invalid("archive auth tag must have four elements".into())
    })?;
    let event = buzz_sdk::builders::build_archive_identity_request(
        agent_pubkey,
        "",
        Some("retired"),
        None,
        Some(&auth),
    )
    .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?
    .sign_with_keys(owner_keys)
    .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let client = client()?;
    let api_base = relay_http_base_url(relay_url);
    submit_event(&client, &api_base, owner_keys, &event, None)
}

pub fn tombstone_persona(
    relay_url: &Url,
    owner_keys: &Keys,
    persona_id: &str,
) -> Result<(), RelayProjectionError> {
    tombstone_coordinate(
        relay_url,
        owner_keys,
        KIND_PERSONA,
        &persona_d_tag(persona_id),
    )
}

fn tombstone_coordinate(
    relay_url: &Url,
    owner_keys: &Keys,
    kind: u16,
    d_tag: &str,
) -> Result<(), RelayProjectionError> {
    let client = client()?;
    let api_base = relay_http_base_url(relay_url);
    let coordinate = format!("{kind}:{}:{d_tag}", owner_keys.public_key().to_hex());
    let tag = Tag::parse(["a", coordinate.as_str()])
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let event = EventBuilder::new(Kind::Custom(KIND_DELETE), "")
        .tags([tag])
        .sign_with_keys(owner_keys)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    submit_event(&client, &api_base, owner_keys, &event, None)
}

fn query_latest(
    client: &Client,
    api_base: &str,
    keys: &Keys,
    filter: serde_json::Value,
) -> Result<Option<Event>, RelayProjectionError> {
    let url = format!("{api_base}/query");
    let body =
        serde_json::to_vec(&[filter]).map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    let auth = nip98_auth_header(keys, &Method::POST, &url, &body)?;
    let response = client
        .post(&url)
        .header("Authorization", auth)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .map_err(|e| RelayProjectionError::Transport(e.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .map_err(|e| RelayProjectionError::Transport(e.to_string()))?;
    if !status.is_success() {
        return Err(RelayProjectionError::Rejected(text));
    }
    let events: Vec<Event> =
        serde_json::from_str(&text).map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    Ok(events.into_iter().next())
}

fn submit_event(
    client: &Client,
    api_base: &str,
    keys: &Keys,
    event: &Event,
    auth_tag: Option<&str>,
) -> Result<(), RelayProjectionError> {
    let url = format!("{api_base}/events");
    let body = event.as_json().into_bytes();
    let auth = nip98_auth_header(keys, &Method::POST, &url, &body)?;
    let mut request = client
        .post(&url)
        .header("Authorization", auth)
        .header("Content-Type", "application/json");
    if let Some(auth_tag) = auth_tag {
        request = request.header("x-auth-tag", auth_tag);
    }
    let response = request
        .body(body)
        .send()
        .map_err(|e| RelayProjectionError::Transport(e.to_string()))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let text = response.text().unwrap_or_default();
    Err(RelayProjectionError::Rejected(text))
}

fn nip98_auth_header(
    keys: &Keys,
    method: &Method,
    url: &str,
    body: &[u8],
) -> Result<String, RelayProjectionError> {
    let payload_hash = format!("{:x}", Sha256::digest(body));
    let nonce = uuid::Uuid::now_v7().to_string();
    let tags = vec![
        Tag::parse(vec!["u", url]).map_err(|e| RelayProjectionError::Invalid(e.to_string()))?,
        Tag::parse(vec!["method", method.as_str()])
            .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?,
        Tag::parse(vec!["payload", &payload_hash])
            .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?,
        Tag::parse(vec!["nonce", &nonce])
            .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?,
    ];
    let event = EventBuilder::new(Kind::HttpAuth, "")
        .tags(tags)
        .sign_with_keys(keys)
        .map_err(|e| RelayProjectionError::Invalid(e.to_string()))?;
    Ok(format!(
        "Nostr {}",
        BASE64.encode(event.as_json().as_bytes())
    ))
}

fn client() -> Result<Client, RelayProjectionError> {
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| RelayProjectionError::Transport(e.to_string()))
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

pub fn persona_d_tag(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if !out
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        out.insert(0, 'a');
    }
    out.truncate(64);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_d_tag_matches_desktop_normalization() {
        assert_eq!(persona_d_tag("CodeReviewer"), "codereviewer");
        assert_eq!(persona_d_tag("_ops"), "a_ops");
    }

    #[test]
    fn managed_agent_projection_omits_definition_owned_fields_when_linked() {
        let content = ManagedAgentEventContent {
            name: "Review bot",
            persona_id: Some("reviewer"),
            system_prompt: None,
            model: None,
            provider: None,
            persona_source_version: None,
            parallelism: 10,
            respond_to: "owner-only",
            respond_to_allowlist: vec![],
        };
        let value = serde_json::to_value(content).unwrap();
        assert_eq!(value["name"], "Review bot");
        assert_eq!(value["persona_id"], "reviewer");
        assert!(value.get("system_prompt").is_none());
        assert!(value.get("model").is_none());
        assert!(value.get("provider").is_none());
    }
}
