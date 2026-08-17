//! Authenticated, non-persistent relay event streaming for the operator CLI.

use std::{
    collections::HashSet,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use buzz_ws_client::RelayMessage;
use nostr::{Keys, Tag};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::relay_adapter::RelayTransportFactory;

pub const SUBSCRIPTION_ID: &str = "buzz-server-events";

#[derive(Default)]
struct EventCursor {
    since: u64,
    seen_at_cursor: HashSet<String>,
}

impl EventCursor {
    fn new(since: u64) -> Self {
        Self {
            since,
            ..Self::default()
        }
    }

    fn accept(&mut self, event: &nostr::Event) -> bool {
        let timestamp = event.created_at.as_secs();
        if timestamp < self.since {
            return true;
        }
        if timestamp > self.since {
            self.since = timestamp;
            self.seen_at_cursor.clear();
        }
        self.seen_at_cursor.insert(event.id.to_hex())
    }
}

pub fn all_events_request(since: u64) -> Value {
    json!(["REQ", SUBSCRIPTION_ID, {"since": since}])
}

pub fn relay_message_json(message: &RelayMessage) -> Value {
    match message {
        RelayMessage::Event {
            subscription_id,
            event,
        } => json!({"type":"event", "subscription_id":subscription_id, "event":event}),
        RelayMessage::Eose { subscription_id } => {
            json!({"type":"eose", "subscription_id":subscription_id})
        }
        RelayMessage::Closed {
            subscription_id,
            message,
        } => json!({"type":"closed", "subscription_id":subscription_id, "message":message}),
        RelayMessage::Notice { message } => json!({"type":"notice", "message":message}),
        RelayMessage::Auth { challenge } => json!({"type":"auth", "challenge":challenge}),
        RelayMessage::Ok(ok) => {
            json!({"type":"ok", "event_id":ok.event_id, "accepted":ok.accepted, "message":ok.message})
        }
        RelayMessage::Count {
            subscription_id,
            count,
        } => json!({"type":"count", "subscription_id":subscription_id, "count":count}),
    }
}

pub fn error_json(message: impl Into<String>) -> Value {
    json!({"type":"error", "message":message.into()})
}

pub async fn run<F, Output>(
    factory: &F,
    relay_url: &str,
    shutdown: watch::Receiver<bool>,
    output: Output,
) -> Result<(), String>
where
    F: RelayTransportFactory,
    Output: FnMut(Value),
{
    run_with_initial_since(factory, relay_url, shutdown, output, unix_seconds()).await
}

async fn run_with_initial_since<F, Output>(
    factory: &F,
    relay_url: &str,
    mut shutdown: watch::Receiver<bool>,
    mut output: Output,
    initial_since: u64,
) -> Result<(), String>
where
    F: RelayTransportFactory,
    Output: FnMut(Value),
{
    let mut backoff = Duration::from_secs(1);
    let mut cursor = EventCursor::new(initial_since);
    while !*shutdown.borrow() {
        let connection = tokio::select! {
            result = factory.connect(relay_url) => result,
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { return Ok(()); } continue; }
        };
        let mut transport = match connection {
            Ok(value) => value,
            Err(error) => {
                output(error_json(&error));
                if wait_or_shutdown(backoff, &mut shutdown).await {
                    return Ok(());
                }
                backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
                continue;
            }
        };
        if let Err(error) = transport.send(&all_events_request(cursor.since)).await {
            output(error_json(&error));
            let _ = transport.close().await;
            if wait_or_shutdown(backoff, &mut shutdown).await {
                return Ok(());
            }
            backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
            continue;
        }
        backoff = Duration::from_secs(1);
        loop {
            let message = tokio::select! {
                changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { let _ = transport.close().await; return Ok(()); } continue; }
                result = transport.next() => result,
            };
            match message {
                Ok(message) => {
                    if let RelayMessage::Event { event, .. } = &message {
                        if !cursor.accept(event) {
                            continue;
                        }
                    }
                    let closed = matches!(message, RelayMessage::Closed { .. });
                    output(relay_message_json(&message));
                    if closed {
                        let _ = transport.close().await;
                        break;
                    }
                }
                Err(error) => {
                    output(error_json(&error));
                    let _ = transport.close().await;
                    break;
                }
            }
        }
        if wait_or_shutdown(backoff, &mut shutdown).await {
            return Ok(());
        }
        backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
    }
    Ok(())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn wait_or_shutdown(duration: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! { () = tokio::time::sleep(duration) => false, changed = shutdown.changed() => changed.is_err() || *shutdown.borrow() }
}

pub fn parse_keys(value: &str) -> Result<Keys, String> {
    Keys::parse(value).map_err(|error| format!("invalid BUZZ_PRIVATE_KEY: {error}"))
}

