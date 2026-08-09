//! SQLite-backed lifecycle application service.

use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use crate::{
    api::{
        AgentCommandRequest, AgentLogsRequest, AgentLogsResource, AgentResource, ApplicationError,
        ChangeAgentStateRequest, CommandMetadata, CreateAgentInput, DraftResource,
        JoinCommunityRequest, LifecycleApplication, ListAgentsRequest, OperationResource,
        SubmitDraftRequest, UpdateAgentInput, UpdateAgentRequest,
    },
    auth::{AuthenticatedPrincipal, Principal},
    storage::{AgentCommandMutation, DurableOperation, IdempotencyRecord, NewAuditRecord},
    AgentId, AgentSpec, CommunityConfig, CommunityConfigId, DesiredAgentState, OperationId,
    OperationKind, OperationStatus, RuntimeSpec, SqliteStore, StorageError,
};

pub const DEFAULT_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

pub trait LifecycleEffects: Send + Sync {
    /// Verify that the Buzz Server identity can join an existing community before persistence.
    fn verify_community_join(&self, _community: &CommunityConfig) -> Result<(), ApplicationError> {
        Ok(())
    }

    /// Called after the last community reference to an internally custodied identity is removed.
    fn community_identity_unreferenced(&self, _pubkey: &str) {}

    /// Resolve create input into the daemon's effective lifecycle cache. Production
    /// resolves file/persona semantics; the default keeps application tests transport-neutral.
    fn prepare_agent_create(
        &self,
        id: AgentId,
        input: &CreateAgentInput,
    ) -> Result<AgentSpec, ApplicationError> {
        let runtime_id = input.runtime_id.clone().ok_or_else(|| {
            ApplicationError::Invalid(crate::ValidationError::new(
                "runtime_id",
                "is required when no file-backed persona resolver is installed",
            ))
        })?;
        Ok(AgentSpec {
            id,
            community_config_id: input.community_config_id,
            display_name: input.display_name.clone(),
            system_prompt: input.system_prompt.clone().unwrap_or_default(),
            runtime: RuntimeSpec {
                runtime_id,
                environment: Default::default(),
            },
            desired_state: DesiredAgentState::Enabled,
        })
    }

    /// Persist the human-authored configuration after the durable create commit
    /// and before reconciliation is awakened. Implementations must not overwrite
    /// an existing hand-edited file on an idempotent replay.
    fn persist_agent_create(
        &self,
        _agent: &AgentSpec,
        _input: &CreateAgentInput,
    ) -> Result<(), ApplicationError> {
        Ok(())
    }

    /// Mirror legacy CLI update fields into the human-authored file. Direct file
    /// edits remain the primary configuration interface.
    fn prepare_agent_update(
        &self,
        _current: &AgentSpec,
        _changes: &UpdateAgentInput,
    ) -> Result<(), ApplicationError> {
        Ok(())
    }

    /// Return the public half of a custodied agent identity for API resources.
    fn agent_public_key(&self, _agent_id: AgentId) -> Option<String> {
        None
    }

    /// Wakes the reconciler after the complete durable command transaction commits.
    fn operation_ready(&self, operation: &DurableOperation) -> Result<(), ApplicationError>;
}

pub struct SqliteLifecycleApplication<E> {
    store: Arc<SqliteStore>,
    effects: Arc<E>,
    now: fn() -> i64,
    retention_seconds: i64,
    completion: Arc<OperationCompletionSignal>,
}

impl<E> Clone for SqliteLifecycleApplication<E> {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            effects: Arc::clone(&self.effects),
            now: self.now,
            retention_seconds: self.retention_seconds,
            completion: Arc::clone(&self.completion),
        }
    }
}

#[derive(Default)]
struct OperationCompletionSignal {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl OperationCompletionSignal {
    fn snapshot(&self) -> Result<u64, ApplicationError> {
        self.generation
            .lock()
            .map(|generation| *generation)
            .map_err(|_| ApplicationError::Internal)
    }

    fn notify(&self) -> Result<(), ApplicationError> {
        let mut generation = self
            .generation
            .lock()
            .map_err(|_| ApplicationError::Internal)?;
        *generation = generation.wrapping_add(1);
        self.changed.notify_all();
        Ok(())
    }

    fn wait_for_change(&self, snapshot: u64, timeout: Duration) -> Result<(), ApplicationError> {
        let generation = self
            .generation
            .lock()
            .map_err(|_| ApplicationError::Internal)?;
        if *generation != snapshot {
            return Ok(());
        }
        let _ = self
            .changed
            .wait_timeout_while(generation, timeout, |generation| *generation == snapshot)
            .map_err(|_| ApplicationError::Internal)?;
        Ok(())
    }
}

struct ExecutionPlan<'a> {
    kind: OperationKind,
    agent_id: AgentId,
    community_id: crate::CommunityConfigId,
    mutation: AgentCommandMutation<'a>,
    promoted_draft_id: Option<&'a str>,
    wake_immediately: bool,
}

