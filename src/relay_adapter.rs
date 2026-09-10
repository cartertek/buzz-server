//! Live `buzz-ws-client` adapter for configured-community readiness sessions.

use std::{
    fs::OpenOptions,
    future::Future,
    io::{self, Write},
    path::PathBuf,
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use buzz_ws_client::{NostrWsConnection, RelayMessage};
use nostr::{Filter, Keys, Kind, Tag, Timestamp};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::community_session::{
    CommunityReadiness, CommunitySession, CommunitySessionError, PRESENCE_EXPIRY_SECONDS,
};

pub type RelayFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Narrow transport seam implemented by the pinned Buzz WebSocket client.
pub trait RelayTransport: Send {
    fn send<'a>(&'a mut self, value: &'a Value) -> RelayFuture<'a, Result<(), String>>;
    fn next<'a>(&'a mut self) -> RelayFuture<'a, Result<RelayMessage, String>>;
    fn close(self: Box<Self>) -> RelayFuture<'static, Result<(), String>>;
}

pub trait RelayTransportFactory: Send + Sync {
    fn connect<'a>(
        &'a self,
        relay_url: &'a str,
    ) -> RelayFuture<'a, Result<Box<dyn RelayTransport>, String>>;
}

pub struct BuzzWsTransport(NostrWsConnection);

impl RelayTransport for BuzzWsTransport {
    fn send<'a>(&'a mut self, value: &'a Value) -> RelayFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0
                .send_raw(value)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn next<'a>(&'a mut self) -> RelayFuture<'a, Result<RelayMessage, String>> {
        Box::pin(async move {
            self.0
                .next_event(Duration::from_secs(24 * 60 * 60))
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn close(self: Box<Self>) -> RelayFuture<'static, Result<(), String>> {
        Box::pin(async move { self.0.disconnect().await.map_err(|error| error.to_string()) })
    }
}

/// Credentials used only for NIP-42 connection authentication.
pub struct BuzzWsFactory {
    pub keys: Keys,
    pub authorization_tag: Option<Tag>,
}

