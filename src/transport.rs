//! Authenticated transport adapter seams for the lifecycle application.

use std::{
    io,
    net::SocketAddr,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::watch,
    task::JoinSet,
    time::{timeout, Duration},
};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::auth::{
    AuthenticatedPrincipal, AuthenticationError, Nip98AuthorityPolicy, ReplayGuard,
    UnixAuthorityPolicy, UnixPeerCredentials,
};
use crate::{
    api::{LifecycleApplication, LifecycleHandler, LifecycleRouteRequest, LifecycleRouteResource},
    ApiError, ErrorCode, SqliteStore,
};

pub const MAX_LIFECYCLE_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_LIFECYCLE_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_UNIX_CONNECTIONS: usize = 64;
const UNIX_IO_TIMEOUT: Duration = Duration::from_secs(10);

pub trait AuthenticatedRequestHandler: Send + Sync + 'static {
    /// Handles decoded transport content. Implementations own route and response serialization.
    fn handle(&self, actor: &AuthenticatedPrincipal, request: &[u8]) -> HandlerResponse;
}

pub struct HandlerResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub struct LifecycleJsonRouter<S>(LifecycleHandler<S>);

impl<S: LifecycleApplication> LifecycleJsonRouter<S> {
    #[must_use]
    pub const fn new(application: S) -> Self {
        Self(LifecycleHandler::new(application))
    }
}

#[derive(serde::Serialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
enum LifecycleWireResponse {
    Ok(LifecycleRouteResource),
    Error(ApiError),
}

impl<S: LifecycleApplication + Send + Sync + 'static> AuthenticatedRequestHandler
    for LifecycleJsonRouter<S>
{
    fn handle(&self, actor: &AuthenticatedPrincipal, bytes: &[u8]) -> HandlerResponse {
        let result = serde_json::from_slice::<LifecycleRouteRequest>(bytes)
            .map_err(|_| ApiError {
                code: ErrorCode::InvalidRequest,
                message: "request body is not a valid lifecycle route".into(),
                field: None,
            })
            .and_then(|request| match request {
                LifecycleRouteRequest::AddCommunity(request) => self
                    .0
                    .add_community(actor, &request)
                    .map(LifecycleRouteResource::Community),
                LifecycleRouteRequest::GetCommunity { community_id } => self
                    .0
                    .get_community(actor, community_id)
                    .map(LifecycleRouteResource::Community),
                LifecycleRouteRequest::ListCommunities => self
                    .0
                    .list_communities(actor)
                    .map(LifecycleRouteResource::Communities),
                LifecycleRouteRequest::RemoveCommunity { community_id } => self
                    .0
                    .remove_community(actor, community_id)
                    .map(LifecycleRouteResource::Community),
                LifecycleRouteRequest::CreateAgent(request) => self
                    .0
                    .create_agent(actor, &request)
                    .map(LifecycleRouteResource::Operation),
                LifecycleRouteRequest::UpdateAgent(request) => self
                    .0
                    .update_agent(actor, &request)
                    .map(LifecycleRouteResource::Operation),
                LifecycleRouteRequest::ChangeAgentState(request) => self
                    .0
                    .change_agent_state(actor, &request)
                    .map(LifecycleRouteResource::Operation),
                LifecycleRouteRequest::DeleteAgent(request) => self
                    .0
                    .delete_agent(actor, &request)
                    .map(LifecycleRouteResource::Operation),
                LifecycleRouteRequest::PurgeAgent(request) => self
                    .0
                    .purge_agent(actor, &request)
                    .map(LifecycleRouteResource::Operation),
                LifecycleRouteRequest::GetAgent { agent_id } => self
                    .0
                    .get_agent(actor, agent_id)
                    .map(LifecycleRouteResource::Agent),
                LifecycleRouteRequest::ListAgents(request) => self
                    .0
                    .list_agents(actor, &request)
                    .map(LifecycleRouteResource::Agents),
                LifecycleRouteRequest::AgentLogs(request) => self
                    .0
                    .agent_logs(actor, &request)
                    .map(LifecycleRouteResource::Logs),
                LifecycleRouteRequest::GetOperation { operation_id } => self
                    .0
                    .get_operation(actor, operation_id)
                    .map(LifecycleRouteResource::Operation),
                LifecycleRouteRequest::SubmitDraft(request) => self
                    .0
                    .submit_draft(actor, &request)
                    .map(LifecycleRouteResource::Draft),
                LifecycleRouteRequest::GetDraft { draft_id } => self
                    .0
                    .get_draft(actor, &draft_id)
                    .map(LifecycleRouteResource::Draft),
                LifecycleRouteRequest::PromoteDraft(request) => self
                    .0
                    .promote_draft(actor, &request)
                    .map(LifecycleRouteResource::Operation),
            });
        let (status, response) = match result {
            Ok(resource) => (200, LifecycleWireResponse::Ok(resource)),
            Err(error) => (api_status(error.code), LifecycleWireResponse::Error(error)),
        };
        HandlerResponse {
            status,
            body: serde_json::to_vec(&response).unwrap_or_else(|_| b"{\"status\":\"error\",\"value\":{\"code\":\"internal\",\"message\":\"response serialization failed\"}}".to_vec()),
        }
    }
}