impl<E: LifecycleEffects> SqliteLifecycleApplication<E> {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>, effects: Arc<E>, now: fn() -> i64) -> Self {
        Self {
            store,
            effects,
            now,
            retention_seconds: DEFAULT_RETENTION_SECONDS,
            completion: Arc::new(OperationCompletionSignal::default()),
        }
    }

    #[must_use]
    pub const fn with_retention_seconds(mut self, retention_seconds: i64) -> Self {
        self.retention_seconds = retention_seconds;
        self
    }

    fn wait_for_operation_terminal(
        &self,
        id: OperationId,
        timeout: Duration,
    ) -> Result<OperationResource, ApplicationError> {
        let deadline = Instant::now() + timeout;
        loop {
            let snapshot = self.completion.snapshot()?;
            let operation = self.get_operation(id)?;
            if operation.status.is_terminal() || Instant::now() >= deadline {
                return Ok(operation);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            self.completion.wait_for_change(snapshot, remaining)?;
        }
    }

    pub fn expired_retained_agents(&self) -> Result<Vec<AgentId>, ApplicationError> {
        self.store
            .expired_retained_agents((self.now)())
            .map_err(ApplicationError::from)
    }

    fn execute(
        &self,
        actor: &AuthenticatedPrincipal,
        metadata: &CommandMetadata,
        request: &impl serde::Serialize,
        plan: ExecutionPlan<'_>,
    ) -> Result<OperationResource, ApplicationError> {
        let ExecutionPlan {
            kind,
            agent_id,
            community_id,
            mutation,
            promoted_draft_id,
            wake_immediately,
        } = plan;
        let now = (self.now)();
        let operation = DurableOperation {
            id: OperationId::new(),
            kind,
            status: OperationStatus::Pending,
            agent_id: Some(agent_id),
            error_code: None,
            created_at: now,
            updated_at: now,
            correlation_id: metadata.correlation_id.clone(),
        };
        let principal_scope = principal_scope(actor);
        let idempotency = IdempotencyRecord {
            scope: format!("{principal_scope}:{}", operation_action(kind)),
            key: metadata.idempotency_key.clone(),
            request_hash: request_hash(request)?,
            operation_id: operation.id,
            created_at: now,
        };
        let subject = agent_id.to_string();
        let principal = audit_actor(actor);
        let (operation, _replayed) = self.store.apply_agent_command(
            &operation,
            &idempotency,
            mutation,
            promoted_draft_id,
            NewAuditRecord {
                occurred_at: now,
                actor_principal: &principal,
                authentication_method: authentication_method(actor),
                community_config_id: Some(community_id),
                operation_id: Some(operation.id),
                correlation_id: &metadata.correlation_id,
                idempotency_key: Some(&metadata.idempotency_key),
                action: operation_action(kind),
                subject_type: "agent",
                subject_id: Some(&subject),
                outcome: "accepted",
                redacted_detail: None,
            },
        )?;
        if wake_immediately
            && matches!(
                operation.status,
                OperationStatus::Pending | OperationStatus::Running
            )
        {
            self.effects.operation_ready(&operation)?;
        }
        Ok(operation_resource(&operation))
    }

    fn existing_community(
        &self,
        agent_id: AgentId,
    ) -> Result<crate::CommunityConfigId, ApplicationError> {
        self.store
            .get_agent(agent_id)?
            .map(|agent| agent.community_config_id)
            .ok_or(ApplicationError::NotFound)
    }

    /// Records the reconciler claiming a durable operation before performing side effects.
    pub fn start_operation(&self, id: OperationId) -> Result<(), ApplicationError> {
        let operation = self
            .store
            .get_operation(id)?
            .ok_or(ApplicationError::NotFound)?;
        self.store
            .transition_operation(id, OperationStatus::Running, None, (self.now)())?;
        self.audit_reconciliation(&operation, "operation.running", "accepted")
    }

    /// Persists a terminal reconciliation result. Successful purge completion, audit, tombstone,
    /// and removal are one SQLite transaction so a restart cannot observe a terminal zombie.
    pub fn complete_operation(
        &self,
        id: OperationId,
        status: OperationStatus,
        error_code: Option<crate::ErrorCode>,
    ) -> Result<(), ApplicationError> {
        if !matches!(
            status,
            OperationStatus::Succeeded | OperationStatus::Failed | OperationStatus::Cancelled
        ) {
            return Err(ApplicationError::Invalid(crate::ValidationError::new(
                "status",
                "completion status must be terminal",
            )));
        }
        let operation = self
            .store
            .get_operation(id)?
            .ok_or(ApplicationError::NotFound)?;
        if status == OperationStatus::Succeeded && operation.kind == OperationKind::PurgeAgent {
            let agent_id = operation
                .agent_id
                .ok_or_else(|| ApplicationError::Conflict("purge operation has no agent".into()))?;
            let community = self
                .store
                .get_agent(agent_id)?
                .ok_or(ApplicationError::NotFound)?
                .community_config_id;
            let subject = agent_id.to_string();
            self.store.complete_purge(
                agent_id,
                id,
                (self.now)(),
                NewAuditRecord {
                    occurred_at: (self.now)(),
                    actor_principal: "system:reconciler",
                    authentication_method: "internal",
                    community_config_id: Some(community),
                    operation_id: Some(id),
                    correlation_id: &operation.correlation_id,
                    idempotency_key: None,
                    action: "operation.complete",
                    subject_type: "agent",
                    subject_id: Some(&subject),
                    outcome: "succeeded",
                    redacted_detail: None,
                },
            )?;
            self.completion.notify()?;
            return Ok(());
        }
        self.store
            .transition_operation(id, status, error_code, (self.now)())?;
        self.audit_reconciliation(
            &operation,
            "operation.complete",
            if status == OperationStatus::Succeeded {
                "succeeded"
            } else {
                "failed"
            },
        )?;
        self.completion.notify()?;
        Ok(())
    }

    fn audit_reconciliation(
        &self,
        operation: &DurableOperation,
        action: &'static str,
        outcome: &'static str,
    ) -> Result<(), ApplicationError> {
        let subject = operation.agent_id.map(|id| id.to_string());
        let community = operation
            .agent_id
            .and_then(|id| self.store.get_agent(id).ok().flatten())
            .map(|agent| agent.community_config_id);
        self.store.append_audit(NewAuditRecord {
            occurred_at: (self.now)(),
            actor_principal: "system:reconciler",
            authentication_method: "internal",
            community_config_id: community,
            operation_id: Some(operation.id),
            correlation_id: &operation.correlation_id,
            idempotency_key: None,
            action,
            subject_type: "agent",
            subject_id: subject.as_deref(),
            outcome,
            redacted_detail: None,
        })?;
        Ok(())
    }
}

