//! Transport-neutral lifecycle API contracts and authorization handlers.
//!
//! HTTP/TLS, Unix socket listeners, and NIP-98 verification are adapters outside this module.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{
    auth::{authorize, AuthenticatedPrincipal, AuthorizationError, Capability, PrincipalOwnership},
    AgentId, CommunityConfig, CommunityConfigId, DesiredAgentState, ErrorCode, OperationId,
    OperationKind, OperationStatus, PersonaDefinition, RuntimeId, StorageError, ValidationError,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JoinCommunityRequest {
    pub display_name: String,
    pub relay_url: url::Url,
    pub identity_pubkey: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpdateCommunityRequest {
    pub community_id: CommunityConfigId,
    pub display_name: String,
}

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
pub struct CreatePersonaRequest {
    pub display_name: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub runtime: Option<RuntimeId>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpdatePersonaInput {
    pub display_name: Option<String>,
    pub system_prompt: Option<String>,
    pub runtime: Option<RuntimeId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpdatePersonaRequest {
    pub persona_id: String,
    pub changes: UpdatePersonaInput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateAgentInput {
    pub community_config_id: CommunityConfigId,
    pub display_name: String,
    #[serde(default)]
    pub persona_id: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub system_prompt_file: Option<String>,
    #[serde(default)]
    pub runtime_id: Option<RuntimeId>,
    #[serde(default)]
    pub filesystem_user: Option<String>,
}

impl CreateAgentInput {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_token("display_name", &self.display_name, 120)?;
        if let Some(prompt) = self.system_prompt.as_deref() {
            if prompt.chars().count() > 65_536 || prompt.contains('\0') {
                return Err(ValidationError::new(
                    "system_prompt",
                    "must be at most 65536 NUL-free characters",
                ));
            }
        }
        if let Some(path) = self.system_prompt_file.as_deref() {
            validate_token("system_prompt_file", path, 4_096)?;
            if !std::path::Path::new(path).is_absolute() {
                return Err(ValidationError::new(
                    "system_prompt_file",
                    "must be an absolute path",
                ));
            }
        }
        if self.persona_id.is_some() && self.system_prompt.is_some() {
            return Err(ValidationError::new(
                "system_prompt",
                "is defined by the selected persona; omit --system-prompt",
            ));
        }
        if self.persona_id.is_some() && self.system_prompt_file.is_some() {
            return Err(ValidationError::new(
                "system_prompt_file",
                "is defined by the selected persona; omit --system-prompt-file",
            ));
        }
        if self.persona_id.is_none() && self.runtime_id.is_none() {
            return Err(ValidationError::new(
                "runtime_id",
                "is required when no persona is selected",
            ));
        }
        Ok(())
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
    #[serde(default)]
    pub system_prompt_file: Option<String>,
    pub runtime_id: Option<RuntimeId>,
    pub filesystem_user: Option<String>,
}

impl UpdateAgentInput {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(value) = self.display_name.as_deref() {
            validate_token("display_name", value, 120)?;
        }
        if let Some(value) = self.system_prompt.as_deref() {
            if value.chars().count() > 65_536 || value.contains('\0') {
                return Err(ValidationError::new(
                    "system_prompt",
                    "must be at most 65536 NUL-free characters",
                ));
            }
        }
        if let Some(value) = self.system_prompt_file.as_deref() {
            validate_token("system_prompt_file", value, 4_096)?;
            if !std::path::Path::new(value).is_absolute() {
                return Err(ValidationError::new(
                    "system_prompt_file",
                    "must be an absolute path",
                ));
            }
        }
        Ok(())
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
    pub runtime_id: RuntimeId,
    pub desired_state: DesiredAgentState,
    pub purge_after: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
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
    JoinCommunity(JoinCommunityRequest),
    UpdateCommunity(UpdateCommunityRequest),
    GetCommunity { community_id: CommunityConfigId },
    ListCommunities,
    RemoveCommunity { community_id: CommunityConfigId },
    CreatePersona(CreatePersonaRequest),
    UpdatePersona(UpdatePersonaRequest),
    GetPersona { persona_id: String },
    ListPersonas,
    DeletePersona { persona_id: String },
    CreateAgent(CreateAgentRequest),
    UpdateAgent(UpdateAgentRequest),
    ChangeAgentState(ChangeAgentStateRequest),
    DeleteAgent(AgentCommandRequest),
    PurgeAgent(AgentCommandRequest),
    GetAgent { agent_id: AgentId },
    ListAgents(ListAgentsRequest),
    AgentLogs(AgentLogsRequest),
    GetOperation { operation_id: OperationId },
    AwaitOperation { operation_id: OperationId },
    SubmitDraft(SubmitDraftRequest),
    GetDraft { draft_id: String },
    PromoteDraft(PromoteDraftRequest),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "resource", content = "value", rename_all = "snake_case")]
pub enum LifecycleRouteResource {
    Community(CommunityConfig),
    Communities(Vec<CommunityConfig>),
    Persona(PersonaDefinition),
    Personas(Vec<PersonaDefinition>),
    Agent(AgentResource),
    Agents(Vec<AgentResource>),
    Logs(AgentLogsResource),
    Operation(OperationResource),
    Draft(DraftResource),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListAgentsRequest {
    pub community_config_id: Option<CommunityConfigId>,
    #[serde(default)]
    pub include_deleted: bool,
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
    #[error("request is forbidden: {0}")]
    Forbidden(String),
    #[error("service unavailable: {0}")]
    Unavailable(String),
    #[error("resource conflict: {0}")]
    Conflict(String),
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
            StorageError::Conflict(message) => Self::Conflict(message),
            StorageError::InvalidData(_)
            | StorageError::Database(_)
            | StorageError::LockPoisoned => Self::Internal,
        }
    }
}

pub trait LifecycleApplication {
    fn join_community(
        &self,
        request: &JoinCommunityRequest,
    ) -> Result<CommunityConfig, ApplicationError>;
    fn update_community(
        &self,
        request: &UpdateCommunityRequest,
    ) -> Result<CommunityConfig, ApplicationError>;
    fn get_community(&self, id: CommunityConfigId) -> Result<CommunityConfig, ApplicationError>;
    fn list_communities(&self) -> Result<Vec<CommunityConfig>, ApplicationError>;
    fn remove_community(&self, id: CommunityConfigId) -> Result<CommunityConfig, ApplicationError>;
    fn create_persona(
        &self,
        _request: &CreatePersonaRequest,
    ) -> Result<PersonaDefinition, ApplicationError> {
        Err(ApplicationError::Unsupported)
    }
    fn update_persona(
        &self,
        _request: &UpdatePersonaRequest,
    ) -> Result<PersonaDefinition, ApplicationError> {
        Err(ApplicationError::Unsupported)
    }
    fn get_persona(&self, _id: &str) -> Result<PersonaDefinition, ApplicationError> {
        Err(ApplicationError::Unsupported)
    }
    fn list_personas(&self) -> Result<Vec<PersonaDefinition>, ApplicationError> {
        Err(ApplicationError::Unsupported)
    }
    fn delete_persona(&self, _id: &str) -> Result<PersonaDefinition, ApplicationError> {
        Err(ApplicationError::Unsupported)
    }
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
    fn wait_operation(
        &self,
        id: OperationId,
        _timeout: Duration,
    ) -> Result<OperationResource, ApplicationError> {
        self.get_operation(id)
    }
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

    pub fn join_community(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &JoinCommunityRequest,
    ) -> Result<CommunityConfig, crate::ApiError> {
        authorize(actor, Capability::ManageCommunity, None).map_err(api_authorization)?;
        let candidate =
            CommunityConfig::new(request.display_name.clone(), request.relay_url.clone())
                .and_then(|community| {
                    community.with_identity_pubkey(request.identity_pubkey.clone())
                })
                .map_err(api_validation)?;
        candidate.validate().map_err(api_validation)?;
        self.application
            .join_community(request)
            .map_err(api_application)
    }

    pub fn update_community(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &UpdateCommunityRequest,
    ) -> Result<CommunityConfig, crate::ApiError> {
        authorize(actor, Capability::ManageCommunity, None).map_err(api_authorization)?;
        validate_token("display_name", &request.display_name, 120).map_err(api_validation)?;
        self.application
            .update_community(request)
            .map_err(api_application)
    }

    pub fn get_community(
        &self,
        actor: &AuthenticatedPrincipal,
        id: CommunityConfigId,
    ) -> Result<CommunityConfig, crate::ApiError> {
        authorize(actor, Capability::ReadCommunity, None).map_err(api_authorization)?;
        self.application.get_community(id).map_err(api_application)
    }

    pub fn list_communities(
        &self,
        actor: &AuthenticatedPrincipal,
    ) -> Result<Vec<CommunityConfig>, crate::ApiError> {
        authorize(actor, Capability::ReadCommunity, None).map_err(api_authorization)?;
        self.application.list_communities().map_err(api_application)
    }

    pub fn remove_community(
        &self,
        actor: &AuthenticatedPrincipal,
        id: CommunityConfigId,
    ) -> Result<CommunityConfig, crate::ApiError> {
        authorize(actor, Capability::ManageCommunity, None).map_err(api_authorization)?;
        self.application
            .remove_community(id)
            .map_err(api_application)
    }

    pub fn create_persona(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &CreatePersonaRequest,
    ) -> Result<PersonaDefinition, crate::ApiError> {
        authorize(actor, Capability::CreateAgent, None).map_err(api_authorization)?;
        validate_token("display_name", &request.display_name, 120).map_err(api_validation)?;
        self.application
            .create_persona(request)
            .map_err(api_application)
    }

    pub fn update_persona(
        &self,
        actor: &AuthenticatedPrincipal,
        request: &UpdatePersonaRequest,
    ) -> Result<PersonaDefinition, crate::ApiError> {
        authorize(actor, Capability::UpdateAgent, None).map_err(api_authorization)?;
        validate_token("persona_id", &request.persona_id, 120).map_err(api_validation)?;
        if request.changes == UpdatePersonaInput::default() {
            return Err(api_validation(ValidationError::new(
                "changes",
                "must include at least one field",
            )));
        }
        self.application
            .update_persona(request)
            .map_err(api_application)
    }

    pub fn get_persona(
        &self,
        actor: &AuthenticatedPrincipal,
        id: &str,
    ) -> Result<PersonaDefinition, crate::ApiError> {
        authorize(actor, Capability::ReadAgent, None).map_err(api_authorization)?;
        self.application.get_persona(id).map_err(api_application)
    }

    pub fn list_personas(
        &self,
        actor: &AuthenticatedPrincipal,
    ) -> Result<Vec<PersonaDefinition>, crate::ApiError> {
        authorize(actor, Capability::ReadAgent, None).map_err(api_authorization)?;
        self.application.list_personas().map_err(api_application)
    }

    pub fn delete_persona(
        &self,
        actor: &AuthenticatedPrincipal,
        id: &str,
    ) -> Result<PersonaDefinition, crate::ApiError> {
        authorize(actor, Capability::DeleteAgent, None).map_err(api_authorization)?;
        self.application.delete_persona(id).map_err(api_application)
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
        request.changes.validate().map_err(api_validation)?;
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

    pub fn await_operation(
        &self,
        actor: &AuthenticatedPrincipal,
        id: OperationId,
    ) -> Result<OperationResource, crate::ApiError> {
        authorize(actor, Capability::ReadAgent, None).map_err(api_authorization)?;
        self.application
            .wait_operation(id, Duration::from_secs(120))
            .map_err(api_application)
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
        ApplicationError::NotFound => (ErrorCode::NotFound, "resource not found".to_owned(), None),
        ApplicationError::Forbidden(message) => (ErrorCode::Forbidden, message, None),
        ApplicationError::Unavailable(message) => (ErrorCode::Unavailable, message, None),
        ApplicationError::Conflict(message) => (ErrorCode::Conflict, message, None),
        ApplicationError::Invalid(error) => {
            return crate::ApiError::validation(error);
        }
        ApplicationError::Unsupported => (
            ErrorCode::Unsupported,
            "operation is unsupported".to_owned(),
            None,
        ),
        ApplicationError::Internal => (
            ErrorCode::Internal,
            "internal service error".to_owned(),
            None,
        ),
    };
    crate::ApiError {
        code,
        message,
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
        fn join_community(
            &self,
            _: &JoinCommunityRequest,
        ) -> Result<CommunityConfig, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn update_community(
            &self,
            _: &UpdateCommunityRequest,
        ) -> Result<CommunityConfig, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn get_community(&self, _: CommunityConfigId) -> Result<CommunityConfig, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn list_communities(&self) -> Result<Vec<CommunityConfig>, ApplicationError> {
            Ok(Vec::new())
        }
        fn remove_community(
            &self,
            _: CommunityConfigId,
        ) -> Result<CommunityConfig, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }

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
            persona_id: None,
            system_prompt: Some("Build safely.".into()),
            system_prompt_file: None,
            runtime_id: Some("codex-acp".parse().unwrap()),
            filesystem_user: None,
        }
    }

    #[test]
    fn persona_backed_create_rejects_agent_system_prompt() {
        let mut input = input();
        input.persona_id = Some("reviewer".into());
        let error = input.validate().unwrap_err();
        assert_eq!(error.field, "system_prompt");
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
