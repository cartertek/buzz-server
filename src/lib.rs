//! Domain foundations for Buzz Server.

pub mod agent;
pub mod api;
pub mod application;
pub mod auth;
pub mod community;
pub mod community_session;
pub mod error;
pub mod id;
pub mod launch;
pub mod operation;
pub mod provider;
pub mod provider_discovery;
pub mod reconcile;
pub mod relay_adapter;
pub mod runtime;
pub mod signer;
pub mod signer_ipc;
pub mod storage;
pub mod supervisor;
pub mod transport;

pub use agent::{AgentSpec, DesiredAgentState, RuntimeSpec};
pub use community::CommunityConfig;
pub use error::{ApiError, ErrorCode, ValidationError};
pub use id::{AgentId, CommunityConfigId, OperationId};
pub use launch::{
    LaunchIdentity, LaunchResolutionError, LaunchSpec, LocalLaunchContext, ObservedProcessState,
    ProcessReceipt, ResolvedRuntime,
};
pub use operation::{OperationKind, OperationStatus};
pub use runtime::{
    CatalogError, PreflightProbe, RuntimeArtifact, RuntimeCatalog, RuntimeCatalogEntry, RuntimeId,
    SecretReference,
};
pub use storage::{DurableOperation, SqliteStore, StorageError};