impl<E: LifecycleEffects> LifecycleApplication for SqliteLifecycleApplication<E> {
    fn join_community(
        &self,
        request: &JoinCommunityRequest,
    ) -> Result<CommunityConfig, ApplicationError> {
        let community =
            CommunityConfig::new(request.display_name.clone(), request.relay_url.clone())
                .and_then(|community| {
                    community.with_identity_pubkey(request.identity_pubkey.clone())
                })
                .map_err(ApplicationError::Invalid)?;
        self.effects.verify_community_join(&community)?;
        self.store.put_community(&community, (self.now)())?;
        Ok(community)
    }

    fn update_community(
        &self,
        request: &crate::api::UpdateCommunityRequest,
    ) -> Result<CommunityConfig, ApplicationError> {
        let mut community = self.get_community(request.community_id)?;
        community.display_name.clone_from(&request.display_name);
        community.validate().map_err(ApplicationError::Invalid)?;
        self.store.put_community(&community, (self.now)())?;
        Ok(community)
    }

    fn get_community(&self, id: CommunityConfigId) -> Result<CommunityConfig, ApplicationError> {
        self.store
            .get_community(id)?
            .ok_or(ApplicationError::NotFound)
    }

    fn list_communities(&self) -> Result<Vec<CommunityConfig>, ApplicationError> {
        self.store
            .list_communities()
            .map_err(ApplicationError::from)
    }

    fn remove_community(&self, id: CommunityConfigId) -> Result<CommunityConfig, ApplicationError> {
        let community = self.get_community(id)?;
        self.store
            .delete_community_with_deleted_agents(id, (self.now)())?;
        if let Some(pubkey) = community.identity_pubkey.as_deref() {
            let still_referenced = self
                .store
                .list_communities()?
                .iter()
                .any(|candidate| candidate.identity_pubkey.as_deref() == Some(pubkey));
            if !still_referenced {
                self.effects.community_identity_unreferenced(pubkey);
            }
        }
        Ok(community)
    }

