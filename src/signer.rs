//! Disposable, policy-constrained NIP-OA signer boundary.
//!
//! This module deliberately exposes one operation. It is not a general signing API and does not
//! support production key import or key-management systems.

use std::str::FromStr;

use buzz_core::{Keys, PublicKey};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::CommunityConfigId;

pub const MAX_SIGNER_FRAME_BYTES: usize = 16 * 1024;
const AUTHORIZE_AGENT_ACTION: &str = "authorize_agent";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthorizeAgentRequest {
    pub action: String,
    pub community_config_id: CommunityConfigId,
    pub relay_url: Url,
    pub agent_pubkey: String,
    pub conditions: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthorizeAgentResponse {
    pub owner_pubkey: String,
    pub agent_pubkey: String,
    pub community_config_id: CommunityConfigId,
    pub relay_url: Url,
    pub auth_tag: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerErrorCode {
    MalformedFrame,
    FrameTooLarge,
    InvalidRequest,
    ActionNotAllowed,
    CommunityNotAllowed,
    RelayNotAllowed,
    AgentNotAllowed,
    ConditionsNotAllowed,
    AuthorizationFailed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct SignerError {
    pub code: SignerErrorCode,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SignerIpcResponse {
    Authorized(AuthorizeAgentResponse),
    Error(SignerError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignerPolicy {
    pub community_config_id: CommunityConfigId,
    pub relay_url: Url,
    pub agent_pubkey: String,
    pub conditions: String,
}

/// Generated agent material for disposable tests and development only.
///
/// It intentionally implements neither `Serialize` nor `Debug`.
pub struct DisposableAgentKeys(Keys);

impl DisposableAgentKeys {
    #[must_use]
    pub fn public_key_hex(&self) -> String {
        self.0.public_key().to_hex()
    }
}

/// A disposable signer whose owner secret is private and non-serializable.
pub struct DisposableSigner {
    owner_keys: Keys,
    policy: SignerPolicy,
}

impl DisposableSigner {
    /// Constructs a constrained signer from an injected durable owner identity
    /// and the exact configured-agent policy. Secret persistence remains the
    /// caller's responsibility and neither key is serializable through this type.
    pub fn from_owner_keys(owner_keys: Keys, policy: SignerPolicy) -> Result<Self, SignerError> {
        let agent = PublicKey::from_str(&policy.agent_pubkey).map_err(|_| {
            error(
                SignerErrorCode::InvalidRequest,
                "agent public key is invalid",
            )
        })?;
        if owner_keys.public_key() == agent {
            return Err(error(
                SignerErrorCode::InvalidRequest,
                "owner and agent identities must be distinct",
            ));
        }
        Ok(Self { owner_keys, policy })
    }

    /// Generates a disposable owner/agent pair and binds the signer to the supplied scope.
    #[must_use]
    pub fn generate(
        community_config_id: CommunityConfigId,
        relay_url: Url,
        conditions: impl Into<String>,
    ) -> (Self, DisposableAgentKeys) {
        let owner_keys = Keys::generate();
        let agent_keys = DisposableAgentKeys(Keys::generate());
        let policy = SignerPolicy {
            community_config_id,
            relay_url,
            agent_pubkey: agent_keys.public_key_hex(),
            conditions: conditions.into(),
        };
        (Self { owner_keys, policy }, agent_keys)
    }

    #[must_use]
    pub fn policy(&self) -> &SignerPolicy {
        &self.policy
    }

    pub fn authorize_agent(
        &self,
        request: &AuthorizeAgentRequest,
    ) -> Result<AuthorizeAgentResponse, SignerError> {
        if request.action != AUTHORIZE_AGENT_ACTION {
            return Err(error(
                SignerErrorCode::ActionNotAllowed,
                "requested action is not allowed",
            ));
        }
        if request.community_config_id != self.policy.community_config_id {
            return Err(error(
                SignerErrorCode::CommunityNotAllowed,
                "requested community is not allowed",
            ));
        }
        if request.relay_url != self.policy.relay_url {
            return Err(error(
                SignerErrorCode::RelayNotAllowed,
                "requested relay is not allowed",
            ));
        }
        if request.agent_pubkey != self.policy.agent_pubkey {
            return Err(error(
                SignerErrorCode::AgentNotAllowed,
                "requested agent is not allowed",
            ));
        }
        if request.conditions != self.policy.conditions {
            return Err(error(
                SignerErrorCode::ConditionsNotAllowed,
                "requested conditions are not allowed",
            ));
        }

        let agent_pubkey = PublicKey::from_str(&request.agent_pubkey).map_err(|_| {
            error(
                SignerErrorCode::InvalidRequest,
                "agent public key is invalid",
            )
        })?;
        let auth_tag = buzz_sdk::nip_oa::compute_auth_tag(
            &self.owner_keys,
            &agent_pubkey,
            &request.conditions,
        )
        .map_err(|_| {
            error(
                SignerErrorCode::AuthorizationFailed,
                "authorization could not be produced",
            )
        })?;

        Ok(AuthorizeAgentResponse {
            owner_pubkey: self.owner_keys.public_key().to_hex(),
            agent_pubkey: request.agent_pubkey.clone(),
            community_config_id: request.community_config_id,
            relay_url: request.relay_url.clone(),
            auth_tag,
        })
    }

    /// Handles one unsigned big-endian-length-prefixed JSON request frame.
    ///
    /// This is the testable boundary used by a future Unix socket accept loop; the transport can
    /// pass each complete frame here without gaining any signing capability beyond authorization.
    #[must_use]
    pub fn handle_frame(&self, frame: &[u8]) -> Vec<u8> {
        let response = match decode_frame(frame) {
            Ok(payload) => match serde_json::from_slice::<AuthorizeAgentRequest>(payload) {
                Ok(request) => match self.authorize_agent(&request) {
                    Ok(response) => SignerIpcResponse::Authorized(response),
                    Err(error) => SignerIpcResponse::Error(error),
                },
                Err(_) => SignerIpcResponse::Error(error(
                    SignerErrorCode::InvalidRequest,
                    "request JSON is invalid",
                )),
            },
            Err(error) => SignerIpcResponse::Error(error),
        };
        encode_frame(&response)
    }
}

#[must_use]
pub fn encode_authorize_request(request: &AuthorizeAgentRequest) -> Vec<u8> {
    encode_frame(request)
}

pub fn decode_signer_response(frame: &[u8]) -> Result<SignerIpcResponse, SignerError> {
    let payload = decode_frame(frame)?;
    serde_json::from_slice(payload)
        .map_err(|_| error(SignerErrorCode::InvalidRequest, "response JSON is invalid"))
}

fn decode_frame(frame: &[u8]) -> Result<&[u8], SignerError> {
    let length_bytes: [u8; 4] = frame
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| error(SignerErrorCode::MalformedFrame, "frame header is invalid"))?;
    let payload_length = u32::from_be_bytes(length_bytes) as usize;
    if payload_length > MAX_SIGNER_FRAME_BYTES {
        return Err(error(
            SignerErrorCode::FrameTooLarge,
            "frame exceeds the maximum size",
        ));
    }
    if frame.len() != payload_length + 4 {
        return Err(error(
            SignerErrorCode::MalformedFrame,
            "frame length does not match its payload",
        ));
    }
    Ok(&frame[4..])
}

fn encode_frame<T: Serialize>(value: &T) -> Vec<u8> {
    let payload = serde_json::to_vec(value).expect("signer protocol types must serialize");
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    frame
}

fn error(code: SignerErrorCode, message: &'static str) -> SignerError {
    SignerError {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> (DisposableSigner, DisposableAgentKeys) {
        DisposableSigner::generate(
            CommunityConfigId::new(),
            Url::parse("wss://relay.example.test").unwrap(),
            "kind=9",
        )
    }

    fn request(signer: &DisposableSigner) -> AuthorizeAgentRequest {
        AuthorizeAgentRequest {
            action: AUTHORIZE_AGENT_ACTION.into(),
            community_config_id: signer.policy.community_config_id,
            relay_url: signer.policy.relay_url.clone(),
            agent_pubkey: signer.policy.agent_pubkey.clone(),
            conditions: signer.policy.conditions.clone(),
        }
    }

    #[test]
    fn valid_authorization_uses_shared_nip_oa() {
        let (signer, agent) = signer();
        let response = signer.authorize_agent(&request(&signer)).unwrap();
        let agent_pubkey = PublicKey::from_str(&agent.public_key_hex()).unwrap();
        let owner = buzz_sdk::nip_oa::verify_auth_tag(&response.auth_tag, &agent_pubkey).unwrap();
        assert_eq!(owner.to_hex(), response.owner_pubkey);
    }

    #[test]
    fn injected_owner_and_agent_identities_are_stable_across_restart() {
        let owner = Keys::parse("0000000000000000000000000000000000000000000000000000000000000001")
            .unwrap();
        let agent = Keys::parse("0000000000000000000000000000000000000000000000000000000000000002")
            .unwrap();
        let policy = SignerPolicy {
            community_config_id: CommunityConfigId::new(),
            relay_url: Url::parse("wss://relay.example.test/").unwrap(),
            agent_pubkey: agent.public_key().to_hex(),
            conditions: "kind=9".into(),
        };
        let request = AuthorizeAgentRequest {
            action: AUTHORIZE_AGENT_ACTION.into(),
            community_config_id: policy.community_config_id,
            relay_url: policy.relay_url.clone(),
            agent_pubkey: policy.agent_pubkey.clone(),
            conditions: policy.conditions.clone(),
        };
        let first = DisposableSigner::from_owner_keys(owner.clone(), policy.clone())
            .unwrap()
            .authorize_agent(&request)
            .unwrap();
        let restarted = DisposableSigner::from_owner_keys(owner.clone(), policy)
            .unwrap()
            .authorize_agent(&request)
            .unwrap();
        assert_eq!(first.owner_pubkey, restarted.owner_pubkey);
        assert_eq!(first.agent_pubkey, restarted.agent_pubkey);
        let first_owner =
            buzz_sdk::nip_oa::verify_auth_tag(&first.auth_tag, &agent.public_key()).unwrap();
        let restarted_owner =
            buzz_sdk::nip_oa::verify_auth_tag(&restarted.auth_tag, &agent.public_key()).unwrap();
        assert_eq!(first_owner, owner.public_key());
        assert_eq!(restarted_owner, owner.public_key());
    }

    #[test]
    fn rejects_wrong_agent_community_relay_action_and_conditions() {
        let (signer, _) = signer();
        let cases = [
            (
                SignerErrorCode::AgentNotAllowed,
                AuthorizeAgentRequest {
                    agent_pubkey: Keys::generate().public_key().to_hex(),
                    ..request(&signer)
                },
            ),
            (
                SignerErrorCode::CommunityNotAllowed,
                AuthorizeAgentRequest {
                    community_config_id: CommunityConfigId::new(),
                    ..request(&signer)
                },
            ),
            (
                SignerErrorCode::RelayNotAllowed,
                AuthorizeAgentRequest {
                    relay_url: Url::parse("wss://other.example.test").unwrap(),
                    ..request(&signer)
                },
            ),
            (
                SignerErrorCode::ActionNotAllowed,
                AuthorizeAgentRequest {
                    action: "sign_event".into(),
                    ..request(&signer)
                },
            ),
            (
                SignerErrorCode::ConditionsNotAllowed,
                AuthorizeAgentRequest {
                    conditions: "kind=1".into(),
                    ..request(&signer)
                },
            ),
        ];
        for (expected, request) in cases {
            assert_eq!(signer.authorize_agent(&request).unwrap_err().code, expected);
        }
    }

    #[test]
    fn ipc_rejects_malformed_and_oversize_frames() {
        let (signer, _) = signer();
        let malformed = decode_signer_response(&signer.handle_frame(&[0, 0, 0, 4, b'{'])).unwrap();
        assert!(matches!(
            malformed,
            SignerIpcResponse::Error(SignerError {
                code: SignerErrorCode::MalformedFrame,
                ..
            })
        ));
        let oversized = (MAX_SIGNER_FRAME_BYTES as u32 + 1).to_be_bytes();
        let response = decode_signer_response(&signer.handle_frame(&oversized)).unwrap();
        assert!(matches!(
            response,
            SignerIpcResponse::Error(SignerError {
                code: SignerErrorCode::FrameTooLarge,
                ..
            })
        ));

        let invalid_json = [0, 0, 0, 1, b'{'];
        let response = decode_signer_response(&signer.handle_frame(&invalid_json)).unwrap();
        assert!(matches!(
            response,
            SignerIpcResponse::Error(SignerError {
                code: SignerErrorCode::InvalidRequest,
                ..
            })
        ));
    }

    #[test]
    fn responses_and_errors_never_contain_owner_secret() {
        let (signer, _) = signer();
        let owner_secret = signer.owner_keys.secret_key().to_secret_hex();
        let success = signer.handle_frame(&encode_authorize_request(&request(&signer)));
        let mut denied = request(&signer);
        denied.action = "arbitrary_sign".into();
        let failure = signer.handle_frame(&encode_authorize_request(&denied));
        assert!(!String::from_utf8_lossy(&success).contains(&owner_secret));
        assert!(!String::from_utf8_lossy(&failure).contains(&owner_secret));
        assert!(
            !serde_json::to_string(&signer.authorize_agent(&denied).unwrap_err())
                .unwrap()
                .contains(&owner_secret)
        );
    }
}
