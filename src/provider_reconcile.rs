//! Durable, authorization-ordered provider deployment seam.
//!
//! The application operation worker owns the transaction and authorization
//! proof. This coordinator atomically records intent before invoking an
//! external provider. A crash after external success therefore leaves an
//! explicit in-flight record which must be reconciled by stable request ID;
//! it is never converted into a blind second deploy.

use serde::{Deserialize, Serialize};

use crate::provider::{NegotiatedProvider, ProviderError};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderDeploymentIntent {
    pub request_id: String,
    pub provider_id: String,
    pub staged_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderDeploymentReceipt {
    pub request_id: String,
    pub provider_id: String,
    pub staged_sha256: String,
    pub external_agent_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BeginDeployment {
    Acquired,
    InFlight(ProviderDeploymentIntent),
    Complete(ProviderDeploymentReceipt),
}

/// Storage implementations must make `begin` an atomic insert-if-absent and
/// `complete` a compare-and-set from the matching in-flight record. This is
/// the concurrency boundary for a deployment request ID.
pub trait ProviderDeploymentRepository {
    type Error;

    fn begin(&self, intent: &ProviderDeploymentIntent) -> Result<BeginDeployment, Self::Error>;
    fn complete(
        &self,
        receipt: &ProviderDeploymentReceipt,
    ) -> Result<ProviderDeploymentReceipt, Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderReconcileError<E> {
    #[error("provider deployment failed: {0}")]
    Provider(#[from] ProviderError),
    #[error("provider deployment storage failed")]
    Storage(E),
    #[error("provider deployment record does not match the request or negotiated provider")]
    RecordMismatch,
    #[error(
        "provider deployment remains in flight; reconcile external state by stable request ID before retrying"
    )]
    InFlightRequiresReconciliation,
}

pub struct ProviderDeploymentCoordinator<'a, R> {
    deployments: &'a R,
}

impl<'a, R> ProviderDeploymentCoordinator<'a, R> {
    #[must_use]
    pub const fn new(deployments: &'a R) -> Self {
        Self { deployments }
    }
}

impl<R: ProviderDeploymentRepository> ProviderDeploymentCoordinator<'_, R> {
    /// The payload closure performs the application's already-signed lookup.
    /// It runs only for the caller that atomically acquires a new intent and
    /// only after provider trust and `info` negotiation have completed.
    pub fn deploy_once<F>(
        &self,
        request_id: &str,
        provider: &NegotiatedProvider,
        build_authorized_payload: F,
    ) -> Result<ProviderDeploymentReceipt, ProviderReconcileError<R::Error>>
    where
        F: FnOnce() -> Result<(serde_json::Value, serde_json::Value), ProviderError>,
    {
        if request_id.is_empty() || request_id.len() > 200 {
            return Err(ProviderError::Payload.into());
        }
        let intent = ProviderDeploymentIntent {
            request_id: request_id.to_owned(),
            provider_id: provider.id.clone(),
            staged_sha256: provider.staged_sha256.clone(),
        };
        match self
            .deployments
            .begin(&intent)
            .map_err(ProviderReconcileError::Storage)?
        {
            BeginDeployment::Acquired => {}
            BeginDeployment::InFlight(existing) => {
                validate_intent(&existing, &intent)?;
                return Err(ProviderReconcileError::InFlightRequiresReconciliation);
            }
            BeginDeployment::Complete(receipt) => {
                validate_receipt(&receipt, &intent)?;
                return Ok(receipt);
            }
        }

        let external_agent_id = provider.deploy_idempotent(request_id, build_authorized_payload)?;
        let receipt = ProviderDeploymentReceipt {
            request_id: request_id.to_owned(),
            provider_id: provider.id.clone(),
            staged_sha256: provider.staged_sha256.clone(),
            external_agent_id,
        };
        let stored = self
            .deployments
            .complete(&receipt)
            .map_err(ProviderReconcileError::Storage)?;
        validate_receipt(&stored, &intent)?;
        Ok(stored)
    }
}

fn validate_intent<E>(
    actual: &ProviderDeploymentIntent,
    expected: &ProviderDeploymentIntent,
) -> Result<(), ProviderReconcileError<E>> {
    if actual == expected {
        Ok(())
    } else {
        Err(ProviderReconcileError::RecordMismatch)
    }
}

fn validate_receipt<E>(
    receipt: &ProviderDeploymentReceipt,
    expected: &ProviderDeploymentIntent,
) -> Result<(), ProviderReconcileError<E>> {
    if receipt.request_id == expected.request_id
        && receipt.provider_id == expected.provider_id
        && receipt.staged_sha256 == expected.staged_sha256
        && !receipt.external_agent_id.is_empty()
    {
        Ok(())
    } else {
        Err(ProviderReconcileError::RecordMismatch)
    }
}