    fn create_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        metadata: &CommandMetadata,
        input: &CreateAgentInput,
    ) -> Result<OperationResource, ApplicationError> {
        let agent = self.effects.prepare_agent_create(AgentId::new(), input)?;
        agent.validate().map_err(ApplicationError::Invalid)?;
        let resource = self.execute(
            actor,
            metadata,
            input,
            ExecutionPlan {
                kind: OperationKind::CreateAgent,
                agent_id: agent.id,
                community_id: agent.community_config_id,
                mutation: AgentCommandMutation::Create(&agent),
                promoted_draft_id: None,
                wake_immediately: false,
            },
        )?;
        let committed_agent_id = resource.agent_id.ok_or(ApplicationError::Internal)?;
        let committed_agent = if committed_agent_id == agent.id {
            agent
        } else {
            self.store
                .get_agent(committed_agent_id)?
                .ok_or(ApplicationError::NotFound)?
        };
        self.effects.persist_agent_create(&committed_agent, input)?;
        if let Some(operation) = self.store.get_operation(resource.id)? {
            if matches!(
                operation.status,
                OperationStatus::Pending | OperationStatus::Running
            ) {
                self.effects.operation_ready(&operation)?;
            }
        }
        Ok(resource)
    }

    fn update_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &UpdateAgentRequest,
    ) -> Result<OperationResource, ApplicationError> {
        let current = self
            .store
            .get_agent(request.agent_id)?
            .ok_or(ApplicationError::NotFound)?;
        let community = current.community_config_id;
        self.effects
            .prepare_agent_update(&current, &request.changes)?;
        self.execute(
            actor,
            &request.metadata,
            request,
            ExecutionPlan {
                kind: OperationKind::UpdateAgent,
                agent_id: request.agent_id,
                community_id: community,
                mutation: AgentCommandMutation::Update {
                    id: request.agent_id,
                    changes: &request.changes,
                },
                promoted_draft_id: None,
                wake_immediately: true,
            },
        )
    }

    fn change_agent_state(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &ChangeAgentStateRequest,
    ) -> Result<OperationResource, ApplicationError> {
        let kind = match request.desired_state {
            DesiredAgentState::Enabled => OperationKind::EnableAgent,
            DesiredAgentState::Disabled => OperationKind::DisableAgent,
            DesiredAgentState::Deleted => {
                return Err(ApplicationError::Invalid(crate::ValidationError::new(
                    "desired_state",
                    "deleted state requires delete",
                )));
            }
        };
        let community = self.existing_community(request.agent_id)?;
        self.execute(
            actor,
            &request.metadata,
            request,
            ExecutionPlan {
                kind,
                agent_id: request.agent_id,
                community_id: community,
                mutation: AgentCommandMutation::SetState {
                    id: request.agent_id,
                    desired_state: request.desired_state,
                    retention_deadline: None,
                },
                promoted_draft_id: None,
                wake_immediately: true,
            },
        )
    }

    fn delete_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentCommandRequest,
    ) -> Result<OperationResource, ApplicationError> {
        self.deleted_command(actor, request, OperationKind::DeleteAgent)
    }

    fn purge_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentCommandRequest,
    ) -> Result<OperationResource, ApplicationError> {
        // Purge first records Deleted intent. Physical removal, terminal success, audit, and the
        // tombstone are committed atomically only after reconciliation has stopped the process.
        self.deleted_command(actor, request, OperationKind::PurgeAgent)
    }

    fn get_agent(&self, id: AgentId) -> Result<AgentResource, ApplicationError> {
        let agent = self
            .store
            .get_agent(id)?
            .ok_or(ApplicationError::NotFound)?;
        let public_key = self.effects.agent_public_key(id);
        Ok(agent_resource(
            agent,
            self.store
                .agent_retention(id)?
                .map(|value| value.purge_after),
            public_key,
        ))
    }

    fn list_agents(
        &self,
        request: &ListAgentsRequest,
    ) -> Result<Vec<AgentResource>, ApplicationError> {
        Ok(self
            .store
            .list_agents(request.community_config_id)?
            .into_iter()
            .filter(|agent| {
                request.include_deleted || agent.desired_state != DesiredAgentState::Deleted
            })
            .map(|agent| {
                let purge_after = self
                    .store
                    .agent_retention(agent.id)?
                    .map(|value| value.purge_after);
                let public_key = self.effects.agent_public_key(agent.id);
                Ok(agent_resource(agent, purge_after, public_key))
            })
            .collect::<Result<Vec<_>, StorageError>>()?)
    }

    fn agent_logs(
        &self,
        request: &AgentLogsRequest,
    ) -> Result<AgentLogsResource, ApplicationError> {
        self.existing_community(request.agent_id)?;
        let entries = self.store.agent_logs(
            request.agent_id,
            request.after_cursor.as_deref(),
            request.limit,
        )?;
        let next_cursor = entries.last().map(|entry| entry.cursor.clone());
        Ok(AgentLogsResource {
            entries,
            next_cursor,
        })
    }

    fn get_operation(&self, id: OperationId) -> Result<OperationResource, ApplicationError> {
        self.store
            .get_operation(id)?
            .as_ref()
            .map(operation_resource)
            .ok_or(ApplicationError::NotFound)
    }

    fn wait_operation(
        &self,
        id: OperationId,
        timeout: Duration,
    ) -> Result<OperationResource, ApplicationError> {
        self.wait_for_operation_terminal(id, timeout)
    }

    fn submit_draft(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &SubmitDraftRequest,
    ) -> Result<DraftResource, ApplicationError> {
        request
            .agent
            .validate()
            .map_err(ApplicationError::Invalid)?;
        let draft = DraftResource {
            id: format!("draft_{}", uuid::Uuid::now_v7().simple()),
            owner: actor.principal.ownership_key(),
            agent: request.agent.clone(),
        };
        let principal_scope = principal_scope(actor);
        let principal = audit_actor(actor);
        let (draft, _) = self.store.apply_draft_command(
            &draft,
            &principal_scope,
            &request.metadata.idempotency_key,
            &request_hash(request)?,
            (self.now)(),
            NewAuditRecord {
                occurred_at: (self.now)(),
                actor_principal: &principal,
                authentication_method: authentication_method(actor),
                community_config_id: Some(request.agent.community_config_id),
                operation_id: None,
                correlation_id: &request.metadata.correlation_id,
                idempotency_key: Some(&request.metadata.idempotency_key),
                action: "draft.submit",
                subject_type: "draft",
                subject_id: Some(&draft.id),
                outcome: "accepted",
                redacted_detail: None,
            },
        )?;
        Ok(draft)
    }

    fn get_draft(&self, id: &str) -> Result<DraftResource, ApplicationError> {
        self.store.get_draft(id)?.ok_or(ApplicationError::NotFound)
    }

    fn promote_draft(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &crate::api::PromoteDraftRequest,
    ) -> Result<OperationResource, ApplicationError> {
        let draft = self.get_draft(&request.draft_id)?;
        draft.agent.validate().map_err(ApplicationError::Invalid)?;
        let agent = self
            .effects
            .prepare_agent_create(AgentId::new(), &draft.agent)?;
        agent.validate().map_err(ApplicationError::Invalid)?;
        let resource = self.execute(
            actor,
            &request.metadata,
            request,
            ExecutionPlan {
                kind: OperationKind::CreateAgent,
                agent_id: agent.id,
                community_id: agent.community_config_id,
                mutation: AgentCommandMutation::Create(&agent),
                promoted_draft_id: Some(&request.draft_id),
                wake_immediately: false,
            },
        )?;
        let committed_agent_id = resource.agent_id.ok_or(ApplicationError::Internal)?;
        let committed_agent = if committed_agent_id == agent.id {
            agent
        } else {
            self.store
                .get_agent(committed_agent_id)?
                .ok_or(ApplicationError::NotFound)?
        };
        self.effects
            .persist_agent_create(&committed_agent, &draft.agent)?;
        if let Some(operation) = self.store.get_operation(resource.id)? {
            if matches!(
                operation.status,
                OperationStatus::Pending | OperationStatus::Running
            ) {
                self.effects.operation_ready(&operation)?;
            }
        }
        Ok(resource)
    }
}