impl RelayTransportFactory for BuzzWsFactory {
    fn connect<'a>(
        &'a self,
        relay_url: &'a str,
    ) -> RelayFuture<'a, Result<Box<dyn RelayTransport>, String>> {
        Box::pin(async move {
            NostrWsConnection::connect_authenticated(
                relay_url,
                &self.keys,
                self.authorization_tag.as_ref(),
            )
            .await
            .map(|connection| Box::new(BuzzWsTransport(connection)) as Box<dyn RelayTransport>)
            .map_err(|error| error.to_string())
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelayAdapterConfig {
    pub tick_interval: Duration,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RelayAdapterConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(1),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

pub trait RelayAdapterObserver {
    fn readiness_changed(&mut self, readiness: CommunityReadiness);
    fn transport_error(&mut self, message: &str);
    fn transport_state(&mut self, state: RelayAdapterState);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum RelayAdapterState {
    Connecting,
    Connected,
    Authenticated,
    PresenceSubscriptionSent,
    ReplayComplete,
    Closed,
    Disconnected,
    Backoff,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct RelayStateRecord {
    pub occurred_at_unix_ms: u64,
    pub state: RelayAdapterState,
}

/// Credential-free operator evidence for the community transport lifecycle.
/// Each record is append-only JSONL so operators can retrieve state after a
/// failed connection without exposing relay credentials or event payloads.
pub struct RelayStateJournal {
    path: PathBuf,
}

impl RelayStateJournal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn records(&self) -> io::Result<Vec<RelayStateRecord>> {
        let payload = match std::fs::read_to_string(&self.path) {
            Ok(payload) => payload,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        payload
            .lines()
            .map(|line| serde_json::from_str(line).map_err(io::Error::other))
            .collect()
    }

    fn append(&self, state: RelayAdapterState) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let record = RelayStateRecord {
            occurred_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            state,
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, &record).map_err(io::Error::other)?;
        file.write_all(b"\n")
    }
}

impl RelayAdapterObserver for RelayStateJournal {
    fn readiness_changed(&mut self, _readiness: CommunityReadiness) {}

    fn transport_error(&mut self, _message: &str) {
        // Error text may contain credentials or relay payloads; state transitions
        // are the supported, credential-free diagnostic boundary.
    }

    fn transport_state(&mut self, state: RelayAdapterState) {
        let _ = self.append(state);
    }
}

pub trait RelayClock: Send + Sync {
    fn now_seconds(&self) -> u64;
}

pub struct SystemRelayClock;

impl RelayClock for SystemRelayClock {
    fn now_seconds(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

pub struct CommunityRelayAdapter<F, C> {
    pub factory: F,
    pub clock: C,
    pub config: RelayAdapterConfig,
}

impl<F: RelayTransportFactory, C: RelayClock> CommunityRelayAdapter<F, C> {
    /// Runs until shutdown, reconnecting with capped exponential backoff.
    pub async fn run<O: RelayAdapterObserver>(
        &self,
        session: &mut CommunitySession,
        observer: &mut O,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), RelayAdapterError> {
        self.config.validate()?;
        if !session.authorization_verified {
            return Err(RelayAdapterError::AuthorizationNotVerified);
        }
        let mut backoff = self.config.initial_backoff;
        while !*shutdown.borrow() {
            observer.transport_state(RelayAdapterState::Connecting);
            let connection = tokio::select! {
                connection = self.factory.connect(session.key.relay_url.as_str()) => connection,
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        session.disconnected();
                        return Ok(());
                    }
                    continue;
                }
            };
            let mut transport = match connection {
                Ok(transport) => transport,
                Err(error) => {
                    session.disconnected();
                    observer.transport_state(RelayAdapterState::Disconnected);
                    observer.transport_error(&error);
                    observer.transport_state(RelayAdapterState::Backoff);
                    if wait_or_shutdown(backoff, &mut shutdown).await {
                        return Ok(());
                    }
                    backoff = doubled_capped(backoff, self.config.max_backoff);
                    continue;
                }
            };
            observer.transport_state(RelayAdapterState::Connected);
            session.connected();
            session.authenticated()?;
            observer.transport_state(RelayAdapterState::Authenticated);
            let request = presence_subscription(session, self.clock.now_seconds());
            if let Err(error) = transport.send(&request).await {
                observer.transport_error(&error);
                session.disconnected();
                observer.transport_state(RelayAdapterState::Disconnected);
                let _ = transport.close().await;
                observer.transport_state(RelayAdapterState::Backoff);
                if wait_or_shutdown(backoff, &mut shutdown).await {
                    return Ok(());
                }
                backoff = doubled_capped(backoff, self.config.max_backoff);
                continue;
            }
            observer.transport_state(RelayAdapterState::PresenceSubscriptionSent);
            backoff = self.config.initial_backoff;

            loop {
                observer.readiness_changed(session.readiness(self.clock.now_seconds()));
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            let _ = transport.close().await;
                            session.disconnected();
                            observer.transport_state(RelayAdapterState::Closed);
                            return Ok(());
                        }
                    }
                        message = transport.next() => {
                            if matches!(&message, Ok(RelayMessage::Eose { .. })) {
                                observer.transport_state(RelayAdapterState::ReplayComplete);
                            }
                            match message {
                            Ok(message) => drive_relay_message(session, message, self.clock.now_seconds(), observer),
                            Err(error) => {
                                observer.transport_error(&error);
                                session.disconnected();
                                observer.transport_state(RelayAdapterState::Disconnected);
                                let _ = transport.close().await;
                                observer.transport_state(RelayAdapterState::Backoff);
                                break;
                            }
                        }
                    }
                    () = tokio::time::sleep(self.config.tick_interval) => {}
                }
            }
        }
        Ok(())
    }
}

fn drive_relay_message<O: RelayAdapterObserver>(
    session: &mut CommunitySession,
    message: RelayMessage,
    now_seconds: u64,
    observer: &mut O,
) {
    if let RelayMessage::Event { event, .. } = message {
        if let Err(error) = session.observe_presence(&event, now_seconds) {
            observer.transport_error(&error.to_string());
        }
    }
    observer.readiness_changed(session.readiness(now_seconds));
}

pub fn presence_subscription(session: &CommunitySession, now_seconds: u64) -> Value {
    let since = now_seconds.saturating_sub(PRESENCE_EXPIRY_SECONDS);
    let filter = Filter::new()
        .author(session.expected_agent)
        .kind(Kind::Custom(buzz_core::kind::KIND_PRESENCE_UPDATE as u16))
        .since(Timestamp::from(since))
        .limit(1);
    json!(["REQ", subscription_id(session), filter])
}

fn subscription_id(session: &CommunitySession) -> String {
    format!("community-presence-{}", session.key.community_config_id)
}

async fn wait_or_shutdown(duration: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(duration) => false,
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
    }
}