pub fn parse_auth_tag(value: Option<&str>) -> Result<Option<Tag>, String> {
    value
        .map(buzz_sdk::nip_oa::parse_auth_tag)
        .transpose()
        .map_err(|error| format!("invalid BUZZ_AUTH_TAG: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay_adapter::{RelayFuture, RelayTransport, RelayTransportFactory};
    use buzz_ws_client::RelayMessage;
    use nostr::{EventBuilder, Kind, Timestamp};
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };
    #[test]
    fn request_is_live_only_but_unrestricted() {
        assert_eq!(
            all_events_request(1_700_000_000),
            json!(["REQ", SUBSCRIPTION_ID, {"since": 1_700_000_000}])
        );
    }
    #[test]
    fn event_and_eose_are_structured() {
        let event = EventBuilder::new(Kind::Custom(1), "live")
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert_eq!(
            relay_message_json(&RelayMessage::Event {
                subscription_id: SUBSCRIPTION_ID.into(),
                event: Box::new(event)
            })["type"],
            "event"
        );
        assert_eq!(
            relay_message_json(&RelayMessage::Eose {
                subscription_id: SUBSCRIPTION_ID.into()
            })["type"],
            "eose"
        );
    }

    struct FakeTransport {
        sent: Arc<Mutex<Vec<Value>>>,
        messages: VecDeque<Result<RelayMessage, String>>,
    }

    impl RelayTransport for FakeTransport {
        fn send<'a>(&'a mut self, value: &'a Value) -> RelayFuture<'a, Result<(), String>> {
            self.sent.lock().unwrap().push(value.clone());
            Box::pin(async { Ok(()) })
        }
        fn next<'a>(&'a mut self) -> RelayFuture<'a, Result<RelayMessage, String>> {
            Box::pin(async {
                self.messages
                    .pop_front()
                    .unwrap_or_else(|| Err("disconnect".into()))
            })
        }
        fn close(self: Box<Self>) -> RelayFuture<'static, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct FakeFactory {
        sent: Arc<Mutex<Vec<Value>>>,
        connections: Mutex<VecDeque<Vec<Result<RelayMessage, String>>>>,
    }

    impl RelayTransportFactory for FakeFactory {
        fn connect<'a>(
            &'a self,
            _: &'a str,
        ) -> RelayFuture<'a, Result<Box<dyn RelayTransport>, String>> {
            let messages = self.connections.lock().unwrap().pop_front();
            let sent = self.sent.clone();
            Box::pin(async move {
                messages
                    .map(|messages| {
                        Box::new(FakeTransport {
                            sent,
                            messages: messages.into(),
                        }) as Box<dyn RelayTransport>
                    })
                    .ok_or_else(|| "no connection".into())
            })
        }
    }

    #[tokio::test]
    async fn reconnect_reuses_cursor_and_bounds_inclusive_deduplication() {
        let keys = Keys::generate();
        let first = EventBuilder::new(Kind::Custom(1), "first")
            .custom_created_at(Timestamp::from(100))
            .sign_with_keys(&keys)
            .unwrap();
        let second = EventBuilder::new(Kind::Custom(1), "second")
            .custom_created_at(Timestamp::from(100))
            .sign_with_keys(&keys)
            .unwrap();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let factory = FakeFactory {
            sent: sent.clone(),
            connections: Mutex::new(VecDeque::from([
                vec![
                    Ok(RelayMessage::Event {
                        subscription_id: SUBSCRIPTION_ID.into(),
                        event: Box::new(first.clone()),
                    }),
                    Ok(RelayMessage::Eose {
                        subscription_id: SUBSCRIPTION_ID.into(),
                    }),
                    Err("disconnect".into()),
                ],
                vec![
                    Ok(RelayMessage::Event {
                        subscription_id: SUBSCRIPTION_ID.into(),
                        event: Box::new(first.clone()),
                    }),
                    Ok(RelayMessage::Event {
                        subscription_id: SUBSCRIPTION_ID.into(),
                        event: Box::new(second.clone()),
                    }),
                ],
            ])),
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let output = Arc::new(Mutex::new(Vec::new()));
        let output_copy = output.clone();
        run_with_initial_since(
            &factory,
            "wss://relay.example",
            shutdown_rx,
            move |value| {
                let stop = value["type"] == "event" && value["event"]["id"] == second.id.to_hex();
                output_copy.lock().unwrap().push(value);
                if stop {
                    let _ = shutdown_tx.send(true);
                }
            },
            100,
        )
        .await
        .unwrap();
        let requests = sent.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0][2]["since"], 100);
        assert_eq!(requests[1][2]["since"], 100);
        let output = output.lock().unwrap();
        let events: Vec<_> = output
            .iter()
            .filter(|value| value["type"] == "event")
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event"]["id"], first.id.to_hex());
        assert_eq!(events[1]["event"]["id"], second.id.to_hex());
    }
}
