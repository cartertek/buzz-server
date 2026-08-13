//! Event-driven open-channel auto-join support for managed agents.

use std::time::Duration;

use buzz_ws_client::{NostrWsConnection, RelayMessage};
use nostr::{Keys, Kind};
use serde_json::{json, Value};
use tokio::sync::watch;
use url::Url;
use uuid::Uuid;

const CREATE_GROUP_KIND: u16 = 9007;
const MEMBER_ADDED_NOTIFICATION_KIND: u16 = 44_100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenChannel {
    pub id: Uuid,
    pub created_at: u64,
}

pub fn open_channel(event: &nostr::Event) -> Option<OpenChannel> {
    if event.kind != Kind::Custom(CREATE_GROUP_KIND) {
        return None;
    }
    let mut channel = None;
    let mut open = false;
    for tag in event.tags.iter() {
        let values = tag.as_slice();
        match values.first().map(String::as_str) {
            Some("h") => {
                channel = values.get(1).and_then(|value| value.parse::<Uuid>().ok());
            }
            Some("visibility") => {
                open = values.get(1).is_some_and(|value| value == "open");
            }
            _ => {}
        }
    }
    open.then_some(OpenChannel {
        id: channel?,
        created_at: event.created_at.as_secs(),
    })
}

pub fn channel_notification(event: &nostr::Event, owner_pubkey: &str) -> Option<Uuid> {
    if event.kind != Kind::Custom(MEMBER_ADDED_NOTIFICATION_KIND) {
        return None;
    }
    let content: Value = serde_json::from_str(&event.content).ok()?;
    if content.get("type").and_then(Value::as_str) != Some("member_added") {
        return None;
    }
    let mut channel = None;
    let mut addressed_to_owner = false;
    for tag in event.tags.iter() {
        let values = tag.as_slice();
        match values.first().map(String::as_str) {
            Some("h") => {
                channel = values.get(1).and_then(|value| value.parse::<Uuid>().ok());
            }
            Some("p") => {
                addressed_to_owner |= values.get(1).is_some_and(|value| value == owner_pubkey);
            }
            _ => {}
        }
    }
    addressed_to_owner.then_some(channel?)
}

pub fn channel_notification_subscription(subscription_id: &str, owner_pubkey: &str) -> Value {
    json!(["REQ", subscription_id, {
        "kinds": [MEMBER_ADDED_NOTIFICATION_KIND],
        "#p": [owner_pubkey],
    }])
}

pub fn channel_metadata_subscription(subscription_id: &str, channel_id: Uuid) -> Value {
    json!(["REQ", subscription_id, {
        "kinds": [CREATE_GROUP_KIND],
        "#h": [channel_id.to_string()],
        "limit": 1,
    }])
}

pub async fn fetch_open_channel(
    relay_url: &Url,
    keys: &Keys,
    channel_id: Uuid,
) -> Result<Option<OpenChannel>, String> {
    let mut connection = NostrWsConnection::connect_authenticated(relay_url.as_str(), keys, None)
        .await
        .map_err(|error| error.to_string())?;
    let subscription_id = format!("server-auto-join-channel-{channel_id}");
    let request = channel_metadata_subscription(&subscription_id, channel_id);
    connection
        .send_raw(&request)
        .await
        .map_err(|error| error.to_string())?;
    loop {
        match connection
            .next_event(Duration::from_secs(30))
            .await
            .map_err(|error| error.to_string())?
        {
            RelayMessage::Event {
                subscription_id: event_subscription,
                event,
            } if event_subscription == subscription_id => {
                if let Some(channel) = open_channel(&event) {
                    if channel.id != channel_id {
                        continue;
                    }
                    let _ = connection.disconnect().await;
                    return Ok(Some(channel));
                }
            }
            RelayMessage::Eose {
                subscription_id: event_subscription,
            } if event_subscription == subscription_id => {
                let _ = connection.disconnect().await;
                return Ok(None);
            }
            RelayMessage::Closed {
                subscription_id: event_subscription,
                message,
            } if event_subscription == subscription_id => return Err(message),
            _ => {}
        }
    }
}

