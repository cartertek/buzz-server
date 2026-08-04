//! SQLite-backed lifecycle application service.

use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::{
    api::{
        AgentCommandRequest, AgentLogsRequest, AgentLogsResource, AgentResource, ApplicationError,
        ChangeAgentStateRequest, CommandMetadata, CreateAgentInput, DraftResource,
        LifecycleApplication, ListAgentsRequest, OperationResource, SubmitDraftRequest,
        UpdateAgentRequest,
    },
    auth::{AuthenticatedPrincipal, Principal},
    storage::{AgentCommandMutation, DurableOperation, IdempotencyRecord, NewAuditRecord},
    AgentId, AgentSpec, DesiredAgentState, OperationId, OperationKind, OperationStatus,
    RuntimeSpec, SqliteStore, StorageError,
};

pub const DEFAULT_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

pub trait LifecycleEffects: Send + Sync {
    /// Wakes the reconciler after the complete durable command transaction commits.
    fn operation_ready(&self, operation: &DurableOperation) -> Result<(), ApplicationError>;
}

#[derive(Clone)]
pub struct SqliteLifecycleApplication<E> {
    store: Arc<SqliteStore>,
    effects: Arc<E>,
    now: fn() -> i64,
    retention_seconds: i64,
}

struct ExecutionPlan<'a> {
    kind: OperationKind,
    agent_id: AgentId,
    community_id: crate::CommunityConfigId,
    mutation: AgentCommandMutation<'a>,
    promoted_draft_id: Option<&'a str>,
}

impl<E: LifecycleEffects> SqliteLifecycleApplication<E> {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>, effects: Arc<E>, now: fn() -> i64) -> Self {
        Self {
            store,
            effects,
            now,
            retention_seconds: DEFAULT_RETENTION_SECONDS,
        }
    }

    #[must_use]
    pub const fn with_retention_seconds(mut self, retention_seconds: i64) -> Self {
        self.retention_seconds = retention_seconds;
        self
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
        let (operation, replayed) = self.store.apply_agent_command(
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
        if !replayed {
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

    /// Persists a terminal reconciliation result and only then performs successful purge cleanup.
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
        if status == OperationStatus::Succeeded && operation.kind == OperationKind::PurgeAgent {
            if let Some(agent_id) = operation.agent_id {
                if !self.store.finalize_purge(agent_id, id)? {
                    return Err(ApplicationError::Conflict);
                }
            }
        }
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
    fn create_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        metadata: &CommandMetadata,
        input: &CreateAgentInput,
    ) -> Result<OperationResource, ApplicationError> {
        let agent = AgentSpec {
            id: AgentId::new(),
            community_config_id: input.community_config_id,
            display_name: input.display_name.clone(),
            system_prompt: input.system_prompt.clone(),
            runtime: RuntimeSpec {
                runtime_id: input.runtime_id.clone(),
                environment: Default::default(),
            },
            desired_state: DesiredAgentState::Enabled,
        };
        agent.validate().map_err(ApplicationError::Invalid)?;
        self.execute(
            actor,
            metadata,
            input,
            ExecutionPlan {
                kind: OperationKind::CreateAgent,
                agent_id: agent.id,
                community_id: agent.community_config_id,
                mutation: AgentCommandMutation::Create(&agent),
                promoted_draft_id: None,
            },
        )
    }

    fn update_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &UpdateAgentRequest,
    ) -> Result<OperationResource, ApplicationError> {
        let community = self.existing_community(request.agent_id)?;
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
        // Purge first records Deleted intent. Physical removal is only `finalize_purge` after the
        // reconciler has stopped the process and transitioned this operation to Succeeded.
        self.deleted_command(actor, request, OperationKind::PurgeAgent)
    }

    fn get_agent(&self, id: AgentId) -> Result<AgentResource, ApplicationError> {
        let agent = self
            .store
            .get_agent(id)?
            .ok_or(ApplicationError::NotFound)?;
        Ok(agent_resource(
            agent,
            self.store
                .agent_retention(id)?
                .map(|value| value.purge_after),
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
            .map(|agent| {
                let purge_after = self
                    .store
                    .agent_retention(agent.id)?
                    .map(|value| value.purge_after);
                Ok(agent_resource(agent, purge_after))
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
        let agent = AgentSpec {
            id: AgentId::new(),
            community_config_id: draft.agent.community_config_id,
            display_name: draft.agent.display_name.clone(),
            system_prompt: draft.agent.system_prompt.clone(),
            runtime: RuntimeSpec {
                runtime_id: draft.agent.runtime_id.clone(),
                environment: Default::default(),
            },
            desired_state: DesiredAgentState::Enabled,
        };
        self.execute(
            actor,
            &request.metadata,
            request,
            ExecutionPlan {
                kind: OperationKind::CreateAgent,
                agent_id: agent.id,
                community_id: agent.community_config_id,
                mutation: AgentCommandMutation::Create(&agent),
                promoted_draft_id: Some(&request.draft_id),
            },
        )
    }
}

impl<E: LifecycleEffects> SqliteLifecycleApplication<E> {
    fn deleted_command(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentCommandRequest,
        kind: OperationKind,
    ) -> Result<OperationResource, ApplicationError> {
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
                    desired_state: DesiredAgentState::Deleted,
                    retention_deadline: (kind == OperationKind::DeleteAgent)
                        .then(|| (self.now)().saturating_add(self.retention_seconds.max(0))),
                },
                promoted_draft_id: None,
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

fn agent_resource(agent: AgentSpec, purge_after: Option<i64>) -> AgentResource {
    AgentResource {
        id: agent.id,
        community_config_id: agent.community_config_id,
        display_name: agent.display_name,
        system_prompt: agent.system_prompt,
        runtime_id: agent.runtime.runtime_id,
        desired_state: agent.desired_state,
        purge_after,
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
    fn create_replays_after_restart_without_duplicate_agent_operation_or_audit() {
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
                system_prompt: "Build safely.".into(),
                runtime_id: "codex-acp".parse().unwrap(),
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
        assert_eq!(effects.0.load(Ordering::SeqCst), 0);
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
                    system_prompt: "Build safely.".into(),
                    runtime_id: "codex-acp".parse().unwrap(),
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
        assert!(!store.finalize_purge(agent_id, purge.id).unwrap());
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
                        system_prompt: "Build safely.".into(),
                        runtime_id: "codex-acp".parse().unwrap(),
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
            Err(ApplicationError::Conflict)
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
                    system_prompt: "Build safely.".into(),
                    runtime_id: "codex-acp".parse().unwrap(),
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
                    community_config_id: Some(community.id)
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
        assert!(service.expired_retained_agents().unwrap().is_empty());
        assert_eq!(store.expired_retained_agents(150).unwrap(), vec![agent_id]);

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
