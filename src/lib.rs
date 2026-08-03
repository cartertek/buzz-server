//! Domain foundations for Buzz Server.

pub mod agent;
pub mod community;
pub mod error;
pub mod id;
pub mod operation;

pub use agent::{AgentSpec, DesiredAgentState, RuntimeSpec};
pub use community::CommunityConfig;
pub use error::{ApiError, ErrorCode, ValidationError};
pub use id::{AgentId, CommunityConfigId, OperationId};
pub use operation::{OperationKind, OperationStatus};