pub async fn publish_join(
    relay_url: &Url,
    keys: &Keys,
    auth_tag_json: &str,
    channel_id: Uuid,
) -> Result<(), String> {
    let auth_tag = buzz_sdk::nip_oa::parse_auth_tag(auth_tag_json).map_err(|e| e.to_string())?;
    let event = buzz_sdk::build_join(channel_id)
        .map_err(|e| e.to_string())?
        .tags([auth_tag.clone()])
        .sign_with_keys(keys)
        .map_err(|e| e.to_string())?;
    let result =
        buzz_ws_client::publish_event(relay_url.as_str(), event, keys, Some(&auth_tag), 75)
            .await
            .map_err(|e| e.to_string())?;
    if result.accepted {
        Ok(())
    } else {
        Err(result.message)
    }
}

pub async fn next_channel_notification(
    connection: &mut NostrWsConnection,
    shutdown: &mut watch::Receiver<bool>,
    owner_pubkey: &str,
) -> Result<Option<Uuid>, String> {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(None);
                }
            }
            message = connection.next_event(Duration::from_secs(24 * 60 * 60)) => {
                if let RelayMessage::Event { event, .. } = message.map_err(|e| e.to_string())? {
                    if let Some(channel) = channel_notification(&event, owner_pubkey) {
                        return Ok(Some(channel));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Tag};

    fn create(visibility: &str) -> nostr::Event {
        let channel = Uuid::now_v7();
        let channel_text = channel.to_string();
        EventBuilder::new(Kind::Custom(CREATE_GROUP_KIND), "")
            .tags([
                Tag::parse(["h", channel_text.as_str()]).unwrap(),
                Tag::parse(["visibility", visibility]).unwrap(),
            ])
            .sign_with_keys(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn recognizes_only_open_channel_creation() {
        assert!(open_channel(&create("open")).is_some());
        assert!(open_channel(&create("private")).is_none());
        let other = EventBuilder::new(Kind::Custom(9), "")
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(open_channel(&other).is_none());
    }

    fn notification(
        owner_pubkey: &str,
        channel: Uuid,
        kind: u16,
        notification: &str,
    ) -> nostr::Event {
        let channel_text = channel.to_string();
        EventBuilder::new(
            Kind::Custom(kind),
            format!(r#"{{"type":"{notification}"}}"#),
        )
        .tags([
            Tag::parse(["h", channel_text.as_str()]).unwrap(),
            Tag::parse(["p", owner_pubkey]).unwrap(),
        ])
        .sign_with_keys(&Keys::generate())
        .unwrap()
    }

    #[test]
    fn recognizes_only_owner_member_added_notifications() {
        let owner = Keys::generate().public_key().to_hex();
        let channel = Uuid::now_v7();
        assert_eq!(
            channel_notification(
                &notification(
                    &owner,
                    channel,
                    MEMBER_ADDED_NOTIFICATION_KIND,
                    "member_added"
                ),
                &owner,
            ),
            Some(channel)
        );
        assert!(channel_notification(
            &notification(
                &owner,
                channel,
                MEMBER_ADDED_NOTIFICATION_KIND,
                "member_removed"
            ),
            &owner,
        )
        .is_none());
        assert!(channel_notification(
            &notification(
                &owner,
                channel,
                MEMBER_ADDED_NOTIFICATION_KIND,
                "member_added"
            ),
            &Keys::generate().public_key().to_hex(),
        )
        .is_none());
        assert!(channel_notification(
            &notification(&owner, channel, CREATE_GROUP_KIND, "member_added"),
            &owner,
        )
        .is_none());
    }

    #[test]
    fn subscription_uses_owner_scoped_global_notification_kind() {
        let owner = Keys::generate().public_key().to_hex();
        let value = channel_notification_subscription("autojoin", &owner);
        assert_eq!(value[0], "REQ");
        assert_eq!(value[1], "autojoin");
        assert_eq!(value[2]["kinds"][0], MEMBER_ADDED_NOTIFICATION_KIND);
        assert_eq!(value[2]["#p"][0], owner);
    }

    #[test]
    fn metadata_lookup_is_scoped_to_the_notified_channel() {
        let channel = Uuid::now_v7();
        let value = channel_metadata_subscription("metadata", channel);
        assert_eq!(value[0], "REQ");
        assert_eq!(value[1], "metadata");
        assert_eq!(value[2]["kinds"][0], CREATE_GROUP_KIND);
        assert_eq!(value[2]["#h"][0], channel.to_string());
        assert_eq!(value[2]["limit"], 1);
    }
}
