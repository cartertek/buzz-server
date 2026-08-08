//! Community configuration and relay boundary validation.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{CommunityConfigId, ValidationError};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommunityConfig {
    pub id: CommunityConfigId,
    pub display_name: String,
    pub relay_url: Url,
    /// Public key of the human identity used to join this community.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_pubkey: Option<String>,
}

impl CommunityConfig {
    pub fn new(display_name: impl Into<String>, relay_url: Url) -> Result<Self, ValidationError> {
        let config = Self {
            id: CommunityConfigId::new(),
            display_name: display_name.into(),
            relay_url,
            identity_pubkey: None,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_identity_pubkey(
        mut self,
        pubkey: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let pubkey = pubkey.into().trim().to_ascii_lowercase();
        if pubkey.len() != 64 || !pubkey.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ValidationError::new(
                "identity_pubkey",
                "must be a 64-character hex Nostr public key",
            ));
        }
        self.identity_pubkey = Some(pubkey);
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_nonempty("display_name", &self.display_name, 120)?;
        if !matches!(self.relay_url.scheme(), "ws" | "wss") {
            return Err(ValidationError::new(
                "relay_url",
                "must use the ws or wss scheme",
            ));
        }
        if self.relay_url.host_str().is_none() {
            return Err(ValidationError::new("relay_url", "must include a host"));
        }
        if !self.relay_url.username().is_empty() || self.relay_url.password().is_some() {
            return Err(ValidationError::new(
                "relay_url",
                "must not include embedded credentials",
            ));
        }
        if self.relay_url.fragment().is_some() || self.relay_url.query().is_some() {
            return Err(ValidationError::new(
                "relay_url",
                "must not include a query or fragment",
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_nonempty(
    field: &'static str,
    value: &str,
    max_chars: usize,
) -> Result<(), ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ValidationError::new(field, "must not be empty"));
    }
    if trimmed.chars().count() > max_chars {
        return Err(ValidationError::new(
            field,
            format!("must be at most {max_chars} characters"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn community_config_round_trips() {
        let config = CommunityConfig::new(
            "Engineering",
            Url::parse("wss://buzz.example.test").unwrap(),
        )
        .unwrap();

        let encoded = serde_json::to_string(&config).unwrap();
        assert_eq!(
            serde_json::from_str::<CommunityConfig>(&encoded).unwrap(),
            config
        );
    }

    #[test]
    fn relay_url_requires_websocket_scheme() {
        let error = CommunityConfig::new(
            "Engineering",
            Url::parse("https://buzz.example.test").unwrap(),
        )
        .unwrap_err();

        assert_eq!(error.field, "relay_url");
    }
}