const fn api_status(code: ErrorCode) -> u16 {
    match code {
        ErrorCode::InvalidRequest => 400,
        ErrorCode::Unauthorized => 401,
        ErrorCode::Forbidden => 403,
        ErrorCode::NotFound => 404,
        ErrorCode::Conflict => 409,
        ErrorCode::Unsupported => 501,
        ErrorCode::Internal => 500,
    }
}

pub struct UnixLifecycleServer<H> {
    socket_path: PathBuf,
    authority: UnixAuthorityPolicy,
    handler: Arc<H>,
}

impl<H: AuthenticatedRequestHandler> UnixLifecycleServer<H> {
    #[must_use]
    pub fn new(
        socket_path: impl Into<PathBuf>,
        authority: UnixAuthorityPolicy,
        handler: Arc<H>,
    ) -> Self {
        Self {
            socket_path: socket_path.into(),
            authority,
            handler,
        }
    }

    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<(), TransportError> {
        prepare_socket(&self.socket_path)?;
        let listener = UnixListener::bind(&self.socket_path)?;
        // Authorization is derived from SO_PEERCRED, not filesystem ownership. The socket must be
        // connectable by configured non-root draft submitters while its parent remains non-writable.
        std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o666))?;
        let _guard = SocketGuard(self.socket_path.clone());
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                accepted = listener.accept(), if connections.len() < MAX_UNIX_CONNECTIONS => {
                    let (stream, _) = accepted?;
                    let authority = self.authority.clone();
                    let handler = Arc::clone(&self.handler);
                    connections.spawn(async move {
                        timeout(UNIX_IO_TIMEOUT, serve_connection(stream, &authority, handler.as_ref())).await
                    });
                }
                completed = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = completed {
                        return Err(TransportError::Task(error.to_string()));
                    }
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        Ok(())
    }
}

