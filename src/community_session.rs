//! Deterministic configured-community relay and readiness boundary.
//!
//! Network I/O is deliberately outside this module. A relay adapter reports
//! connection/authentication changes and passes received signed events here.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::CommunityConfigId;

pub const INITIAL_READINESS_TIMEOUT_SECONDS: u64 = 30;
pub const PRESENCE_EXPIRY_SECONDS: u64 = 180;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommunitySessionKey {
    pub community_config_id: CommunityConfigId,
    pub relay_url: Url,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayConnectionState {
    #[default]
    Disconnected,
    Connected,
    Authenticated,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityReadiness {
    Pending,
    Ready,
    Degraded,
}

/// Verifies the configured NIP-OA authorization outside the relay transport.
/// Implementations should use the pinned shared SDK verification path.
pub trait CommunityAuthorizationVerifier {
    type Error;

    fn verify(
        &self,
        community_config_id: CommunityConfigId,
        expected_agent: &buzz_core::PublicKey,
        authorization: &str,
    ) -> Result<(), Self::Error>;
}

/// Durable state for one configured community and expected agent identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommunitySession {
    pub key: CommunitySessionKey,
    pub expected_agent: buzz_core::PublicKey,
    pub connection: RelayConnectionState,
    pub authorization_verified: bool,
    pub readiness_started_at_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_online_presence_at_seconds: Option<u64>,
}

impl CommunitySession {
    pub fn new(
        community_config_id: CommunityConfigId,
        relay_url: Url,
        expected_agent: buzz_core::PublicKey,
        now_seconds: u64,
    ) -> Result<Self, CommunitySessionError> {
        if !matches!(relay_url.scheme(), "ws" | "wss")
            || relay_url.host_str().is_none()
            || !relay_url.username().is_empty()
            || relay_url.password().is_some()
        {
            return Err(CommunitySessionError::InvalidRelayUrl);
        }
        let normalized =
            Url::parse(relay_url.as_str()).map_err(|_| CommunitySessionError::InvalidRelayUrl)?;
        Ok(Self {
            key: CommunitySessionKey {
                community_config_id,
                relay_url: normalized,
            },
            expected_agent,
            connection: RelayConnectionState::Disconnected,
            authorization_verified: false,
            readiness_started_at_seconds: now_seconds,
            last_online_presence_at_seconds: None,
        })
    }

    pub fn verify_authorization<V: CommunityAuthorizationVerifier>(
        &mut self,
        verifier: &V,
        authorization: &str,
    ) -> Result<(), CommunitySessionError> {
        verifier
            .verify(
                self.key.community_config_id,
                &self.expected_agent,
                authorization,
            )
            .map_err(|_| CommunitySessionError::AuthorizationRejected)?;
        self.authorization_verified = true;
        Ok(())
    }

    pub fn connected(&mut self) {
        self.connection = RelayConnectionState::Connected;
    }

    pub fn authenticated(&mut self) -> Result<(), CommunitySessionError> {
        if self.connection != RelayConnectionState::Connected {
            return Err(CommunitySessionError::InvalidConnectionTransition);
        }
        self.connection = RelayConnectionState::Authenticated;
        Ok(())
    }

    pub fn disconnected(&mut self) {
        self.connection = RelayConnectionState::Disconnected;
    }

    /// Verifies and records a canonical signed kind-20001 `online` presence.
    /// Old or future events cannot refresh readiness.
    pub fn observe_presence(
        &mut self,
        event: &buzz_core::Event,
        now_seconds: u64,
    ) -> Result<(), CommunitySessionError> {
        buzz_core::verify_event(event).map_err(|_| CommunitySessionError::InvalidSignature)?;
        if event.pubkey != self.expected_agent {
            return Err(CommunitySessionError::UnexpectedAgent);
        }
        if event.kind.as_u16() as u32 != buzz_core::kind::KIND_PRESENCE_UPDATE {
            return Err(CommunitySessionError::UnexpectedEventKind);
        }
        if event.content != buzz_core::PresenceStatus::Online.as_str() {
            return Err(CommunitySessionError::NotOnlinePresence);
        }
        let event_seconds = event.created_at.as_secs();
        if event_seconds > now_seconds {
            return Err(CommunitySessionError::FuturePresence);
        }
        if now_seconds.saturating_sub(event_seconds) > PRESENCE_EXPIRY_SECONDS {
            return Err(CommunitySessionError::ExpiredPresence);
        }
        if self
            .last_online_presence_at_seconds
            .is_some_and(|previous| event_seconds < previous)
        {
            return Err(CommunitySessionError::OutOfOrderPresence);
        }
        self.last_online_presence_at_seconds = Some(event_seconds);
        Ok(())
    }

    #[must_use]
    pub fn readiness(&self, now_seconds: u64) -> CommunityReadiness {
        let presence_current = self.last_online_presence_at_seconds.is_some_and(|seen| {
            seen <= now_seconds && now_seconds.saturating_sub(seen) <= PRESENCE_EXPIRY_SECONDS
        });
        if self.authorization_verified
            && self.connection == RelayConnectionState::Authenticated
            && presence_current
        {
            return CommunityReadiness::Ready;
        }
        if now_seconds.saturating_sub(self.readiness_started_at_seconds)
            < INITIAL_READINESS_TIMEOUT_SECONDS
        {
            CommunityReadiness::Pending
        } else {
            CommunityReadiness::Degraded
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CommunitySessionError {
    #[error("relay URL must be a credential-free ws or wss URL with a host")]
    InvalidRelayUrl,
    #[error("configured community authorization was rejected")]
    AuthorizationRejected,
    #[error("invalid relay connection state transition")]
    InvalidConnectionTransition,
    #[error("presence event signature or ID is invalid")]
    InvalidSignature,
    #[error("presence event was not signed by the expected agent")]
    UnexpectedAgent,
    #[error("event is not a Buzz presence update")]
    UnexpectedEventKind,
    #[error("presence status is not online")]
    NotOnlinePresence,
    #[error("presence event timestamp is in the future")]
    FuturePresence,
    #[error("presence event has already expired")]
    ExpiredPresence,
    #[error("presence event is older than the last observation")]
    OutOfOrderPresence,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Timestamp};

    struct Allow;

    impl CommunityAuthorizationVerifier for Allow {
        type Error = ();

        fn verify(
            &self,
            _: CommunityConfigId,
            _: &buzz_core::PublicKey,
            authorization: &str,
        ) -> Result<(), Self::Error> {
            (authorization == "verified").then_some(()).ok_or(())
        }
    }

    fn session(keys: &Keys, now: u64) -> CommunitySession {
        CommunitySession::new(
            CommunityConfigId::new(),
            Url::parse("wss://relay.example.com/").unwrap(),
            keys.public_key(),
            now,
        )
        .unwrap()
    }

    fn presence(keys: &Keys, status: &str, at: u64) -> buzz_core::Event {
        EventBuilder::new(
            Kind::Custom(buzz_core::kind::KIND_PRESENCE_UPDATE as u16),
            status,
        )
        .custom_created_at(Timestamp::from(at))
        .tags([])
        .sign_with_keys(keys)
        .unwrap()
    }

    #[test]
    fn readiness_requires_authorization_authentication_and_signed_online_presence() {
        let keys = Keys::generate();
        let mut value = session(&keys, 1_000);
        assert_eq!(value.readiness(1_029), CommunityReadiness::Pending);
        value.verify_authorization(&Allow, "verified").unwrap();
        value.connected();
        value.authenticated().unwrap();
        value
            .observe_presence(&presence(&keys, "online", 1_010), 1_010)
            .unwrap();
        assert_eq!(value.readiness(1_010), CommunityReadiness::Ready);
    }

    #[test]
    fn initial_timeout_and_presence_expiry_degrade_deterministically() {
        let keys = Keys::generate();
        let mut value = session(&keys, 10_000);
        value.verify_authorization(&Allow, "verified").unwrap();
        value.connected();
        value.authenticated().unwrap();
        assert_eq!(value.readiness(10_030), CommunityReadiness::Degraded);
        value
            .observe_presence(&presence(&keys, "online", 10_030), 10_030)
            .unwrap();
        assert_eq!(value.readiness(10_210), CommunityReadiness::Ready);
        assert_eq!(value.readiness(10_211), CommunityReadiness::Degraded);
        value.disconnected();
        assert_eq!(value.readiness(10_100), CommunityReadiness::Degraded);
    }

    #[test]
    fn presence_must_be_current_signed_online_and_from_expected_agent() {
        let keys = Keys::generate();
        let other = Keys::generate();
        let mut value = session(&keys, 1_000);
        assert_eq!(
            value.observe_presence(&presence(&other, "online", 1_000), 1_000),
            Err(CommunitySessionError::UnexpectedAgent)
        );
        assert_eq!(
            value.observe_presence(&presence(&keys, "away", 1_000), 1_000),
            Err(CommunitySessionError::NotOnlinePresence)
        );
        assert_eq!(
            value.observe_presence(&presence(&keys, "online", 1_001), 1_000),
            Err(CommunitySessionError::FuturePresence)
        );
        assert_eq!(
            value.observe_presence(&presence(&keys, "online", 800), 1_000),
            Err(CommunitySessionError::ExpiredPresence)
        );
    }

    #[test]
    fn relay_identity_is_normalized_and_durable() {
        let keys = Keys::generate();
        let value = CommunitySession::new(
            CommunityConfigId::new(),
            Url::parse("WSS://Relay.Example.COM").unwrap(),
            keys.public_key(),
            42,
        )
        .unwrap();
        assert_eq!(value.key.relay_url.as_str(), "wss://relay.example.com/");
        let encoded = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<CommunitySession>(&encoded).unwrap(),
            value
        );
    }
}
