//! Event-driven open-channel auto-join support for managed agents.

use std::time::Duration;

use buzz_ws_client::{NostrWsConnection, RelayMessage};
use nostr::{Filter, Keys, Kind};
use serde_json::{json, Value};
use tokio::sync::watch;
use url::Url;
use uuid::Uuid;

const CREATE_GROUP_KIND: u16 = 9007;

pub fn open_channel_id(event: &nostr::Event) -> Option<Uuid> {
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
    open.then_some(channel).flatten()
}

pub fn channel_creation_subscription(subscription_id: &str) -> Value {
    let filter = Filter::new().kind(Kind::Custom(CREATE_GROUP_KIND));
    json!(["REQ", subscription_id, filter])
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

pub async fn next_open_channel(
    connection: &mut NostrWsConnection,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Option<Uuid>, String> {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(None);
                }
            }
            message = connection.next_event(Duration::from_secs(24 * 60 * 60)) => {
                match message.map_err(|e| e.to_string())? {
                    RelayMessage::Event { event, .. } => {
                        if let Some(channel_id) = open_channel_id(&event) {
                            return Ok(Some(channel_id));
                        }
                    }
                    _ => {}
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
        assert!(open_channel_id(&create("open")).is_some());
        assert!(open_channel_id(&create("private")).is_none());
        let other = EventBuilder::new(Kind::Custom(9), "")
            .sign_with_keys(&Keys::generate())
            .unwrap();
        assert!(open_channel_id(&other).is_none());
    }

    #[test]
    fn subscription_uses_nip29_create_kind() {
        let value = channel_creation_subscription("autojoin");
        assert_eq!(value[0], "REQ");
        assert_eq!(value[1], "autojoin");
        assert_eq!(value[2]["kinds"][0], CREATE_GROUP_KIND);
    }
}
