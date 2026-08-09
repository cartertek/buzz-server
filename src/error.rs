//! Stable validation and API error contracts.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct ValidationError {
    pub field: &'static str,
    pub message: String,
}

impl ValidationError {
    #[must_use]
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    NotFound,
    Conflict,
    Unauthorized,
    Forbidden,
    Unsupported,
    Unavailable,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl ApiError {
    #[must_use]
    pub fn validation(error: ValidationError) -> Self {
        Self {
            code: ErrorCode::InvalidRequest,
            message: error.message,
            field: Some(error.field.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_has_a_stable_wire_shape() {
        let error = ApiError::validation(ValidationError::new("display_name", "must not be empty"));
        let value = serde_json::to_value(error).unwrap();

        assert_eq!(value["code"], "invalid_request");
        assert_eq!(value["field"], "display_name");
        assert_eq!(value["message"], "must not be empty");
    }
}