impl<E: LifecycleEffects> SqliteLifecycleApplication<E> {
    fn deleted_command(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentCommandRequest,
        kind: OperationKind,
    ) -> Result<OperationResource, ApplicationError> {
        let agent = self
            .store
            .get_agent(request.agent_id)?
            .ok_or(ApplicationError::NotFound)?;
        let community = agent.community_config_id;
        let retention_deadline = if kind == OperationKind::DeleteAgent {
            if agent.desired_state == DesiredAgentState::Deleted {
                self.store
                    .agent_retention(request.agent_id)?
                    .map(|retention| retention.purge_after)
                    .or_else(|| Some((self.now)().saturating_add(self.retention_seconds.max(0))))
            } else {
                Some((self.now)().saturating_add(self.retention_seconds.max(0)))
            }
        } else {
            None
        };
        self.execute(
            actor,
            &request.metadata,
            request,
            ExecutionPlan {
                kind,
                agent_id: request.agent_id,
                community_id: community,
                mutation: AgentCommandMutation::SetState {
                    id: request.agent_id,
                    desired_state: DesiredAgentState::Deleted,
                    retention_deadline,
                },
                promoted_draft_id: None,
                wake_immediately: true,
            },
        )
    }
}

fn request_hash(value: &impl serde::Serialize) -> Result<String, ApplicationError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ApplicationError::Internal)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn principal_scope(actor: &AuthenticatedPrincipal) -> String {
    match &actor.principal {
        Principal::UnixPeer { uid, .. } => format!("unix_uid:{uid}"),
        Principal::Nip98 { pubkey } => format!("nostr:{pubkey}"),
    }
}