async fn serve_connection<H: AuthenticatedRequestHandler>(
    mut stream: UnixStream,
    authority: &UnixAuthorityPolicy,
    handler: &H,
) -> Result<(), TransportError> {
    let credentials = stream.peer_cred()?;
    let actor = authority.authenticate(UnixPeerCredentials {
        uid: credentials.uid(),
        gid: credentials.gid(),
        pid: credentials.pid().and_then(|pid| u32::try_from(pid).ok()),
    })?;
    let request = read_frame(&mut stream).await?;
    let response = handler.handle(&actor, &request);
    if response.body.len() > MAX_LIFECYCLE_RESPONSE_BYTES {
        return Err(TransportError::ResponseTooLarge);
    }
    write_frame(&mut stream, &response.body).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Authentication seam used only after a TLS HTTP listener has validated framing and extracted
/// the NIP-98 event. TLS certificate/key loading remains a deployment adapter responsibility.
pub struct TlsNip98Authenticator<R> {
    pub authority: Nip98AuthorityPolicy,
    pub replay: R,
}

#[derive(Clone)]
pub struct SqliteReplayGuard {
    pub store: Arc<SqliteStore>,
    pub now: fn() -> u64,
}

impl ReplayGuard for SqliteReplayGuard {
    fn claim(&self, event_id: &str, expires_at: u64) -> bool {
        self.store
            .claim_nip98_replay(event_id, expires_at, (self.now)())
            .unwrap_or(false)
    }
}

impl<R: ReplayGuard> TlsNip98Authenticator<R> {
    pub fn authenticate(
        &self,
        event_json: &str,
        method: &str,
        canonical_url: &str,
        payload_hash: Option<&str>,
        now: u64,
    ) -> Result<AuthenticatedPrincipal, AuthenticationError> {
        self.authority.authenticate(
            event_json,
            method,
            canonical_url,
            payload_hash,
            now,
            &self.replay,
        )
    }
}

struct TlsState<R, H> {
    authenticator: TlsNip98Authenticator<R>,
    canonical_origin: String,
    handler: Arc<H>,
}

impl<R: Clone, H> Clone for TlsState<R, H> {
    fn clone(&self) -> Self {
        Self {
            authenticator: TlsNip98Authenticator {
                authority: self.authenticator.authority.clone(),
                replay: self.authenticator.replay.clone(),
            },
            canonical_origin: self.canonical_origin.clone(),
            handler: Arc::clone(&self.handler),
        }
    }
}

pub struct TlsLifecycleServer<R, H> {
    pub address: SocketAddr,
    pub certificate_pem: PathBuf,
    pub private_key_pem: PathBuf,
    pub canonical_origin: String,
    pub authenticator: TlsNip98Authenticator<R>,
    pub handler: Arc<H>,
}

impl<R, H> TlsLifecycleServer<R, H>
where
    R: ReplayGuard + Clone + Send + Sync + 'static,
    H: AuthenticatedRequestHandler,
{
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) -> Result<(), TransportError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &self.certificate_pem,
            &self.private_key_pem,
        )
        .await
        .map_err(|error| TransportError::TlsConfiguration(error.to_string()))?;
        let state = TlsState {
            authenticator: self.authenticator,
            canonical_origin: self.canonical_origin.trim_end_matches('/').to_owned(),
            handler: self.handler,
        };
        let router = Router::new()
            .fallback(any(tls_route::<R, H>))
            .layer(DefaultBodyLimit::max(MAX_LIFECYCLE_REQUEST_BYTES))
            .with_state(state);
        let handle = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            let _ = shutdown.changed().await;
            shutdown_handle.graceful_shutdown(Some(Duration::from_secs(5)));
        });
        axum_server::bind_rustls(self.address, tls)
            .handle(handle)
            .serve(router.into_make_service())
            .await
            .map_err(TransportError::Io)
    }
}

async fn tls_route<R, H>(
    State(state): State<TlsState<R, H>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response
where
    R: ReplayGuard + Clone + Send + Sync + 'static,
    H: AuthenticatedRequestHandler,
{
    let Some(header) = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
    else {
        return (StatusCode::UNAUTHORIZED, "missing NIP-98 authorization").into_response();
    };
    let Some(encoded_event) = header.strip_prefix("Nostr ") else {
        return (StatusCode::UNAUTHORIZED, "invalid NIP-98 authorization").into_response();
    };
    let event = match base64::engine::general_purpose::STANDARD.decode(encoded_event) {
        Ok(value) => value,
        Err(_) => {
            return (StatusCode::UNAUTHORIZED, "invalid NIP-98 authorization").into_response()
        }
    };
    let event_json = match std::str::from_utf8(&event) {
        Ok(value) => value,
        Err(_) => {
            return (StatusCode::UNAUTHORIZED, "invalid NIP-98 authorization").into_response()
        }
    };
    let canonical_url = format!("{}{}", state.canonical_origin, uri);
    let payload_hash = (!body.is_empty()).then(|| format!("{:x}", Sha256::digest(&body)));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let actor = match state.authenticator.authenticate(
        event_json,
        method.as_str(),
        &canonical_url,
        payload_hash.as_deref(),
        now,
    ) {
        Ok(actor) => actor,
        Err(AuthenticationError::UnknownPrincipal) => {
            return (StatusCode::FORBIDDEN, "principal is not authorized").into_response();
        }
        Err(_) => {
            return (StatusCode::UNAUTHORIZED, "invalid NIP-98 authorization").into_response()
        }
    };
    let response = state.handler.handle(&actor, &body);
    if response.body.len() > MAX_LIFECYCLE_RESPONSE_BYTES {
        return (StatusCode::INTERNAL_SERVER_ERROR, "response exceeds limit").into_response();
    }
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, response.body).into_response()
}

async fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, TransportError> {
    let length = stream.read_u32().await? as usize;
    if length > MAX_LIFECYCLE_REQUEST_BYTES {
        return Err(TransportError::RequestTooLarge);
    }
    let mut request = vec![0; length];
    stream.read_exact(&mut request).await?;
    Ok(request)
}

async fn write_frame(stream: &mut UnixStream, response: &[u8]) -> Result<(), TransportError> {
    let length = u32::try_from(response.len()).map_err(|_| TransportError::ResponseTooLarge)?;
    stream.write_u32(length).await?;
    stream.write_all(response).await?;
    Ok(())
}

fn prepare_socket(path: &Path) -> Result<(), TransportError> {
    let parent = path.parent().ok_or(TransportError::MissingParent)?;
    let metadata = std::fs::metadata(parent)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o022 != 0 {
        return Err(TransportError::InsecureParent);
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => return Err(TransportError::PathOccupied),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.0).is_ok_and(|value| value.file_type().is_socket()) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("socket path has no parent")]
    MissingParent,
    #[error("socket parent must not be group- or world-writable")]
    InsecureParent,
    #[error("socket path is occupied")]
    PathOccupied,
    #[error("request exceeds transport limit")]
    RequestTooLarge,
    #[error("response exceeds transport limit")]
    ResponseTooLarge,
    #[error("transport connection task failed: {0}")]
    Task(String),
    #[error("TLS certificate or key configuration is invalid: {0}")]
    TlsConfiguration(String),
    #[error(transparent)]
    Authentication(#[from] AuthenticationError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::os::unix::fs::MetadataExt;
    use std::sync::Mutex;

    use super::*;
    use crate::api::*;
    use base64::Engine;
    use nostr::{EventBuilder, JsonUtil, Keys, Kind, Tag, Timestamp};

    struct Echo(Mutex<Option<AuthenticatedPrincipal>>);

    impl AuthenticatedRequestHandler for Echo {
        fn handle(&self, actor: &AuthenticatedPrincipal, request: &[u8]) -> HandlerResponse {
            *self.0.lock().unwrap() = Some(actor.clone());
            HandlerResponse {
                status: 200,
                body: request.to_vec(),
            }
        }
    }

    #[derive(Default)]
    struct RouterApplication(Mutex<Option<AgentResource>>);

    impl LifecycleApplication for RouterApplication {
        fn add_community(
            &self,
            _: &AddCommunityRequest,
        ) -> Result<crate::CommunityConfig, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn get_community(
            &self,
            _: crate::CommunityConfigId,
        ) -> Result<crate::CommunityConfig, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn list_communities(&self) -> Result<Vec<crate::CommunityConfig>, ApplicationError> {
            Ok(Vec::new())
        }
        fn remove_community(
            &self,
            _: crate::CommunityConfigId,
        ) -> Result<crate::CommunityConfig, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }

        fn create_agent(
            &self,
            _: &AuthenticatedPrincipal,
            metadata: &CommandMetadata,
            input: &CreateAgentInput,
        ) -> Result<OperationResource, ApplicationError> {
            let id = crate::AgentId::new();
            *self.0.lock().unwrap() = Some(AgentResource {
                id,
                community_config_id: input.community_config_id,
                display_name: input.display_name.clone(),
                system_prompt: input.system_prompt.clone(),
                runtime_id: input.runtime_id.clone(),
                desired_state: crate::DesiredAgentState::Enabled,
                purge_after: None,
            });
            Ok(OperationResource {
                id: crate::OperationId::new(),
                kind: crate::OperationKind::CreateAgent,
                status: crate::OperationStatus::Pending,
                agent_id: Some(id),
                correlation_id: metadata.correlation_id.clone(),
                error_code: None,
                created_at: 1,
                updated_at: 1,
            })
        }
        fn get_agent(&self, id: crate::AgentId) -> Result<AgentResource, ApplicationError> {
            self.0
                .lock()
                .unwrap()
                .clone()
                .filter(|agent| agent.id == id)
                .ok_or(ApplicationError::NotFound)
        }
        fn list_agents(
            &self,
            _: &ListAgentsRequest,
        ) -> Result<Vec<AgentResource>, ApplicationError> {
            Ok(self.0.lock().unwrap().clone().into_iter().collect())
        }
        fn update_agent(
            &self,
            _: &AuthenticatedPrincipal,
            _: &UpdateAgentRequest,
        ) -> Result<OperationResource, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn change_agent_state(
            &self,
            _: &AuthenticatedPrincipal,
            _: &ChangeAgentStateRequest,
        ) -> Result<OperationResource, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn delete_agent(
            &self,
            _: &AuthenticatedPrincipal,
            _: &AgentCommandRequest,
        ) -> Result<OperationResource, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn purge_agent(
            &self,
            _: &AuthenticatedPrincipal,
            _: &AgentCommandRequest,
        ) -> Result<OperationResource, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn agent_logs(&self, _: &AgentLogsRequest) -> Result<AgentLogsResource, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn get_operation(
            &self,
            _: crate::OperationId,
        ) -> Result<OperationResource, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn submit_draft(
            &self,
            _: &AuthenticatedPrincipal,
            _: &SubmitDraftRequest,
        ) -> Result<DraftResource, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn get_draft(&self, _: &str) -> Result<DraftResource, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
        fn promote_draft(
            &self,
            _: &AuthenticatedPrincipal,
            _: &PromoteDraftRequest,
        ) -> Result<OperationResource, ApplicationError> {
            Err(ApplicationError::Unsupported)
        }
    }

    fn administrator() -> AuthenticatedPrincipal {
        AuthenticatedPrincipal {
            principal: crate::auth::Principal::UnixPeer {
                uid: 1000,
                gid: 1000,
                pid: None,
            },
            authority: crate::auth::Authority::Administrator,
        }
    }

    #[test]
    fn json_router_dispatches_mutation_read_and_malformed_error() {
        let router = LifecycleJsonRouter::new(RouterApplication::default());
        let community_config_id = crate::CommunityConfigId::new();
        let create = LifecycleRouteRequest::CreateAgent(CreateAgentRequest {
            metadata: CommandMetadata {
                idempotency_key: "create-1".into(),
                correlation_id: "correlation-1".into(),
            },
            agent: CreateAgentInput {
                community_config_id,
                display_name: "Builder".into(),
                system_prompt: "Build safely.".into(),
                runtime_id: "codex-acp".parse().unwrap(),
            },
        });
        let created = router.handle(&administrator(), &serde_json::to_vec(&create).unwrap());
        assert_eq!(created.status, 200);
        let created: serde_json::Value = serde_json::from_slice(&created.body).unwrap();
        assert_eq!(created["status"], "ok");

        let listed = router.handle(
            &administrator(),
            &serde_json::to_vec(&LifecycleRouteRequest::ListAgents(ListAgentsRequest {
                community_config_id: Some(community_config_id),
            }))
            .unwrap(),
        );
        assert_eq!(listed.status, 200);
        let listed: serde_json::Value = serde_json::from_slice(&listed.body).unwrap();
        assert_eq!(listed["value"]["resource"], "agents");
        assert_eq!(listed["value"]["value"].as_array().unwrap().len(), 1);

        let malformed = router.handle(&administrator(), b"not-json");
        assert_eq!(malformed.status, 400);
        let malformed: serde_json::Value = serde_json::from_slice(&malformed.body).unwrap();
        assert_eq!(malformed["value"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn unix_server_uses_kernel_peer_credentials_and_connectable_socket() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let uid = std::fs::metadata(directory.path()).unwrap().uid();
        let path = directory.path().join("lifecycle.sock");
        let handler = Arc::new(Echo(Mutex::new(None)));
        let server = UnixLifecycleServer::new(
            &path,
            UnixAuthorityPolicy {
                administrator_uids: vec![uid],
                draft_submitter_uids: Vec::new(),
            },
            Arc::clone(&handler),
        );
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move { server.run(shutdown_rx).await });
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o666
        );
        let mut client = UnixStream::connect(&path).await.unwrap();
        client.write_u32(4).await.unwrap();
        client.write_all(b"ping").await.unwrap();
        assert_eq!(client.read_u32().await.unwrap(), 4);
        let mut response = [0; 4];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"ping");
        assert_eq!(
            handler.0.lock().unwrap().as_ref().unwrap().authority,
            crate::auth::Authority::Administrator
        );
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn idle_clients_do_not_block_accepts_or_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let uid = std::fs::metadata(directory.path()).unwrap().uid();
        let path = directory.path().join("idle.sock");
        let handler = Arc::new(Echo(Mutex::new(None)));
        let server = UnixLifecycleServer::new(
            &path,
            UnixAuthorityPolicy {
                administrator_uids: vec![uid],
                draft_submitter_uids: Vec::new(),
            },
            handler,
        );
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move { server.run(shutdown_rx).await });
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let _idle = UnixStream::connect(&path).await.unwrap();
        let mut active = UnixStream::connect(&path).await.unwrap();
        active.write_u32(4).await.unwrap();
        active.write_all(b"ping").await.unwrap();
        assert_eq!(active.read_u32().await.unwrap(), 4);
        shutdown_tx.send(true).unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .expect("idle connection must be aborted on shutdown")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn unix_connection_concurrency_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let uid = std::fs::metadata(directory.path()).unwrap().uid();
        let path = directory.path().join("bounded.sock");
        let server = UnixLifecycleServer::new(
            &path,
            UnixAuthorityPolicy {
                administrator_uids: vec![uid],
                draft_submitter_uids: Vec::new(),
            },
            Arc::new(Echo(Mutex::new(None))),
        );
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move { server.run(shutdown_rx).await });
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let mut idle = Vec::new();
        for _ in 0..MAX_UNIX_CONNECTIONS {
            idle.push(UnixStream::connect(&path).await.unwrap());
        }
        tokio::task::yield_now().await;
        let mut queued = UnixStream::connect(&path).await.unwrap();
        queued.write_u32(4).await.unwrap();
        queued.write_all(b"ping").await.unwrap();
        assert!(timeout(Duration::from_millis(50), queued.read_u32())
            .await
            .is_err());
        drop(idle.pop());
        assert_eq!(
            timeout(Duration::from_secs(1), queued.read_u32())
                .await
                .unwrap()
                .unwrap(),
            4
        );
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[derive(Clone, Default)]
    struct TestReplay(Arc<Mutex<HashSet<String>>>);

    impl ReplayGuard for TestReplay {
        fn claim(&self, event_id: &str, _expires_at: u64) -> bool {
            self.0.lock().unwrap().insert(event_id.into())
        }
    }

    #[tokio::test]
    async fn tls_http_listener_extracts_and_verifies_nip98() {
        let directory = tempfile::tempdir().unwrap();
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let certificate_path = directory.path().join("certificate.pem");
        let key_path = directory.path().join("key.pem");
        std::fs::write(&certificate_path, certified.cert.pem()).unwrap();
        std::fs::write(&key_path, certified.signing_key.serialize_pem()).unwrap();
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = probe.local_addr().unwrap();
        drop(probe);
        let origin = format!("https://localhost:{}", address.port());
        let url = format!("{origin}/v1/agents");
        let body = b"request-body";
        let payload = format!("{:x}", Sha256::digest(body));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(27_235), "")
            .tags([
                Tag::parse(["u", &url]).unwrap(),
                Tag::parse(["method", "POST"]).unwrap(),
                Tag::parse(["payload", &payload]).unwrap(),
            ])
            .custom_created_at(Timestamp::from(now))
            .sign_with_keys(&keys)
            .unwrap();
        let authorization = format!(
            "Nostr {}",
            base64::engine::general_purpose::STANDARD.encode(event.as_json())
        );
        let handler = Arc::new(Echo(Mutex::new(None)));
        let server = TlsLifecycleServer {
            address,
            certificate_pem: certificate_path,
            private_key_pem: key_path,
            canonical_origin: origin,
            authenticator: TlsNip98Authenticator {
                authority: Nip98AuthorityPolicy {
                    administrator_pubkeys: vec![keys.public_key().to_hex()],
                    draft_submitter_pubkeys: Vec::new(),
                    freshness_seconds: 60,
                },
                replay: TestReplay::default(),
            },
            handler: Arc::clone(&handler),
        };
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move { server.run(shutdown_rx).await });
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();
        let mut response = None;
        for _ in 0..100 {
            match client
                .post(&url)
                .header("authorization", &authorization)
                .body(body.to_vec())
                .send()
                .await
            {
                Ok(value) => {
                    response = Some(value);
                    break;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        let response = response.expect("TLS listener must start");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap().as_ref(), body);
        assert_eq!(
            handler.0.lock().unwrap().as_ref().unwrap().authority,
            crate::auth::Authority::Administrator
        );
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }
}
