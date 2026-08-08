//! Transport-independent authenticated principals and lifecycle authorization.

use serde::{Deserialize, Serialize};

use buzz_core::{verify_event, Event};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Administrator,
    DraftSubmitter,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum Principal {
    UnixPeer {
        uid: u32,
        gid: u32,
        pid: Option<u32>,
    },
    Nip98 {
        pubkey: String,
    },
}

impl Principal {
    /// Stable ownership identity. PID is excluded because it changes between connections.
    #[must_use]
    pub fn ownership_key(&self) -> PrincipalOwnership {
        match self {
            Self::UnixPeer { uid, .. } => PrincipalOwnership::UnixUid { uid: *uid },
            Self::Nip98 { pubkey } => PrincipalOwnership::NostrPubkey {
                pubkey: pubkey.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrincipalOwnership {
    UnixUid { uid: u32 },
    NostrPubkey { pubkey: String },
}

/// Assigned only after a transport adapter authenticates the principal. This type deliberately
/// does not implement `Deserialize`, so a client cannot self-assert an authority from JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AuthenticatedPrincipal {
    pub principal: Principal,
    pub authority: Authority,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    ReadCommunity,
    ManageCommunity,
    ReadAgent,
    CreateAgent,
    UpdateAgent,
    ChangeAgentState,
    DeleteAgent,
    PurgeAgent,
    SubmitDraft,
    ReadDraft,
    PromoteDraft,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthorizationError {
    #[error("the principal lacks the required capability")]
    Forbidden,
    #[error("the draft belongs to another principal")]
    NotOwner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthenticationError {
    #[error("principal is not configured")]
    UnknownPrincipal,
    #[error("authentication proof is malformed")]
    MalformedProof,
    #[error("authentication signature is invalid")]
    InvalidSignature,
    #[error("authentication proof does not match the request")]
    RequestMismatch,
    #[error("authentication proof is stale")]
    Stale,
    #[error("authentication proof was already used")]
    Replay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnixPeerCredentials {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct UnixAuthorityPolicy {
    pub administrator_uids: Vec<u32>,
    pub draft_submitter_uids: Vec<u32>,
}

impl UnixAuthorityPolicy {
    pub fn authenticate(
        &self,
        credentials: UnixPeerCredentials,
    ) -> Result<AuthenticatedPrincipal, AuthenticationError> {
        let authority = if self.administrator_uids.contains(&credentials.uid) {
            Authority::Administrator
        } else if self.draft_submitter_uids.contains(&credentials.uid) {
            Authority::DraftSubmitter
        } else {
            return Err(AuthenticationError::UnknownPrincipal);
        };
        Ok(AuthenticatedPrincipal {
            principal: Principal::UnixPeer {
                uid: credentials.uid,
                gid: credentials.gid,
                pid: credentials.pid,
            },
            authority,
        })
    }
}

pub trait ReplayGuard {
    /// Atomically returns true only the first time an event ID is observed.
    fn claim(&self, event_id: &str, expires_at: u64) -> bool;
}

#[derive(Clone, Debug, Default)]
pub struct Nip98AuthorityPolicy {
    pub administrator_pubkeys: Vec<String>,
    pub draft_submitter_pubkeys: Vec<String>,
    pub freshness_seconds: u64,
}

impl Nip98AuthorityPolicy {
    /// Verifies one already TLS-protected HTTP authentication event.
    pub fn authenticate<R: ReplayGuard>(
        &self,
        event_json: &str,
        method: &str,
        url: &str,
        payload_hash: Option<&str>,
        now: u64,
        replay: &R,
    ) -> Result<AuthenticatedPrincipal, AuthenticationError> {
        let event: Event =
            serde_json::from_str(event_json).map_err(|_| AuthenticationError::MalformedProof)?;
        verify_event(&event).map_err(|_| AuthenticationError::InvalidSignature)?;
        if event.kind.as_u16() != 27_235 {
            return Err(AuthenticationError::RequestMismatch);
        }
        let created_at = event.created_at.as_secs();
        let freshness = self.freshness_seconds.max(1);
        if created_at.abs_diff(now) > freshness {
            return Err(AuthenticationError::Stale);
        }
        let tag_value = |name: &str| {
            event.tags.iter().find_map(|tag| {
                let values = tag.as_slice();
                (values.first().map(String::as_str) == Some(name))
                    .then(|| values.get(1).cloned())
                    .flatten()
            })
        };
        if tag_value("u").as_deref() != Some(url)
            || tag_value("method").as_deref() != Some(method)
            || payload_hash.is_some_and(|hash| tag_value("payload").as_deref() != Some(hash))
        {
            return Err(AuthenticationError::RequestMismatch);
        }
        let pubkey = event.pubkey.to_hex();
        let authority = if self.administrator_pubkeys.contains(&pubkey) {
            Authority::Administrator
        } else if self.draft_submitter_pubkeys.contains(&pubkey) {
            Authority::DraftSubmitter
        } else {
            return Err(AuthenticationError::UnknownPrincipal);
        };
        let event_id = event.id.to_hex();
        if !replay.claim(&event_id, created_at.saturating_add(freshness)) {
            return Err(AuthenticationError::Replay);
        }
        Ok(AuthenticatedPrincipal {
            principal: Principal::Nip98 { pubkey },
            authority,
        })
    }
}

pub fn authorize(
    actor: &AuthenticatedPrincipal,
    capability: Capability,
    owner: Option<&PrincipalOwnership>,
) -> Result<(), AuthorizationError> {
    if actor.authority == Authority::Administrator {
        return Ok(());
    }

    match capability {
        Capability::SubmitDraft => Ok(()),
        Capability::ReadDraft if owner == Some(&actor.principal.ownership_key()) => Ok(()),
        Capability::ReadDraft => Err(AuthorizationError::NotOwner),
        Capability::ReadCommunity
        | Capability::ManageCommunity
        | Capability::ReadAgent
        | Capability::CreateAgent
        | Capability::UpdateAgent
        | Capability::ChangeAgentState
        | Capability::DeleteAgent
        | Capability::PurgeAgent
        | Capability::PromoteDraft => Err(AuthorizationError::Forbidden),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Mutex};

    use nostr::{EventBuilder, JsonUtil, Keys, Kind, Tag, Timestamp};

    use super::*;

    #[derive(Default)]
    struct MemoryReplay(Mutex<HashSet<String>>);

    impl ReplayGuard for MemoryReplay {
        fn claim(&self, event_id: &str, _expires_at: u64) -> bool {
            self.0.lock().unwrap().insert(event_id.into())
        }
    }

    fn actor(authority: Authority, uid: u32) -> AuthenticatedPrincipal {
        AuthenticatedPrincipal {
            principal: Principal::UnixPeer {
                uid,
                gid: uid,
                pid: Some(99),
            },
            authority,
        }
    }

    #[test]
    fn authority_matrix_is_fixed() {
        let administrator = actor(Authority::Administrator, 1);
        let submitter = actor(Authority::DraftSubmitter, 2);
        let privileged = [
            Capability::ReadCommunity,
            Capability::ManageCommunity,
            Capability::ReadAgent,
            Capability::CreateAgent,
            Capability::UpdateAgent,
            Capability::ChangeAgentState,
            Capability::DeleteAgent,
            Capability::PurgeAgent,
            Capability::PromoteDraft,
        ];
        for capability in privileged {
            assert!(authorize(&administrator, capability, None).is_ok());
            assert_eq!(
                authorize(&submitter, capability, None),
                Err(AuthorizationError::Forbidden)
            );
        }
        assert!(authorize(&submitter, Capability::SubmitDraft, None).is_ok());
    }

    #[test]
    fn submitters_only_read_their_own_drafts() {
        let submitter = actor(Authority::DraftSubmitter, 2);
        let owned = PrincipalOwnership::UnixUid { uid: 2 };
        let other = PrincipalOwnership::UnixUid { uid: 3 };
        assert!(authorize(&submitter, Capability::ReadDraft, Some(&owned)).is_ok());
        assert_eq!(
            authorize(&submitter, Capability::ReadDraft, Some(&other)),
            Err(AuthorizationError::NotOwner)
        );
    }

    #[test]
    fn principal_wire_shapes_never_carry_credentials() {
        let unix = actor(Authority::Administrator, 1000);
        let remote = AuthenticatedPrincipal {
            principal: Principal::Nip98 {
                pubkey: "ab".repeat(32),
            },
            authority: Authority::DraftSubmitter,
        };
        let encoded = serde_json::to_string(&(unix, remote)).unwrap();
        assert!(!encoded.contains("authorization"));
        assert!(!encoded.contains("signature"));
        assert!(!encoded.contains("secret"));
    }

    #[test]
    fn unix_peer_credentials_map_only_configured_uids() {
        let policy = UnixAuthorityPolicy {
            administrator_uids: vec![1000],
            draft_submitter_uids: vec![1001],
        };
        let administrator = policy
            .authenticate(UnixPeerCredentials {
                uid: 1000,
                gid: 1000,
                pid: Some(42),
            })
            .unwrap();
        assert_eq!(administrator.authority, Authority::Administrator);
        assert_eq!(
            policy.authenticate(UnixPeerCredentials {
                uid: 2000,
                gid: 2000,
                pid: None,
            }),
            Err(AuthenticationError::UnknownPrincipal)
        );
    }

    #[test]
    fn nip98_verifies_signature_scope_freshness_authority_and_replay() {
        let keys = Keys::generate();
        let now = 1_800_000_000;
        let url = "https://server.example.test/v1/agents";
        let event = EventBuilder::new(Kind::Custom(27_235), "")
            .tags([
                Tag::parse(["u", url]).unwrap(),
                Tag::parse(["method", "POST"]).unwrap(),
                Tag::parse(["payload", "sha256:body"]).unwrap(),
            ])
            .custom_created_at(Timestamp::from(now))
            .sign_with_keys(&keys)
            .unwrap();
        let policy = Nip98AuthorityPolicy {
            administrator_pubkeys: vec![keys.public_key().to_hex()],
            draft_submitter_pubkeys: Vec::new(),
            freshness_seconds: 60,
        };
        let replay = MemoryReplay::default();
        let principal = policy
            .authenticate(
                &event.as_json(),
                "POST",
                url,
                Some("sha256:body"),
                now,
                &replay,
            )
            .unwrap();
        assert_eq!(principal.authority, Authority::Administrator);
        assert_eq!(
            policy.authenticate(
                &event.as_json(),
                "POST",
                url,
                Some("sha256:body"),
                now,
                &replay,
            ),
            Err(AuthenticationError::Replay)
        );
        let fresh_replay = MemoryReplay::default();
        assert_eq!(
            policy.authenticate(
                &event.as_json(),
                "GET",
                url,
                Some("sha256:body"),
                now,
                &fresh_replay,
            ),
            Err(AuthenticationError::RequestMismatch)
        );
        assert_eq!(
            policy.authenticate(
                &event.as_json(),
                "POST",
                url,
                Some("sha256:body"),
                now + 61,
                &fresh_replay,
            ),
            Err(AuthenticationError::Stale)
        );
    }
}