fn doubled_capped(current: Duration, cap: Duration) -> Duration {
    current.saturating_mul(2).min(cap)
}

impl RelayAdapterConfig {
    fn validate(self) -> Result<(), RelayAdapterError> {
        if self.tick_interval.is_zero()
            || self.initial_backoff.is_zero()
            || self.max_backoff < self.initial_backoff
        {
            return Err(RelayAdapterError::InvalidConfig);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RelayAdapterError {
    #[error("relay adapter intervals must be non-zero and max backoff must not be smaller than initial backoff")]
    InvalidConfig,
    #[error("community authorization must be verified before relay connection")]
    AuthorizationNotVerified,
    #[error(transparent)]
    Session(#[from] CommunitySessionError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommunityConfigId;
    use nostr::EventBuilder;
    use std::collections::VecDeque;
    use url::Url;

    struct FixedClock(u64);
    impl RelayClock for FixedClock {
        fn now_seconds(&self) -> u64 {
            self.0
        }
    }

    #[derive(Default)]
    struct Observer {
        readiness: Vec<CommunityReadiness>,
        errors: Vec<String>,
        states: Vec<RelayAdapterState>,
    }
    impl RelayAdapterObserver for Observer {
        fn readiness_changed(&mut self, readiness: CommunityReadiness) {
            self.readiness.push(readiness);
        }
        fn transport_error(&mut self, message: &str) {
            self.errors.push(message.to_owned());
        }
        fn transport_state(&mut self, state: RelayAdapterState) {
            self.states.push(state);
        }
    }

    struct FakeTransport {
        messages: VecDeque<Result<RelayMessage, String>>,
    }

    struct NeverFactory;
    impl RelayTransportFactory for NeverFactory {
        fn connect<'a>(
            &'a self,
            _: &'a str,
        ) -> RelayFuture<'a, Result<Box<dyn RelayTransport>, String>> {
            Box::pin(async { Err("must not connect".into()) })
        }
    }
    impl RelayTransport for FakeTransport {
        fn send<'a>(&'a mut self, _: &'a Value) -> RelayFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn next<'a>(&'a mut self) -> RelayFuture<'a, Result<RelayMessage, String>> {
            Box::pin(async move {
                self.messages
                    .pop_front()
                    .unwrap_or_else(|| Err("closed".into()))
            })
        }
        fn close(self: Box<Self>) -> RelayFuture<'static, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn relay_state_journal_persists_and_retrieves_credential_free_states() {
        let directory = tempfile::tempdir().unwrap();
        let mut journal = RelayStateJournal::new(directory.path().join("relay-state.jsonl"));
        journal.transport_state(RelayAdapterState::Connecting);
        journal.transport_state(RelayAdapterState::Backoff);
        let records = journal.records().unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.state)
                .collect::<Vec<_>>(),
            vec![RelayAdapterState::Connecting, RelayAdapterState::Backoff]
        );
    }

    fn session(keys: &Keys) -> CommunitySession {
        let mut session = CommunitySession::new(
            CommunityConfigId::new(),
            Url::parse("wss://relay.example/").unwrap(),
            keys.public_key(),
            1_000,
        )
        .unwrap();
        session.authorization_verified = true;
        session.connected();
        session.authenticated().unwrap();
        session
    }

    #[test]
    fn subscription_uses_canonical_bounded_presence_filter() {
        let keys = Keys::generate();
        let session = session(&keys);
        let request = presence_subscription(&session, 1_000);
        assert_eq!(request[0], "REQ");
        assert_eq!(request[2]["authors"][0], keys.public_key().to_hex());
        assert_eq!(
            request[2]["kinds"][0],
            buzz_core::kind::KIND_PRESENCE_UPDATE
        );
        assert_eq!(request[2]["since"], 820);
        assert_eq!(request[2]["limit"], 1);
    }

    #[tokio::test]
    async fn fake_transport_message_drives_signed_presence_readiness() {
        let keys = Keys::generate();
        let mut session = session(&keys);
        let event = EventBuilder::new(
            Kind::Custom(buzz_core::kind::KIND_PRESENCE_UPDATE as u16),
            "online",
        )
        .custom_created_at(Timestamp::from(1_000))
        .tags([])
        .sign_with_keys(&keys)
        .unwrap();
        let mut fake = FakeTransport {
            messages: VecDeque::from([Ok(RelayMessage::Event {
                subscription_id: "test".into(),
                event: Box::new(event),
            })]),
        };
        let message = fake.next().await.unwrap();
        let mut observer = Observer::default();
        drive_relay_message(
            &mut session,
            message,
            FixedClock(1_000).now_seconds(),
            &mut observer,
        );
        assert_eq!(observer.readiness.last(), Some(&CommunityReadiness::Ready));
        assert!(observer.errors.is_empty());
    }

    #[tokio::test]
    async fn adapter_refuses_connection_before_authorization_verification() {
        let keys = Keys::generate();
        let mut session = CommunitySession::new(
            CommunityConfigId::new(),
            Url::parse("wss://relay.example/").unwrap(),
            keys.public_key(),
            1_000,
        )
        .unwrap();
        let adapter = CommunityRelayAdapter {
            factory: NeverFactory,
            clock: FixedClock(1_000),
            config: RelayAdapterConfig::default(),
        };
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut observer = Observer::default();
        assert_eq!(
            adapter.run(&mut session, &mut observer, shutdown_rx).await,
            Err(RelayAdapterError::AuthorizationNotVerified)
        );
    }

    #[tokio::test]
    async fn adapter_run_persists_transport_states_for_supported_retrieval() {
        let keys = Keys::generate();
        let mut session = session(&keys);
        let directory = tempfile::tempdir().unwrap();
        let journal_path = directory.path().join("relay-state.jsonl");
        let mut journal = RelayStateJournal::new(&journal_path);
        let adapter = CommunityRelayAdapter {
            factory: NeverFactory,
            clock: FixedClock(1_000),
            config: RelayAdapterConfig {
                tick_interval: Duration::from_millis(1),
                initial_backoff: Duration::from_millis(1),
                max_backoff: Duration::from_millis(1),
            },
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task =
            tokio::spawn(async move { adapter.run(&mut session, &mut journal, shutdown_rx).await });
        tokio::time::sleep(Duration::from_millis(5)).await;
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
        let records = RelayStateJournal::new(journal_path).records().unwrap();
        assert!(records
            .iter()
            .any(|record| record.state == RelayAdapterState::Connecting));
        assert!(records
            .iter()
            .any(|record| record.state == RelayAdapterState::Disconnected));
        assert!(records
            .iter()
            .any(|record| record.state == RelayAdapterState::Backoff));
    }

    #[test]
    fn reconnect_backoff_is_bounded() {
        assert_eq!(
            doubled_capped(Duration::from_secs(2), Duration::from_secs(30)),
            Duration::from_secs(4)
        );
        assert_eq!(
            doubled_capped(Duration::from_secs(20), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }
}