fn audit_actor(actor: &AuthenticatedPrincipal) -> String {
    match &actor.principal {
        Principal::UnixPeer { uid, .. } => format!("unix_uid:{uid}"),
        Principal::Nip98 { pubkey } => {
            let digest = format!("{:x}", Sha256::digest(pubkey.as_bytes()));
            format!("nostr_sha256:{}", &digest[..16])
        }
    }
}

const fn authentication_method(actor: &AuthenticatedPrincipal) -> &'static str {
    match &actor.principal {
        Principal::UnixPeer { .. } => "unix_peer",
        Principal::Nip98 { .. } => "nip98",
    }
}

fn agent_resource(
    agent: AgentSpec,
    purge_after: Option<i64>,
    public_key: Option<String>,
) -> AgentResource {
    AgentResource {
        id: agent.id,
        community_config_id: agent.community_config_id,
        display_name: agent.display_name,
        system_prompt: agent.system_prompt,
        runtime_id: agent.runtime.runtime_id,
        desired_state: agent.desired_state,
        purge_after,
        public_key,
    }
}

fn operation_resource(operation: &DurableOperation) -> OperationResource {
    OperationResource {
        id: operation.id,
        kind: operation.kind,
        status: operation.status,
        agent_id: operation.agent_id,
        correlation_id: operation.correlation_id.clone(),
        error_code: operation.error_code,
        created_at: operation.created_at,
        updated_at: operation.updated_at,
    }
}

