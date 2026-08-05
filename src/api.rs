//! Transport-neutral lifecycle API contracts and authorization handlers.
//!
//! HTTP/TLS, Unix socket listeners, and NIP-98 verification are adapters outside this module.

use serde::{Deserialize, Serialize};

use crate::{
    auth::{authorize, AuthenticatedPrincipal, AuthorizationError, Capability, PrincipalOwnership},
    AgentId, CommunityConfigId, DesiredAgentState, ErrorCode, OperationId, OperationKind,
    OperationStatus, RuntimeId, StorageError, ValidationError,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandMetadata {
    pub idempotency_key: String,
    pub correlation_id: String,
}

impl CommandMetadata {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_token("idempotency_key", &self.idempotency_key, 200)?;
        validate_token("correlation_id", &self.correlation_id, 200)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateAgentInput {
    pub community_config_id: CommunityConfigId,
    pub display_name: String,
    pub system_prompt: String,
    pub runtime_id: RuntimeId,
}

impl CreateAgentInput {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_token("display_name", &self.display_name, 120)?;
        validate_token("system_prompt", &self.system_prompt, 65_536)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateAgentRequest {
    pub metadata: CommandMetadata,
    pub agent: CreateAgentInput,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpdateAgentInput {
    pub display_name: Option<String>,
    pub system_prompt: Option<String>,
    pub runtime_id: Option<RuntimeId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpdateAgentRequest {
    pub metadata: CommandMetadata,
    pub agent_id: AgentId,
    pub changes: UpdateAgentInput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChangeAgentStateRequest {
    pub metadata: CommandMetadata,
    pub agent_id: AgentId,
    pub desired_state: DesiredAgentState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentCommandRequest {
    pub metadata: CommandMetadata,
    pub agent_id: AgentId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SubmitDraftRequest {
    pub metadata: CommandMetadata,
    pub agent: CreateAgentInput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromoteDraftRequest {
    pub metadata: CommandMetadata,
    pub draft_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentResource {
    pub id: AgentId,
    pub community_config_id: CommunityConfigId,
    pub display_name: String,
    pub system_prompt: String,
    pub runtime_id: RuntimeId,
    pub desired_state: DesiredAgentState,
    pub purge_after: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DraftResource {
    pub id: String,
    pub owner: PrincipalOwnership,
    pub agent: CreateAgentInput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationResource {
    pub id: OperationId,
    pub kind: OperationKind,
    pub status: OperationStatus,
    pub agent_id: Option<AgentId>,
    pub correlation_id: String,
    pub error_code: Option<ErrorCode>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "route", content = "request", rename_all = "snake_case")]
pub enum LifecycleRouteRequest {
    CreateAgent(CreateAgentRequest),
    UpdateAgent(UpdateAgentRequest),
    ChangeAgentState(ChangeAgentStateRequest),
    DeleteAgent(AgentCommandRequest),
    PurgeAgent(AgentCommandRequest),
    GetAgent { agent_id: AgentId },
    ListAgents(ListAgentsRequest),
    AgentLogs(AgentLogsRequest),
    GetOperation { operation_id: OperationId },
    SubmitDraft(SubmitDraftRequest),
    GetDraft { draft_id: String },
    PromoteDraft(PromoteDraftRequest),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "resource", content = "value", rename_all = "snake_case")]
pub enum LifecycleRouteResource {
    Agent(AgentResource),
    Agents(Vec<AgentResource>),
    Logs(AgentLogsResource),
    Operation(OperationResource),
    Draft(DraftResource),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListAgentsRequest {
    pub community_config_id: Option<CommunityConfigId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentLogsRequest {
    pub agent_id: AgentId,
    pub after_cursor: Option<String>,
    pub limit: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RedactedLogEntry {
    pub cursor: String,
    pub occurred_at: i64,
    pub stream: String,
    /// Log text after the application service applies its redaction policy.
    pub redacted_message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentLogsResource {
    pub entries: Vec<RedactedLogEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    #[error("resource not found")]
    NotFound,
    #[error("resource conflict")]
    Conflict,
    #[error("request is invalid: {0}")]
    Invalid(ValidationError),
    #[error("operation is unsupported")]
    Unsupported,
    #[error("application service failed")]
    Internal,
}

impl From<StorageError> for ApplicationError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::NotFound => Self::NotFound,
            StorageError::Conflict(_) => Self::Conflict,
            StorageError::InvalidData(_)
            | StorageError::Database(_)
            | StorageError::LockPoisoned => Self::Internal,
        }
    }
}

pub trait LifecycleApplication {
    fn create_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        metadata: &CommandMetadata,
        input: &CreateAgentInput,
    ) -> Result<OperationResource, ApplicationError>;
    fn update_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &UpdateAgentRequest,
    ) -> Result<OperationResource, ApplicationError>;
    fn change_agent_state(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &ChangeAgentStateRequest,
    ) -> Result<OperationResource, ApplicationError>;
    fn delete_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentCommandRequest,
    ) -> Result<OperationResource, ApplicationError>;
    fn purge_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentCommandRequest,
    ) -> Result<OperationResource, ApplicationError>;
    fn get_agent(&self, id: AgentId) -> Result<AgentResource, ApplicationError>;
    fn list_agents(
        &self,
        request: &ListAgentsRequest,
    ) -> Result<Vec<AgentResource>, ApplicationError>;
    fn agent_logs(&self, request: &AgentLogsRequest)
        -> Result<AgentLogsResource, ApplicationError>;
    fn get_operation(&self, id: OperationId) -> Result<OperationResource, ApplicationError>;
    fn submit_draft(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &SubmitDraftRequest,
    ) -> Result<DraftResource, ApplicationError>;
    fn get_draft(&self, id: &str) -> Result<DraftResource, ApplicationError>;
    fn promote_draft(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &PromoteDraftRequest,
    ) -> Result<OperationResource, ApplicationError>;
}

pub struct LifecycleHandler<S> {
    application: S,
}

impl<S: LifecycleApplication> LifecycleHandler<S> {
    #[must_use]
    pub const fn new(application: S) -> Self {
        Self { application }
    }

    pub fn create_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &CreateAgentRequest,
    ) -> Result<OperationResource, crate::ApiError> {
        authorize(actor, Capability::CreateAgent, None).map_err(api_authorization)?;
        request.metadata.validate().map_err(api_validation)?;
        request.agent.validate().map_err(api_validation)?;
        self.application
            .create_agent(actor, &request.metadata, &request.agent)
            .map_err(api_application)
    }

    pub fn update_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &UpdateAgentRequest,
    ) -> Result<OperationResource, crate::ApiError> {
        authorize(actor, Capability::UpdateAgent, None).map_err(api_authorization)?;
        request.metadata.validate().map_err(api_validation)?;
        if request.changes == UpdateAgentInput::default() {
            return Err(api_validation(ValidationError::new(
                "changes",
                "must include at least one field",
            )));
        }
        self.application
            .update_agent(actor, request)
            .map_err(api_application)
    }

    pub fn change_agent_state(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &ChangeAgentStateRequest,
    ) -> Result<OperationResource, crate::ApiError> {
        authorize(actor, Capability::ChangeAgentState, None).map_err(api_authorization)?;
        request.metadata.validate().map_err(api_validation)?;
        if request.desired_state == DesiredAgentState::Deleted {
            return Err(api_validation(ValidationError::new(
                "desired_state",
                "deleted state requires the delete operation",
            )));
        }
        self.application
            .change_agent_state(actor, request)
            .map_err(api_application)
    }

    pub fn delete_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentCommandRequest,
    ) -> Result<OperationResource, crate::ApiError> {
        self.agent_command(actor, request, Capability::DeleteAgent, false)
    }

    pub fn purge_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentCommandRequest,
    ) -> Result<OperationResource, crate::ApiError> {
        self.agent_command(actor, request, Capability::PurgeAgent, true)
    }

    pub fn get_agent(
        &self,
        actor: &AuthenticatedPrincipal,
        id: AgentId,
    ) -> Result<AgentResource, crate::ApiError> {
        authorize(actor, Capability::ReadAgent, None).map_err(api_authorization)?;
        self.application.get_agent(id).map_err(api_application)
    }

    pub fn list_agents(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &ListAgentsRequest,
    ) -> Result<Vec<AgentResource>, crate::ApiError> {
        authorize(actor, Capability::ReadAgent, None).map_err(api_authorization)?;
        self.application
            .list_agents(request)
            .map_err(api_application)
    }

    pub fn agent_logs(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentLogsRequest,
    ) -> Result<AgentLogsResource, crate::ApiError> {
        authorize(actor, Capability::ReadAgent, None).map_err(api_authorization)?;
        if request.limit == 0 || request.limit > 1_000 {
            return Err(api_validation(ValidationError::new(
                "limit",
                "must be between 1 and 1000",
            )));
        }
        self.application
            .agent_logs(request)
            .map_err(api_application)
    }

    pub fn get_operation(
        &self,
        actor: &AuthenticatedPrincipal,
        id: OperationId,
    ) -> Result<OperationResource, crate::ApiError> {
        authorize(actor, Capability::ReadAgent, None).map_err(api_authorization)?;
        self.application.get_operation(id).map_err(api_application)
    }

    pub fn submit_draft(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &SubmitDraftRequest,
    ) -> Result<DraftResource, crate::ApiError> {
        authorize(actor, Capability::SubmitDraft, None).map_err(api_authorization)?;
        request.metadata.validate().map_err(api_validation)?;
        request.agent.validate().map_err(api_validation)?;
        self.application
            .submit_draft(actor, request)
            .map_err(api_application)
    }

    pub fn get_draft(
        &self,
        actor: &AuthenticatedPrincipal,
        id: &str,
    ) -> Result<DraftResource, crate::ApiError> {
        let draft = self.application.get_draft(id).map_err(api_application)?;
        authorize(actor, Capability::ReadDraft, Some(&draft.owner)).map_err(api_authorization)?;
        Ok(draft)
    }

    /// Promotion deliberately enters the exact same application method as direct creation.
    pub fn promote_draft(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &PromoteDraftRequest,
    ) -> Result<OperationResource, crate::ApiError> {
        authorize(actor, Capability::PromoteDraft, None).map_err(api_authorization)?;
        request.metadata.validate().map_err(api_validation)?;
        validate_token("draft_id", &request.draft_id, 200).map_err(api_validation)?;
        self.application
            .promote_draft(actor, request)
            .map_err(api_application)
    }

    fn agent_command(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &AgentCommandRequest,
        capability: Capability,
        purge: bool,
    ) -> Result<OperationResource, crate::ApiError> {
        authorize(actor, capability, None).map_err(api_authorization)?;
        request.metadata.validate().map_err(api_validation)?;
        if purge {
            self.application.purge_agent(actor, request)
        } else {
            self.application.delete_agent(actor, request)
        }
        .map_err(api_application)
    }
}

fn api_validation(error: ValidationError) -> crate::ApiError {
    crate::ApiError::validation(error)
}

fn api_authorization(error: AuthorizationError) -> crate::ApiError {
    crate::ApiError {
        code: ErrorCode::Forbidden,
        message: match error {
            AuthorizationError::Forbidden => "operation is not permitted",
            AuthorizationError::NotOwner => "resource is not owned by the principal",
        }
        .into(),
        field: None,
    }
}

fn api_application(error: ApplicationError) -> crate::ApiError {
    let (code, message, field) = match error {
        ApplicationError::NotFound => (ErrorCode::NotFound, "resource not found", None),
        ApplicationError::Conflict => (ErrorCode::Conflict, "resource conflict", None),
        ApplicationError::Invalid(error) => {
            return crate::ApiError::validation(error);
        }
        ApplicationError::Unsupported => (ErrorCode::Unsupported, "operation is unsupported", None),
        ApplicationError::Internal => (ErrorCode::Internal, "internal service error", None),
    };
    crate::ApiError {
        code,
        message: message.into(),
        field,
    }
}

fn validate_token(
    field: &'static str,
    value: &str,
    max_chars: usize,
) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(field, "must not be empty"));
    }
    if value.chars().count() > max_chars {
        return Err(ValidationError::new(
            field,
            format!("must be at most {max_chars} characters"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::auth::{Authority, Principal};

    struct FakeApplication {
        draft: DraftResource,
        create_calls: Mutex<Vec<CreateAgentInput>>,
    }

    impl FakeApplication {
        fn operation(metadata: &CommandMetadata, kind: OperationKind) -> OperationResource {
            OperationResource {
                id: OperationId::new(),
                kind,
                status: OperationStatus::Pending,
                agent_id: None,
                correlation_id: metadata.correlation_id.clone(),
                error_code: None,
                created_at: 1,
                updated_at: 1,
            }
        }
    }

    impl LifecycleApplication for FakeApplication {
        fn create_agent(
            &self,
            _: &AuthenticatedPrincipal,
            metadata: &CommandMetadata,
            input: &CreateAgentInput,
        ) -> Result<OperationResource, ApplicationError> {
            self.create_calls.lock().unwrap().push(input.clone());
            Ok(Self::operation(metadata, OperationKind::CreateAgent))
        }
        fn update_agent(
            &self,
            _: &AuthenticatedPrincipal,
            request: &UpdateAgentRequest,
        ) -> Result<OperationResource, ApplicationError> {
            Ok(Self::operation(
                &request.metadata,
                OperationKind::UpdateAgent,
            ))
        }
        fn change_agent_state(
            &self,
            _: &AuthenticatedPrincipal,
            request: &ChangeAgentStateRequest,
        ) -> Result<OperationResource, ApplicationError> {
            Ok(Self::operation(
                &request.metadata,
                OperationKind::EnableAgent,
            ))
        }
        fn delete_agent(
            &self,
            _: &AuthenticatedPrincipal,
            request: &AgentCommandRequest,
        ) -> Result<OperationResource, ApplicationError> {
            Ok(Self::operation(
                &request.metadata,
                OperationKind::DeleteAgent,
            ))
        }
        fn purge_agent(
            &self,
            _: &AuthenticatedPrincipal,
            request: &AgentCommandRequest,
        ) -> Result<OperationResource, ApplicationError> {
            Ok(Self::operation(
                &request.metadata,
                OperationKind::PurgeAgent,
            ))
        }
        fn get_agent(&self, _: AgentId) -> Result<AgentResource, ApplicationError> {
            Err(ApplicationError::NotFound)
        }
        fn list_agents(
            &self,
            _: &ListAgentsRequest,
        ) -> Result<Vec<AgentResource>, ApplicationError> {
            Ok(Vec::new())
        }
        fn agent_logs(&self, _: &AgentLogsRequest) -> Result<AgentLogsResource, ApplicationError> {
            Ok(AgentLogsResource {
                entries: Vec::new(),
                next_cursor: None,
            })
        }
        fn get_operation(&self, _: OperationId) -> Result<OperationResource, ApplicationError> {
            Err(ApplicationError::NotFound)
        }
        fn submit_draft(
            &self,
            _: &AuthenticatedPrincipal,
            _: &SubmitDraftRequest,
        ) -> Result<DraftResource, ApplicationError> {
            Ok(self.draft.clone())
        }
        fn get_draft(&self, _: &str) -> Result<DraftResource, ApplicationError> {
            Ok(self.draft.clone())
        }
        fn promote_draft(
            &self,
            actor: &AuthenticatedPrincipal,
            request: &PromoteDraftRequest,
        ) -> Result<OperationResource, ApplicationError> {
            self.create_agent(actor, &request.metadata, &self.draft.agent)
        }
    }

    fn actor(authority: Authority, uid: u32) -> AuthenticatedPrincipal {
        AuthenticatedPrincipal {
            principal: Principal::UnixPeer {
                uid,
                gid: uid,
                pid: None,
            },
            authority,
        }
    }

    fn metadata() -> CommandMetadata {
        CommandMetadata {
            idempotency_key: "idem-1".into(),
            correlation_id: "corr-1".into(),
        }
    }

    fn input() -> CreateAgentInput {
        CreateAgentInput {
            community_config_id: CommunityConfigId::new(),
            display_name: "Builder".into(),
            system_prompt: "Build safely.".into(),
            runtime_id: "codex-acp".parse().unwrap(),
        }
    }

    fn make_handler(owner: PrincipalOwnership) -> LifecycleHandler<FakeApplication> {
        LifecycleHandler::new(FakeApplication {
            draft: DraftResource {
                id: "draft-1".into(),
                owner,
                agent: input(),
            },
            create_calls: Mutex::new(Vec::new()),
        })
    }

    #[test]
    fn promotion_uses_the_same_direct_create_application_path() {
        let administrator = actor(Authority::Administrator, 1);
        let handler = make_handler(PrincipalOwnership::UnixUid { uid: 2 });
        let request = PromoteDraftRequest {
            metadata: metadata(),
            draft_id: "draft-1".into(),
        };
        let operation = handler.promote_draft(&administrator, &request).unwrap();
        assert_eq!(operation.kind, OperationKind::CreateAgent);
        assert_eq!(
            handler.application.create_calls.lock().unwrap().as_slice(),
            std::slice::from_ref(&handler.application.draft.agent)
        );
    }

    #[test]
    fn ownership_and_authority_are_enforced_by_handlers() {
        let submitter = actor(Authority::DraftSubmitter, 2);
        let handler = make_handler(PrincipalOwnership::UnixUid { uid: 2 });
        assert!(handler.get_draft(&submitter, "draft-1").is_ok());
        assert_eq!(
            handler
                .create_agent(
                    &submitter,
                    &CreateAgentRequest {
                        metadata: metadata(),
                        agent: input()
                    }
                )
                .unwrap_err()
                .code,
            ErrorCode::Forbidden
        );
        let other = make_handler(PrincipalOwnership::UnixUid { uid: 3 });
        assert_eq!(
            other.get_draft(&submitter, "draft-1").unwrap_err().code,
            ErrorCode::Forbidden
        );
    }

    #[test]
    fn mutation_metadata_is_required_and_has_stable_error_shape() {
        let administrator = actor(Authority::Administrator, 1);
        let handler = make_handler(PrincipalOwnership::UnixUid { uid: 2 });
        let request = CreateAgentRequest {
            metadata: CommandMetadata {
                idempotency_key: String::new(),
                correlation_id: "corr-1".into(),
            },
            agent: input(),
        };
        let error = handler.create_agent(&administrator, &request).unwrap_err();
        let wire = serde_json::to_value(&error).unwrap();
        assert_eq!(wire["code"], "invalid_request");
        assert_eq!(wire["field"], "idempotency_key");
        assert_eq!(wire["message"], "must not be empty");
    }

    #[test]
    fn lifecycle_wire_contract_has_no_secret_or_raw_environment_fields() {
        let value = serde_json::to_value(CreateAgentRequest {
            metadata: metadata(),
            agent: input(),
        })
        .unwrap();
        let object = value["agent"].as_object().unwrap();
        assert!(!object.contains_key("environment"));
        assert!(!object.contains_key("env"));
        assert!(!object.contains_key("private_key"));
        assert!(!object.contains_key("secret"));
        let internal = api_application(ApplicationError::from(StorageError::InvalidData(
            "private_key=leak".into(),
        )));
        assert!(!serde_json::to_string(&internal)
            .unwrap()
            .contains("private_key"));
        assert!(!serde_json::to_string(&internal).unwrap().contains("leak"));
    }

    #[test]
    fn deleted_state_cannot_bypass_the_delete_capability() {
        let administrator = actor(Authority::Administrator, 1);
        let handler = make_handler(PrincipalOwnership::UnixUid { uid: 2 });
        let request = ChangeAgentStateRequest {
            metadata: metadata(),
            agent_id: AgentId::new(),
            desired_state: DesiredAgentState::Deleted,
        };
        let error = handler
            .change_agent_state(&administrator, &request)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(error.field.as_deref(), Some("desired_state"));
    }
}
