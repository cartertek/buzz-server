//! Durable, authorization-ordered provider deployment seam.
//!
//! The application operation worker owns the transaction and authorization
//! proof. This coordinator owns only provider idempotency and never constructs
//! secret payloads unless negotiation has already succeeded.

use serde::{Deserialize, Serialize};

use crate::provider::{NegotiatedProvider, ProviderError};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderDeploymentReceipt {
    pub request_id: String,
    pub provider_id: String,
    pub staged_sha256: String,
    pub external_agent_id: String,
}

pub trait ProviderDeploymentReceiptRepository {
    type Error;

    fn get(&self, request_id: &str) -> Result<Option<ProviderDeploymentReceipt>, Self::Error>;
    fn put_if_absent(
        &self,
        receipt: &ProviderDeploymentReceipt,
    ) -> Result<ProviderDeploymentReceipt, Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderReconcileError<E> {
    #[error("provider deployment failed: {0}")]
    Provider(#[from] ProviderError),
    #[error("provider deployment receipt storage failed")]
    Storage(E),
    #[error("provider deployment receipt does not match the negotiated provider")]
    ReceiptMismatch,
}

pub struct ProviderDeploymentCoordinator<'a, R> {
    receipts: &'a R,
}

impl<'a, R> ProviderDeploymentCoordinator<'a, R> {
    #[must_use]
    pub const fn new(receipts: &'a R) -> Self {
        Self { receipts }
    }
}

impl<R: ProviderDeploymentReceiptRepository> ProviderDeploymentCoordinator<'_, R> {
    /// The payload closure must perform the application's already-signed
    /// deployment lookup. It is not called for an existing receipt, nor until
    /// provider trust and `info` negotiation have completed.
    pub fn deploy_once<F>(
        &self,
        request_id: &str,
        provider: &NegotiatedProvider,
        build_authorized_payload: F,
    ) -> Result<ProviderDeploymentReceipt, ProviderReconcileError<R::Error>>
    where
        F: FnOnce() -> Result<(serde_json::Value, serde_json::Value), ProviderError>,
    {
        if let Some(receipt) = self
            .receipts
            .get(request_id)
            .map_err(ProviderReconcileError::Storage)?
        {
            if receipt.provider_id != provider.id || receipt.staged_sha256 != provider.staged_sha256
            {
                return Err(ProviderReconcileError::ReceiptMismatch);
            }
            return Ok(receipt);
        }

        let external_agent_id = provider.deploy_idempotent(request_id, build_authorized_payload)?;
        let receipt = ProviderDeploymentReceipt {
            request_id: request_id.to_owned(),
            provider_id: provider.id.clone(),
            staged_sha256: provider.staged_sha256.clone(),
            external_agent_id,
        };
        self.receipts
            .put_if_absent(&receipt)
            .map_err(ProviderReconcileError::Storage)
    }
}
