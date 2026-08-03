//! Durable operation kinds and state-transition rules.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    CreateAgent,
    UpdateAgent,
    EnableAgent,
    DisableAgent,
    DeleteAgent,
    PurgeAgent,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl OperationStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Running | Self::Cancelled)
                | (
                    Self::Running,
                    Self::Succeeded | Self::Failed | Self::Cancelled
                )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_transitions_are_monotonic() {
        assert!(OperationStatus::Pending.can_transition_to(OperationStatus::Running));
        assert!(OperationStatus::Running.can_transition_to(OperationStatus::Succeeded));
        assert!(!OperationStatus::Succeeded.can_transition_to(OperationStatus::Running));
        assert!(!OperationStatus::Pending.can_transition_to(OperationStatus::Succeeded));
    }

    #[test]
    fn operation_status_wire_values_are_stable() {
        assert_eq!(
            serde_json::to_string(&OperationStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::from_str::<OperationStatus>("\"cancelled\"").unwrap(),
            OperationStatus::Cancelled
        );
    }
}