const fn operation_action(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::CreateAgent => "agent.create",
        OperationKind::UpdateAgent => "agent.update",
        OperationKind::EnableAgent => "agent.enable",
        OperationKind::DisableAgent => "agent.disable",
        OperationKind::DeleteAgent => "agent.delete",
        OperationKind::PurgeAgent => "agent.purge",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{
        api::{AgentCommandRequest, CreateAgentRequest, PromoteDraftRequest, SubmitDraftRequest},
        auth::{Authority, Principal},
        CommunityConfig,
    };
    use url::Url;

    #[derive(Default)]
    struct Effects(AtomicUsize);

    impl LifecycleEffects for Effects {
        fn operation_ready(&self, _: &DurableOperation) -> Result<(), ApplicationError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn actor() -> AuthenticatedPrincipal {
        AuthenticatedPrincipal {
            principal: Principal::UnixPeer {
                uid: 1000,
                gid: 1000,
                pid: None,
            },
            authority: Authority::Administrator,
        }
    }

    fn metadata(key: &str) -> CommandMetadata {
        CommandMetadata {
            idempotency_key: key.into(),
            correlation_id: format!("correlation-{key}"),
        }
    }

    #[test]
    fn wait_operation_wakes_on_terminal_completion() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let community = CommunityConfig::new(
            "Engineering",
            Url::parse("wss://relay.example.test").unwrap(),
        )
        .unwrap();
        store.put_community(&community, 1).unwrap();
        let effects = Arc::new(Effects::default());
        let service =
            SqliteLifecycleApplication::new(Arc::clone(&store), Arc::clone(&effects), || 10);
        let operation = service
            .create_agent(
                &actor(),
                &metadata("event-wait"),
                &CreateAgentInput {
                    community_config_id: community.id,
                    display_name: "Builder".into(),
                    persona_id: None,
                    system_prompt: Some("Build safely.".into()),
                    runtime_id: Some("codex-acp".parse().unwrap()),
                },
            )
            .unwrap();
        service.start_operation(operation.id).unwrap();

        let waiter = service.clone();
        let operation_id = operation.id;
        let waiting = std::thread::spawn(move || {
            waiter
                .wait_operation(operation_id, Duration::from_secs(2))
                .unwrap()
        });
        std::thread::sleep(Duration::from_millis(25));
        service
            .complete_operation(operation.id, OperationStatus::Succeeded, None)
            .unwrap();

        assert_eq!(waiting.join().unwrap().status, OperationStatus::Succeeded);
    }

    #[test]
    fn create_replay_requeues_without_duplicate_agent_operation_or_audit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lifecycle.sqlite3");
        let community = CommunityConfig::new(
            "Engineering",
            Url::parse("wss://relay.example.test").unwrap(),
        )
        .unwrap();
        let request = CreateAgentRequest {
            metadata: metadata("create-1"),
            agent: CreateAgentInput {
                community_config_id: community.id,
                display_name: "Builder".into(),
                persona_id: None,
                system_prompt: Some("Build safely.".into()),
                runtime_id: Some("codex-acp".parse().unwrap()),
            },
        };
        let first_id;
        {
            let store = Arc::new(SqliteStore::open(&path).unwrap());
            store.put_community(&community, 1).unwrap();
            let effects = Arc::new(Effects::default());
            let service =
                SqliteLifecycleApplication::new(Arc::clone(&store), Arc::clone(&effects), || 10);
            let first = service
                .create_agent(&actor(), &request.metadata, &request.agent)
                .unwrap();
            first_id = first.id;
            assert_eq!(effects.0.load(Ordering::SeqCst), 1);
        }
        let store = Arc::new(SqliteStore::open(&path).unwrap());
        let effects = Arc::new(Effects::default());
        let service =
            SqliteLifecycleApplication::new(Arc::clone(&store), Arc::clone(&effects), || 20);
        let replay = service
            .create_agent(&actor(), &request.metadata, &request.agent)
            .unwrap();
        assert_eq!(replay.id, first_id);
        assert_eq!(effects.0.load(Ordering::SeqCst), 1);
        assert_eq!(store.list_agents(Some(community.id)).unwrap().len(), 1);
    }

    #[test]
    fn purge_is_deferred_until_reconciliation_succeeds() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let community = CommunityConfig::new(
            "Engineering",
            Url::parse("wss://relay.example.test").unwrap(),
        )
        .unwrap();
        store.put_community(&community, 1).unwrap();
        let effects = Arc::new(Effects::default());
        let service =
            SqliteLifecycleApplication::new(Arc::clone(&store), Arc::clone(&effects), || 10);
        let created = service
            .create_agent(
                &actor(),
                &metadata("create"),
                &CreateAgentInput {
                    community_config_id: community.id,
                    display_name: "Builder".into(),
                    persona_id: None,
                    system_prompt: Some("Build safely.".into()),
                    runtime_id: Some("codex-acp".parse().unwrap()),
                },
            )
            .unwrap();
        let agent_id = created.agent_id.unwrap();
        let purge = service
            .purge_agent(
                &actor(),
                &AgentCommandRequest {
                    metadata: metadata("purge"),
                    agent_id,
                },
            )
            .unwrap();
        assert_eq!(
            store.get_agent(agent_id).unwrap().unwrap().desired_state,
            DesiredAgentState::Deleted
        );
        assert!(store.get_agent(agent_id).unwrap().is_some());
        service.start_operation(purge.id).unwrap();
        service
            .complete_operation(purge.id, OperationStatus::Succeeded, None)
            .unwrap();
        assert!(store.get_agent(agent_id).unwrap().is_none());
        assert!(store.is_agent_purged(agent_id).unwrap());
        assert_eq!(
            store
                .audit_for_subject("agent", &agent_id.to_string())
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn draft_promotion_is_one_time_across_distinct_idempotency_keys() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let community = CommunityConfig::new(
            "Engineering",
            Url::parse("wss://relay.example.test").unwrap(),
        )
        .unwrap();
        store.put_community(&community, 1).unwrap();
        let effects = Arc::new(Effects::default());
        let service =
            SqliteLifecycleApplication::new(Arc::clone(&store), Arc::clone(&effects), || 10);
        let draft = service
            .submit_draft(
                &actor(),
                &SubmitDraftRequest {
                    metadata: metadata("draft"),
                    agent: CreateAgentInput {
                        community_config_id: community.id,
                        display_name: "Builder".into(),
                        persona_id: None,
                        system_prompt: Some("Build safely.".into()),
                        runtime_id: Some("codex-acp".parse().unwrap()),
                    },
                },
            )
            .unwrap();
        let first = service
            .promote_draft(
                &actor(),
                &PromoteDraftRequest {
                    metadata: metadata("promote-1"),
                    draft_id: draft.id.clone(),
                },
            )
            .unwrap();
        assert!(matches!(
            service.promote_draft(
                &actor(),
                &PromoteDraftRequest {
                    metadata: metadata("promote-2"),
                    draft_id: draft.id,
                },
            ),
            Err(ApplicationError::Conflict(_))
        ));
        assert_eq!(store.list_agents(Some(community.id)).unwrap().len(), 1);
        assert_eq!(effects.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            store
                .audit_for_subject("agent", &first.agent_id.unwrap().to_string())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn full_lifecycle_contract_persists_polling_logs_retention_and_redacted_audit() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let community = CommunityConfig::new(
            "Engineering",
            Url::parse("wss://relay.example.test").unwrap(),
        )
        .unwrap();
        store.put_community(&community, 1).unwrap();
        let effects = Arc::new(Effects::default());
        let service =
            SqliteLifecycleApplication::new(Arc::clone(&store), Arc::clone(&effects), || 100)
                .with_retention_seconds(50);
        let create = service
            .create_agent(
                &actor(),
                &metadata("complete-create"),
                &CreateAgentInput {
                    community_config_id: community.id,
                    display_name: "Builder".into(),
                    persona_id: None,
                    system_prompt: Some("Build safely.".into()),
                    runtime_id: Some("codex-acp".parse().unwrap()),
                },
            )
            .unwrap();
        let agent_id = create.agent_id.unwrap();
        assert_eq!(service.get_operation(create.id).unwrap().created_at, 100);
        service.start_operation(create.id).unwrap();
        service
            .complete_operation(create.id, OperationStatus::Succeeded, None)
            .unwrap();

        let update = service
            .update_agent(
                &actor(),
                &UpdateAgentRequest {
                    metadata: metadata("complete-update"),
                    agent_id,
                    changes: crate::api::UpdateAgentInput {
                        display_name: Some("Builder 2".into()),
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        assert_eq!(update.kind, OperationKind::UpdateAgent);
        assert_eq!(
            service.get_agent(agent_id).unwrap().display_name,
            "Builder 2"
        );
        assert_eq!(
            service
                .list_agents(&ListAgentsRequest {
                    community_config_id: Some(community.id),
                    include_deleted: false,
                })
                .unwrap()
                .len(),
            1
        );

        store
            .append_redacted_log(
                agent_id,
                &crate::api::RedactedLogEntry {
                    cursor: "log-1".into(),
                    occurred_at: 101,
                    stream: "stderr".into(),
                    redacted_message: "runtime ready".into(),
                },
            )
            .unwrap();
        assert_eq!(
            service
                .agent_logs(&AgentLogsRequest {
                    agent_id,
                    after_cursor: None,
                    limit: 10,
                })
                .unwrap()
                .entries
                .len(),
            1
        );

        let disable = service
            .change_agent_state(
                &actor(),
                &ChangeAgentStateRequest {
                    metadata: metadata("complete-disable"),
                    agent_id,
                    desired_state: DesiredAgentState::Disabled,
                },
            )
            .unwrap();
        assert_eq!(disable.kind, OperationKind::DisableAgent);
        let enable = service
            .change_agent_state(
                &actor(),
                &ChangeAgentStateRequest {
                    metadata: metadata("complete-enable"),
                    agent_id,
                    desired_state: DesiredAgentState::Enabled,
                },
            )
            .unwrap();
        assert_eq!(enable.kind, OperationKind::EnableAgent);

        let delete = service
            .delete_agent(
                &actor(),
                &AgentCommandRequest {
                    metadata: metadata("complete-delete"),
                    agent_id,
                },
            )
            .unwrap();
        assert_eq!(delete.kind, OperationKind::DeleteAgent);
        let retained = service.get_agent(agent_id).unwrap();
        assert_eq!(retained.desired_state, DesiredAgentState::Deleted);
        assert_eq!(retained.purge_after, Some(150));
        assert!(service
            .list_agents(&ListAgentsRequest {
                community_config_id: Some(community.id),
                include_deleted: false,
            })
            .unwrap()
            .is_empty());
        assert_eq!(
            service
                .list_agents(&ListAgentsRequest {
                    community_config_id: Some(community.id),
                    include_deleted: true,
                })
                .unwrap()
                .len(),
            1
        );
        assert!(service.expired_retained_agents().unwrap().is_empty());
        assert_eq!(store.expired_retained_agents(150).unwrap(), vec![agent_id]);
        service
            .change_agent_state(
                &actor(),
                &ChangeAgentStateRequest {
                    metadata: metadata("recover-enable"),
                    agent_id,
                    desired_state: DesiredAgentState::Enabled,
                },
            )
            .unwrap();
        assert_eq!(service.get_agent(agent_id).unwrap().purge_after, None);
        assert!(store.expired_retained_agents(150).unwrap().is_empty());
        service
            .delete_agent(
                &actor(),
                &AgentCommandRequest {
                    metadata: metadata("complete-delete-again"),
                    agent_id,
                },
            )
            .unwrap();
        assert_eq!(store.expired_retained_agents(150).unwrap(), vec![agent_id]);
        assert_eq!(service.get_agent(agent_id).unwrap().purge_after, Some(150));
        service
            .delete_agent(
                &actor(),
                &AgentCommandRequest {
                    metadata: metadata("repeat-delete-preserves-deadline"),
                    agent_id,
                },
            )
            .unwrap();
        assert_eq!(service.get_agent(agent_id).unwrap().purge_after, Some(150));

        let audits = store
            .audit_for_subject("agent", &agent_id.to_string())
            .unwrap();
        assert!(audits.iter().all(|record| matches!(
            record.actor_principal.as_str(),
            "unix_uid:1000" | "system:reconciler"
        )));
        assert!(audits
            .iter()
            .any(|record| record.actor_principal == "unix_uid:1000"));
        assert!(audits
            .iter()
            .any(|record| record.actor_principal == "system:reconciler"));
        assert!(audits
            .iter()
            .all(|record| !record.actor_principal.contains("pid")));
    }
}
