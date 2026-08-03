//! Strongly typed identifiers used at persistence and API boundaries.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a time-ordered identifier suitable for durable records.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}{}", $prefix, self.0.simple())
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let encoded = value
                    .strip_prefix($prefix)
                    .ok_or(ParseIdError::WrongPrefix { expected: $prefix })?;
                if encoded.len() != 32
                    || !encoded
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                {
                    return Err(ParseIdError::NonCanonical);
                }
                let uuid = Uuid::parse_str(encoded).map_err(ParseIdError::InvalidUuid)?;
                Ok(Self(uuid))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

#[derive(Debug, thiserror::Error)]
pub enum ParseIdError {
    #[error("identifier must start with {expected}")]
    WrongPrefix { expected: &'static str },
    #[error("identifier UUID must use 32 lowercase hexadecimal characters")]
    NonCanonical,
    #[error("identifier contains an invalid UUID: {0}")]
    InvalidUuid(uuid::Error),
}

typed_id!(CommunityConfigId, "community_");
typed_id!(AgentId, "agent_");
typed_id!(OperationId, "operation_");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_strings_and_json() {
        let id = AgentId::new();
        let encoded = id.to_string();

        assert_eq!(encoded.parse::<AgentId>().unwrap(), id);
        assert_eq!(
            serde_json::from_str::<AgentId>(&serde_json::to_string(&id).unwrap()).unwrap(),
            id
        );
    }

    #[test]
    fn id_prefixes_prevent_cross_type_parsing() {
        let id = CommunityConfigId::new().to_string();
        let error = id.parse::<AgentId>().unwrap_err();

        assert!(matches!(
            error,
            ParseIdError::WrongPrefix { expected: "agent_" }
        ));
    }

    #[test]
    fn ids_reject_noncanonical_uuid_encodings() {
        let id = AgentId::new().as_uuid();

        assert!(matches!(
            format!("agent_{id}").parse::<AgentId>(),
            Err(ParseIdError::NonCanonical)
        ));
    }
}
