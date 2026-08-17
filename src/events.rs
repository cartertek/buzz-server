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
    mut shutdown: watch::Receiver<bool>,
    mut output: Output,
) -> Result<(), String>
where
    F: RelayTransportFactory,
    Output: FnMut(Value),
{
    let mut backoff = Duration::from_secs(1);
    let mut since = unix_seconds();
    let mut seen_event_ids = HashSet::new();
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
        if let Err(error) = transport.send(&all_events_request(since)).await {
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
                        if !seen_event_ids.insert(event.id.to_hex()) {
                            continue;
                        }
                        since = since.max(event.created_at.as_secs());
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
    use nostr::{EventBuilder, Kind};
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
}
