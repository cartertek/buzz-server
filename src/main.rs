//! Minimal Buzz Server development daemon.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use buzz_core::Keys;
use buzz_server::{
    api::LifecycleApplication,
    application::{LifecycleEffects, SqliteLifecycleApplication},
    auth::{
        AuthenticatedPrincipal, Authority, Nip98AuthorityPolicy, Principal, UnixAuthorityPolicy,
    },
    custody::{AgentIdentityCustody, FilesystemAgentIdentityCustody},
    launch::{ExecutableIdentity, HealthPolicy, RestartPolicy, SecretRef},
    reconcile::{ProcessReceiptRepository, Reconciler},
    signer::DisposableSigner,
    supervisor::{
        LocalLogPolicy, LocalProcessAdapter, ProcessSupervisor, SecretResolver, SupervisorError,
    },
    transport::{
        LifecycleJsonRouter, SqliteReplayGuard, TlsLifecycleServer, TlsNip98Authenticator,
        UnixLifecycleServer,
    },
    AgentFileStore, DurableOperation, LaunchSpec, LocalLaunchContext, ProcessReceipt,
    ResolvedAgentConfig, RuntimeCatalog, SqliteStore, StorageError,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonConfig {
    state_database: PathBuf,
    log_directory: PathBuf,
    working_directory: PathBuf,
    #[serde(default)]
    owner_secret_file: Option<PathBuf>,
    runtime_user: String,
    signer_conditions: String,
    runtime_catalog: RuntimeCatalog,
    harness: ExecutableIdentity,
    #[serde(default)]
    harness_arguments: Vec<String>,
    restart: RestartPolicy,
    health: HealthPolicy,
    #[serde(default)]
    lifecycle_api: LifecycleApiConfig,
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LifecycleApiConfig {
    unix_socket: PathBuf,
    administrator_uids: Vec<u32>,
    draft_submitter_uids: Vec<u32>,
    retention_seconds: i64,
    tls: Option<TlsApiConfig>,
}

impl Default for LifecycleApiConfig {
    fn default() -> Self {
        Self {
            unix_socket: "/run/buzz-server/lifecycle.sock".into(),
            administrator_uids: vec![0],
            draft_submitter_uids: Vec::new(),
            retention_seconds: buzz_server::application::DEFAULT_RETENTION_SECONDS,
            tls: None,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TlsApiConfig {
    address: std::net::SocketAddr,
    certificate_pem: PathBuf,
    private_key_pem: PathBuf,
    canonical_origin: String,
    administrator_pubkeys: Vec<String>,
    #[serde(default)]
    draft_submitter_pubkeys: Vec<String>,
    freshness_seconds: u64,
}

impl DaemonConfig {
    fn load(path: &Path) -> Result<Self, DaemonError> {
        if fs::metadata(path)?.len() > MAX_CONFIG_BYTES {
            return Err(DaemonError::InvalidConfig("config exceeds 1 MiB".into()));
        }
        let config: Self = serde_json::from_slice(&fs::read(path)?)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), DaemonError> {
        self.runtime_catalog.validate()?;
        for path in [
            &self.state_database,
            &self.log_directory,
            &self.working_directory,
            &self.lifecycle_api.unix_socket,
        ] {
            if !path.is_absolute() {
                return Err(DaemonError::InvalidConfig(
                    "all daemon paths must be absolute".into(),
                ));
            }
        }
        if self.lifecycle_api.retention_seconds < 0 {
            return Err(DaemonError::InvalidConfig(
                "lifecycle_api.retention_seconds must not be negative".into(),
            ));
        }
        if self.lifecycle_api.administrator_uids.is_empty() {
            return Err(DaemonError::InvalidConfig(
                "lifecycle_api requires at least one administrator UID".into(),
            ));
        }
        if let Some(tls) = &self.lifecycle_api.tls {
            if tls.canonical_origin.trim().is_empty()
                || tls.administrator_pubkeys.is_empty()
                || tls.freshness_seconds == 0
                || !tls.certificate_pem.is_absolute()
                || !tls.private_key_pem.is_absolute()
            {
                return Err(DaemonError::InvalidConfig(
                    "lifecycle_api TLS configuration is incomplete".into(),
                ));
            }
        }
        if let Some(owner_secret_file) = &self.owner_secret_file {
            if owner_secret_file != Path::new("/run/buzz-server/credentials/owner-secret") {
                return Err(DaemonError::InvalidConfig(
                    "owner_secret_file must be the ephemeral materialized credential path".into(),
                ));
            }
        }
        if self.runtime_user != "buzz-agent" {
            return Err(DaemonError::InvalidConfig(
                "runtime_user must be the isolated buzz-agent account".into(),
            ));
        }
        for (field, path, root) in [
            (
                "state_database",
                &self.state_database,
                Path::new("/var/lib/buzz-server"),
            ),
            (
                "log_directory",
                &self.log_directory,
                Path::new("/var/log/buzz-server"),
            ),
            (
                "working_directory",
                &self.working_directory,
                Path::new("/var/lib/buzz-server"),
            ),
        ] {
            if !path.starts_with(root) {
                return Err(DaemonError::InvalidConfig(format!(
                    "{field} must remain inside {} under the packaged systemd sandbox",
                    root.display()
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
enum ReconcileWork {
    Operation(buzz_server::OperationId),
    StartupAgent(buzz_server::AgentId),
    Shutdown,
}

#[derive(Clone, Copy, Debug)]
enum RelayPublicationWork {
    Wake,
    Shutdown,
}

#[derive(Clone)]
struct LifecycleWake {
    sender: Sender<ReconcileWork>,
    relay_publication_sender: Sender<RelayPublicationWork>,
    store: Arc<SqliteStore>,
    community_join: buzz_server::community_join::DesktopCommunityJoinVerifier,
    community_identity_root: PathBuf,
    agent_files: AgentFileStore,
    custody: FilesystemAgentIdentityCustody,
    legacy_owner_keys: Option<Keys>,
    auto_join_sender: tokio::sync::mpsc::UnboundedSender<buzz_server::CommunityConfigId>,
}

impl LifecycleWake {
    fn owner_keys_for_community(
        &self,
        community: &buzz_server::CommunityConfig,
    ) -> Result<Keys, buzz_server::api::ApplicationError> {
        if let Some(pubkey) = community.identity_pubkey.as_deref() {
            let path = self
                .community_identity_root
                .join(format!("{pubkey}.secret"));
            let secret = fs::read_to_string(path)
                .map_err(|_| buzz_server::api::ApplicationError::Internal)?;
            let keys = Keys::parse(secret.trim())
                .map_err(|_| buzz_server::api::ApplicationError::Internal)?;
            if !keys.public_key().to_hex().eq_ignore_ascii_case(pubkey) {
                return Err(buzz_server::api::ApplicationError::Internal);
            }
            return Ok(keys);
        }
        self.legacy_owner_keys
            .clone()
            .ok_or(buzz_server::api::ApplicationError::Internal)
    }

    fn sync_persona_references(&self, persona: &buzz_server::PersonaDefinition) {
        let mut communities = HashSet::new();
        if let Ok(agents) = self.store.list_agents(None) {
            for agent in agents {
                if self
                    .agent_files
                    .load_agent(agent.id)
                    .ok()
                    .and_then(|file| file.persona_id)
                    .as_deref()
                    == Some(persona.id.as_str())
                {
                    communities.insert(agent.community_config_id);
                }
            }
        }
        if let Ok(scopes) = self.store.relay_projection_scopes(
            buzz_server::storage::RelayProjectionKind::Persona,
            &persona.id,
        ) {
            communities.extend(scopes.into_iter().map(|scope| scope.community_config_id));
        }
        for community_id in communities {
            let Ok(Some(community)) = self.store.get_community(community_id) else {
                continue;
            };
            let Ok(owner_keys) = self.owner_keys_for_community(&community) else {
                eprintln!("persona projection owner key unavailable for {community_id}");
                continue;
            };
            match buzz_server::relay_projection::sync_persona(
                &community.relay_url,
                &owner_keys,
                persona,
            ) {
                Ok(()) => {
                    if let Err(error) = self.store.record_relay_projection(
                        community.id,
                        buzz_server::storage::RelayProjectionKind::Persona,
                        &persona.id,
                        community.relay_url.as_str(),
                        &owner_keys.public_key().to_hex(),
                        &buzz_server::relay_projection::persona_d_tag(&persona.id),
                        unix_seconds_i64(),
                    ) {
                        eprintln!(
                            "failed to record persona projection {}: {error}",
                            persona.id
                        );
                    }
                }
                Err(error) => {
                    eprintln!("persona projection sync failed for {}: {error}", persona.id)
                }
            }
        }
    }
}

impl LifecycleEffects for LifecycleWake {
    fn community_changed(&self, community_id: buzz_server::CommunityConfigId) {
        let _ = self.auto_join_sender.send(community_id);
        let _ = self
            .relay_publication_sender
            .send(RelayPublicationWork::Wake);
    }

    fn community_identity_unreferenced(&self, pubkey: &str) {
        match self.store.has_pending_relay_publications_for_owner(pubkey) {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                eprintln!("community identity cleanup deferred for {pubkey}: {error}");
                return;
            }
        }
        let path = self
            .community_identity_root
            .join(format!("{pubkey}.secret"));
        if let Err(error) = fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("community identity cleanup failed for {pubkey}: {error}");
            }
        }
    }

    fn verify_community_join(
        &self,
        community: &buzz_server::CommunityConfig,
    ) -> Result<(), buzz_server::api::ApplicationError> {
        self.community_join
            .verify(community)
            .map_err(|error| match error {
                buzz_server::community_join::CommunityJoinError::MembershipDenied => {
                    buzz_server::api::ApplicationError::Forbidden(error.to_string())
                }
                buzz_server::community_join::CommunityJoinError::Unreachable(_) => {
                    buzz_server::api::ApplicationError::Unavailable(error.to_string())
                }
                buzz_server::community_join::CommunityJoinError::InvalidResponse => {
                    buzz_server::api::ApplicationError::Unavailable(error.to_string())
                }
            })
    }

    fn create_persona(
        &self,
        request: &buzz_server::api::CreatePersonaRequest,
    ) -> Result<buzz_server::PersonaDefinition, buzz_server::api::ApplicationError> {
        let persona = buzz_server::PersonaDefinition {
            id: uuid::Uuid::now_v7().to_string(),
            display_name: request.display_name.trim().to_owned(),
            avatar_url: None,
            system_prompt: request.system_prompt.trim().to_owned(),
            runtime: request.runtime.clone(),
            model: None,
            provider: None,
            name_pool: vec![],
            environment: Default::default(),
            respond_to: None,
            respond_to_allowlist: vec![],
            parallelism: None,
            is_builtin: false,
            is_active: true,
            shared: false,
        };
        self.agent_files
            .write_persona(&persona)
            .map_err(agent_file_application_error)?;
        self.sync_persona_references(&persona);
        Ok(persona)
    }

    fn update_persona(
        &self,
        request: &buzz_server::api::UpdatePersonaRequest,
    ) -> Result<buzz_server::PersonaDefinition, buzz_server::api::ApplicationError> {
        let mut persona = self
            .agent_files
            .load_persona(&request.persona_id)
            .map_err(agent_file_application_error)?;
        if let Some(value) = &request.changes.display_name {
            persona.display_name = value.trim().to_owned();
        }
        if let Some(value) = &request.changes.system_prompt {
            persona.system_prompt = value.trim().to_owned();
        }
        if let Some(value) = &request.changes.runtime {
            persona.runtime = Some(value.clone());
        }
        self.agent_files
            .write_persona(&persona)
            .map_err(agent_file_application_error)?;
        self.sync_persona_references(&persona);
        Ok(persona)
    }

    fn get_persona(
        &self,
        id: &str,
    ) -> Result<buzz_server::PersonaDefinition, buzz_server::api::ApplicationError> {
        self.agent_files
            .load_persona(id)
            .map_err(agent_file_application_error)
    }

    fn list_personas(
        &self,
    ) -> Result<Vec<buzz_server::PersonaDefinition>, buzz_server::api::ApplicationError> {
        self.agent_files
            .list_personas()
            .map_err(agent_file_application_error)
    }

    fn delete_persona(
        &self,
        id: &str,
    ) -> Result<buzz_server::PersonaDefinition, buzz_server::api::ApplicationError> {
        self.agent_files
            .ensure_persona_removable(id)
            .map_err(agent_file_application_error)?;
        let scopes = self
            .store
            .relay_projection_scopes(buzz_server::storage::RelayProjectionKind::Persona, id)
            .map_err(buzz_server::api::ApplicationError::from)?;
        for scope in &scopes {
            self.store
                .enqueue_relay_publication(
                    buzz_server::storage::RelayPublicationAction::TombstonePersona,
                    scope,
                    id,
                    unix_seconds_i64(),
                )
                .map_err(buzz_server::api::ApplicationError::from)?;
        }
        let persona = self
            .agent_files
            .remove_persona(id)
            .map_err(agent_file_application_error)?;
        if !scopes.is_empty() {
            let _ = self
                .relay_publication_sender
                .send(RelayPublicationWork::Wake);
        }
        Ok(persona)
    }

    fn prepare_agent_create(
        &self,
        id: buzz_server::AgentId,
        input: &buzz_server::api::CreateAgentInput,
    ) -> Result<buzz_server::AgentSpec, buzz_server::api::ApplicationError> {
        let file = self
            .agent_files
            .build_create_file(
                id,
                input.display_name.clone(),
                input.persona_id.clone(),
                input.system_prompt.clone(),
                input.runtime_id.clone(),
            )
            .map_err(agent_file_application_error)?;
        self.agent_files
            .resolve(
                &file,
                input.community_config_id,
                buzz_server::DesiredAgentState::Enabled,
            )
            .map(|resolved| resolved.spec)
            .map_err(agent_file_application_error)
    }

    fn persist_agent_create(
        &self,
        agent: &buzz_server::AgentSpec,
        input: &buzz_server::api::CreateAgentInput,
    ) -> Result<(), buzz_server::api::ApplicationError> {
        if self.agent_files.agent_path(agent.id).exists() {
            return Ok(());
        }
        let file = self
            .agent_files
            .build_create_file(
                agent.id,
                input.display_name.clone(),
                input.persona_id.clone(),
                input.system_prompt.clone(),
                input.runtime_id.clone(),
            )
            .map_err(agent_file_application_error)?;
        self.agent_files
            .write_agent(&file)
            .map_err(agent_file_application_error)
    }

    fn prepare_agent_update(
        &self,
        current: &buzz_server::AgentSpec,
        changes: &buzz_server::api::UpdateAgentInput,
    ) -> Result<(), buzz_server::api::ApplicationError> {
        self.agent_files
            .ensure_agent_file(current)
            .map_err(agent_file_application_error)?;
        let mut file = self
            .agent_files
            .load_agent(current.id)
            .map_err(agent_file_application_error)?;
        if let Some(value) = &changes.display_name {
            file.display_name.clone_from(value);
        }
        if let Some(value) = &changes.system_prompt {
            if file.persona_id.is_some() {
                return Err(buzz_server::api::ApplicationError::Invalid(
                    buzz_server::ValidationError::new(
                        "system_prompt",
                        "is defined by the selected persona; update the persona instead",
                    ),
                ));
            }
            file.system_prompt = Some(value.clone());
        }
        if let Some(value) = &changes.runtime_id {
            file.runtime = Some(value.clone());
        }
        self.agent_files
            .write_agent(&file)
            .map_err(agent_file_application_error)
    }

    fn agent_public_key(&self, agent_id: buzz_server::AgentId) -> Option<String> {
        self.custody
            .load(agent_id)
            .ok()
            .map(|keys| keys.public_key().to_hex())
    }

    fn operation_ready(
        &self,
        operation: &DurableOperation,
    ) -> Result<(), buzz_server::api::ApplicationError> {
        self.sender
            .send(ReconcileWork::Operation(operation.id))
            .map_err(|_| buzz_server::api::ApplicationError::Internal)
    }
}

const INTERNAL_AUTHORIZATION_REFERENCE: &str = "internal:signer-issued-authorization";
const INTERNAL_AGENT_KEY_REFERENCE: &str = "internal:custodied-agent-private-key";

struct EnvironmentSecrets {
    authorization: String,
    agent_private_key: Option<String>,
}

impl SecretResolver for EnvironmentSecrets {
    fn resolve(&self, reference: &SecretRef) -> Result<String, SupervisorError> {
        if reference.key == INTERNAL_AUTHORIZATION_REFERENCE {
            return Ok(self.authorization.clone());
        }
        if reference.key == INTERNAL_AGENT_KEY_REFERENCE {
            return self
                .agent_private_key
                .clone()
                .ok_or(SupervisorError::SecretResolution);
        }
        if !valid_env_name(&reference.key) {
            return Err(SupervisorError::SecretResolution);
        }
        std::env::var(&reference.key).map_err(|_| SupervisorError::SecretResolution)
    }
}

struct ReceiptFile {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ReceiptFile {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }
}

impl ProcessReceiptRepository for ReceiptFile {
    fn get_receipt(
        &self,
        agent_id: buzz_server::AgentId,
    ) -> Result<Option<ProcessReceipt>, StorageError> {
        let _guard = self.lock.lock().map_err(|_| StorageError::LockPoisoned)?;
        match fs::read(&self.path) {
            Ok(bytes) => {
                let receipt: ProcessReceipt = serde_json::from_slice(&bytes)?;
                Ok((receipt.agent_id == agent_id).then_some(receipt))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StorageError::InvalidData(error.to_string())),
        }
    }

    fn put_receipt(&self, receipt: &ProcessReceipt) -> Result<(), StorageError> {
        let _guard = self.lock.lock().map_err(|_| StorageError::LockPoisoned)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| StorageError::InvalidData("receipt path has no parent".into()))?;
        fs::create_dir_all(parent).map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let temporary = self.path.with_extension("json.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        file.write_all(&serde_json::to_vec(receipt)?)
            .and_then(|()| file.sync_all())
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        fs::rename(temporary, &self.path)
            .and_then(|()| fs::File::open(parent)?.sync_all())
            .map_err(|error| StorageError::InvalidData(error.to_string()))
    }

    fn delete_receipt(&self, _agent_id: buzz_server::AgentId) -> Result<(), StorageError> {
        let _guard = self.lock.lock().map_err(|_| StorageError::LockPoisoned)?;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(StorageError::InvalidData(error.to_string())),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), DaemonError> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| {
            DaemonError::InvalidConfig("TLS crypto provider is already configured".into())
        })?;
    let config_path = parse_args()?;
    let config = DaemonConfig::load(&config_path)?;

    for directory in [
        config.state_database.parent(),
        config.lifecycle_api.unix_socket.parent(),
        Some(config.log_directory.as_path()),
    ] {
        fs::create_dir_all(
            directory.ok_or_else(|| {
                DaemonError::InvalidConfig("configured path has no parent".into())
            })?,
        )?;
    }
    let child_identity = resolve_user(&config.runtime_user)?;
    let child_home = resolve_user_home(&config.runtime_user)?;
    if let Some(lifecycle_directory) = config.lifecycle_api.unix_socket.parent() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(lifecycle_directory, fs::Permissions::from_mode(0o711))?;
        }
    }

    let legacy_owner_keys = if let Some(owner_secret_file) = &config.owner_secret_file {
        let owner_secret = read_secret_file(owner_secret_file)?;
        let keys = Keys::parse(&owner_secret).map_err(|_| DaemonError::InvalidOwnerSecret)?;
        drop(owner_secret);
        Some(keys)
    } else {
        None
    };

    let store = Arc::new(SqliteStore::open(&config.state_database)?);
    let supervisor = LocalProcessAdapter::new(
        LocalLogPolicy {
            directory: config.log_directory.clone(),
            max_file_bytes: 10 * 1024 * 1024,
            max_read_bytes: 64 * 1024,
        },
        Duration::from_secs(10),
        Some(child_identity),
        Some(child_home),
    )?;
    let custody_root = config
        .state_database
        .parent()
        .ok_or_else(|| DaemonError::InvalidConfig("state database has no parent".into()))?
        .join("identities");
    let custody = FilesystemAgentIdentityCustody::new(custody_root, 0);
    let agent_files = AgentFileStore::new(
        config
            .state_database
            .parent()
            .ok_or_else(|| DaemonError::InvalidConfig("state database has no parent".into()))?
            .join("agent-config"),
    )?;
    let config = Arc::new(config);
    let (operation_tx, operation_rx) = mpsc::channel();
    let (relay_publication_tx, relay_publication_rx) = mpsc::channel();
    let community_identity_root = config
        .state_database
        .parent()
        .ok_or_else(|| DaemonError::InvalidConfig("state database has no parent".into()))?
        .join("community-identities");
    let community_join = buzz_server::community_join::DesktopCommunityJoinVerifier::new(
        community_identity_root.clone(),
    )
    .map_err(|error| DaemonError::Task(format!("community join verifier: {error}")))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (auto_join_tx, auto_join_rx) = tokio::sync::mpsc::unbounded_channel();
    let lifecycle_application = SqliteLifecycleApplication::new(
        Arc::clone(&store),
        Arc::new(LifecycleWake {
            sender: operation_tx.clone(),
            relay_publication_sender: relay_publication_tx.clone(),
            store: Arc::clone(&store),
            community_join,
            community_identity_root: community_identity_root.clone(),
            agent_files: agent_files.clone(),
            custody: custody.clone(),
            legacy_owner_keys: legacy_owner_keys.clone(),
            auto_join_sender: auto_join_tx.clone(),
        }),
        unix_seconds_i64,
    )
    .with_retention_seconds(config.lifecycle_api.retention_seconds);

    let worker = spawn_reconciliation_worker(
        operation_rx,
        ReconcileContext {
            application: lifecycle_application.clone(),
            store: Arc::clone(&store),
            config: Arc::clone(&config),
            legacy_owner_keys: legacy_owner_keys.clone(),
            custody: custody.clone(),
            agent_files: agent_files.clone(),
            supervisor,
            child_identity,
            relay_publication_sender: relay_publication_tx.clone(),
        },
    );
    let relay_publication_worker = spawn_relay_publication_worker(
        relay_publication_rx,
        RelayPublicationContext {
            store: Arc::clone(&store),
            community_identity_root: community_identity_root.clone(),
            legacy_owner_keys: legacy_owner_keys.clone(),
        },
    );
    let _ = relay_publication_tx.send(RelayPublicationWork::Wake);

    for operation in store.nonterminal_operations()? {
        operation_tx
            .send(ReconcileWork::Operation(operation.id))
            .map_err(|_| DaemonError::Task("reconciliation worker stopped".into()))?;
    }
    for agent in store.list_agents(None)? {
        agent_files.ensure_agent_file(&agent)?;
        let file = agent_files.load_agent(agent.id)?;
        let resolved =
            agent_files.resolve(&file, agent.community_config_id, agent.desired_state)?;
        store.put_agent(&resolved.spec, unix_seconds_i64())?;
        operation_tx
            .send(ReconcileWork::StartupAgent(agent.id))
            .map_err(|_| DaemonError::Task("reconciliation worker stopped".into()))?;
    }

    let auto_join_context = AutoJoinContext {
        store: Arc::clone(&store),
        agent_files: agent_files.clone(),
        custody: custody.clone(),
        community_identity_root: community_identity_root.clone(),
        legacy_owner_keys: legacy_owner_keys.clone(),
    };
    let mut auto_join_task = tokio::spawn(run_auto_join_manager(
        auto_join_rx,
        shutdown_rx.clone(),
        auto_join_context,
    ));
    for community in store.list_communities()? {
        let _ = auto_join_tx.send(community.id);
    }

    let unix_lifecycle = UnixLifecycleServer::new(
        &config.lifecycle_api.unix_socket,
        UnixAuthorityPolicy {
            administrator_uids: config.lifecycle_api.administrator_uids.clone(),
            draft_submitter_uids: config.lifecycle_api.draft_submitter_uids.clone(),
        },
        Arc::new(LifecycleJsonRouter::new(lifecycle_application.clone())),
    );
    let mut unix_lifecycle_task = tokio::spawn({
        let lifecycle_shutdown = shutdown_rx.clone();
        async move { unix_lifecycle.run(lifecycle_shutdown).await }
    });
    let mut tls_lifecycle_task = config.lifecycle_api.tls.clone().map(|tls| {
        let lifecycle_shutdown = shutdown_rx.clone();
        let server = TlsLifecycleServer {
            address: tls.address,
            certificate_pem: tls.certificate_pem,
            private_key_pem: tls.private_key_pem,
            canonical_origin: tls.canonical_origin,
            authenticator: TlsNip98Authenticator {
                authority: Nip98AuthorityPolicy {
                    administrator_pubkeys: tls.administrator_pubkeys,
                    draft_submitter_pubkeys: tls.draft_submitter_pubkeys,
                    freshness_seconds: tls.freshness_seconds,
                },
                replay: SqliteReplayGuard {
                    store: Arc::clone(&store),
                    now: unix_seconds_unchecked,
                },
            },
            handler: Arc::new(LifecycleJsonRouter::new(lifecycle_application.clone())),
        };
        tokio::spawn(async move { server.run(lifecycle_shutdown).await })
    });
    let mut retention_tick = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
    retention_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    enum Completion {
        Lifecycle(Result<(), buzz_server::transport::TransportError>),
        AutoJoin(Result<(), DaemonError>),
        Signal,
    }
    let completion = loop {
        tokio::select! {
            result = &mut unix_lifecycle_task => break Completion::Lifecycle(result.map_err(|error| buzz_server::transport::TransportError::Task(error.to_string()))?),
            result = async { tls_lifecycle_task.as_mut().expect("guarded").await }, if tls_lifecycle_task.is_some() => break Completion::Lifecycle(result.map_err(|error| buzz_server::transport::TransportError::Task(error.to_string()))?),
            result = &mut auto_join_task => break Completion::AutoJoin(result?),
            signal = tokio::signal::ctrl_c() => { signal?; break Completion::Signal },
            _ = terminate.recv() => break Completion::Signal,
            _ = retention_tick.tick() => {
                enqueue_expired_purges(&lifecycle_application)?;
            },
        }
    };
    let _ = shutdown_tx.send(true);
    match completion {
        Completion::Lifecycle(result) => result?,
        Completion::AutoJoin(result) => result?,
        Completion::Signal => {}
    }
    unix_lifecycle_task.abort();
    if let Some(task) = tls_lifecycle_task {
        task.abort();
    }
    auto_join_task.abort();
    let _ = operation_tx.send(ReconcileWork::Shutdown);
    let _ = relay_publication_tx.send(RelayPublicationWork::Shutdown);
    worker
        .join()
        .map_err(|_| DaemonError::Task("reconciliation worker panicked".into()))?;
    relay_publication_worker
        .join()
        .map_err(|_| DaemonError::Task("relay publication worker panicked".into()))?;
    Ok(())
}

#[derive(Clone)]
struct AutoJoinContext {
    store: Arc<SqliteStore>,
    agent_files: AgentFileStore,
    custody: FilesystemAgentIdentityCustody,
    community_identity_root: PathBuf,
    legacy_owner_keys: Option<Keys>,
}

async fn run_auto_join_manager(
    mut changes: tokio::sync::mpsc::UnboundedReceiver<buzz_server::CommunityConfigId>,
    mut shutdown: watch::Receiver<bool>,
    context: AutoJoinContext,
) -> Result<(), DaemonError> {
    let mut watchers: HashMap<buzz_server::CommunityConfigId, tokio::task::JoinHandle<()>> =
        HashMap::new();
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            community_id = changes.recv() => {
                let Some(community_id) = community_id else { break; };
                if let Some(task) = watchers.remove(&community_id) {
                    task.abort();
                }
                let Some(community) = context.store.get_community(community_id)? else {
                    continue;
                };
                let task_context = context.clone();
                let task_shutdown = shutdown.clone();
                watchers.insert(community_id, tokio::spawn(async move {
                    if let Err(error) = run_community_auto_join(task_context, community, task_shutdown).await {
                        eprintln!("auto-join watcher failed for community {community_id}: {error}");
                    }
                }));
            }
        }
    }
    for (_, task) in watchers {
        task.abort();
    }
    Ok(())
}

async fn run_community_auto_join(
    context: AutoJoinContext,
    community: buzz_server::CommunityConfig,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), DaemonError> {
    let owner_keys = auto_join_owner_keys(&context, &community)?;
    let subscription_id = format!("server-auto-join-{}", community.id);
    let request = buzz_server::auto_join::channel_creation_subscription(&subscription_id);
    let mut backoff = Duration::from_secs(1);
    while !*shutdown.borrow() {
        let connection = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                continue;
            }
            result = buzz_ws_client::NostrWsConnection::connect_authenticated(
                community.relay_url.as_str(),
                &owner_keys,
                None,
            ) => result,
        };
        let mut connection = match connection {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!(
                    "auto-join relay connection failed for {}: {error}",
                    community.id
                );
                if auto_join_wait_or_shutdown(backoff, &mut shutdown).await {
                    return Ok(());
                }
                backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
                continue;
            }
        };
        if let Err(error) = connection.send_raw(&request).await {
            eprintln!(
                "auto-join subscription failed for {}: {error}",
                community.id
            );
            let _ = connection.disconnect().await;
            if auto_join_wait_or_shutdown(backoff, &mut shutdown).await {
                return Ok(());
            }
            backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
            continue;
        }
        backoff = Duration::from_secs(1);
        loop {
            match buzz_server::auto_join::next_open_channel(&mut connection, &mut shutdown).await {
                Ok(Some(channel_id)) => {
                    reconcile_auto_join_channel(&context, &community, &owner_keys, channel_id).await
                }
                Ok(None) => {
                    let _ = connection.disconnect().await;
                    return Ok(());
                }
                Err(error) => {
                    eprintln!(
                        "auto-join relay stream failed for {}: {error}",
                        community.id
                    );
                    let _ = connection.disconnect().await;
                    break;
                }
            }
        }
        if auto_join_wait_or_shutdown(backoff, &mut shutdown).await {
            return Ok(());
        }
        backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
    }
    Ok(())
}

async fn reconcile_auto_join_channel(
    context: &AutoJoinContext,
    community: &buzz_server::CommunityConfig,
    owner_keys: &Keys,
    channel_id: uuid::Uuid,
) {
    let agents = match context.store.list_agents(Some(community.id)) {
        Ok(agents) => agents,
        Err(error) => {
            eprintln!(
                "auto-join could not list agents for {}: {error}",
                community.id
            );
            return;
        }
    };
    for agent in agents {
        if agent.desired_state == buzz_server::DesiredAgentState::Deleted {
            continue;
        }
        let file = match context.agent_files.load_agent(agent.id) {
            Ok(file) => file,
            Err(error) => {
                eprintln!("auto-join could not read config for {}: {error}", agent.id);
                continue;
            }
        };
        if !file.auto_join_open_channels {
            continue;
        }
        let agent_keys = match context.custody.provision(agent.id) {
            Ok(identity) => match context.custody.load(agent.id) {
                Ok(keys) if keys.public_key().to_hex() == identity.public_key => keys,
                Ok(_) => {
                    eprintln!("auto-join identity mismatch for {}", agent.id);
                    continue;
                }
                Err(error) => {
                    eprintln!(
                        "auto-join could not load identity for {}: {error}",
                        agent.id
                    );
                    continue;
                }
            },
            Err(error) => {
                eprintln!(
                    "auto-join could not provision identity for {}: {error}",
                    agent.id
                );
                continue;
            }
        };
        let auth_tag = match buzz_sdk::nip_oa::compute_auth_tag(
            owner_keys,
            &agent_keys.public_key(),
            "kind=9021",
        ) {
            Ok(tag) => tag,
            Err(error) => {
                eprintln!("auto-join authorization failed for {}: {error}", agent.id);
                continue;
            }
        };
        match buzz_server::auto_join::publish_join(
            &community.relay_url,
            &agent_keys,
            &auth_tag,
            channel_id,
        )
        .await
        {
            Ok(()) => eprintln!("auto-joined agent {} to channel {channel_id}", agent.id),
            Err(error) if auto_join_already_member(&error) => {}
            Err(error) => eprintln!(
                "auto-join failed for agent {} channel {channel_id}: {error}",
                agent.id
            ),
        }
    }
}

fn auto_join_owner_keys(
    context: &AutoJoinContext,
    community: &buzz_server::CommunityConfig,
) -> Result<Keys, DaemonError> {
    let Some(pubkey) = community.identity_pubkey.as_deref() else {
        return context.legacy_owner_keys.clone().ok_or_else(|| {
            DaemonError::InvalidConfig(
                "legacy community has no associated identity; rejoin the community".into(),
            )
        });
    };
    let secret = fs::read_to_string(
        context
            .community_identity_root
            .join(format!("{pubkey}.secret")),
    )?;
    let keys = Keys::parse(secret.trim()).map_err(|_| DaemonError::InvalidOwnerSecret)?;
    if !keys.public_key().to_hex().eq_ignore_ascii_case(pubkey) {
        return Err(DaemonError::InvalidOwnerSecret);
    }
    Ok(keys)
}

fn auto_join_already_member(message: &str) -> bool {
    let value = message.to_ascii_lowercase();
    value.contains("already") && value.contains("member")
}

async fn auto_join_wait_or_shutdown(
    duration: Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        () = tokio::time::sleep(duration) => false,
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
    }
}

#[derive(Clone)]
struct RelayPublicationContext {
    store: Arc<SqliteStore>,
    community_identity_root: PathBuf,
    legacy_owner_keys: Option<Keys>,
}

fn spawn_relay_publication_worker(
    receiver: Receiver<RelayPublicationWork>,
    context: RelayPublicationContext,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("buzz-relay-publications".into())
        .spawn(move || loop {
            match receiver.recv_timeout(Duration::from_secs(30)) {
                Ok(RelayPublicationWork::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(RelayPublicationWork::Wake) | Err(RecvTimeoutError::Timeout) => {
                    if let Err(error) = drain_relay_publications(&context) {
                        eprintln!("relay publication drain failed: {error}");
                    }
                }
            }
        })
        .expect("failed to spawn relay publication worker")
}

fn drain_relay_publications(context: &RelayPublicationContext) -> Result<(), DaemonError> {
    for publication in context.store.pending_relay_publications()? {
        let relay_url = url::Url::parse(&publication.relay_url)
            .map_err(|error| DaemonError::Task(format!("invalid retained relay URL: {error}")))?;
        let owner_keys = match relay_publication_owner_keys(context, &publication.owner_pubkey) {
            Ok(keys) => keys,
            Err(error) => {
                context.store.fail_relay_publication(
                    &publication.id,
                    &error.to_string(),
                    unix_seconds_i64(),
                )?;
                continue;
            }
        };
        let result = match publication.action {
            buzz_server::storage::RelayPublicationAction::TombstoneManagedAgent => {
                buzz_server::relay_projection::tombstone_managed_agent(
                    &relay_url,
                    &owner_keys,
                    &publication.d_tag,
                )
            }
            buzz_server::storage::RelayPublicationAction::ArchiveIdentity => {
                buzz_server::relay_projection::archive_identity(
                    &relay_url,
                    &owner_keys,
                    &publication.d_tag,
                )
            }
            buzz_server::storage::RelayPublicationAction::TombstonePersona => {
                buzz_server::relay_projection::tombstone_persona(
                    &relay_url,
                    &owner_keys,
                    &publication.subject_id,
                )
            }
        };
        match result {
            Ok(()) => {
                context.store.complete_relay_publication(&publication.id)?;
                if let Some(community_id) = publication.community_config_id {
                    let projection = match publication.action {
                        buzz_server::storage::RelayPublicationAction::TombstoneManagedAgent => {
                            Some(buzz_server::storage::RelayProjectionKind::ManagedAgent)
                        }
                        buzz_server::storage::RelayPublicationAction::TombstonePersona => {
                            Some(buzz_server::storage::RelayProjectionKind::Persona)
                        }
                        buzz_server::storage::RelayPublicationAction::ArchiveIdentity => None,
                    };
                    if let Some(kind) = projection {
                        context.store.remove_relay_projection(
                            community_id,
                            kind,
                            &publication.subject_id,
                        )?;
                    }
                }
                cleanup_unreferenced_projection_owner(context, &publication.owner_pubkey)?;
            }
            Err(error) => {
                context.store.fail_relay_publication(
                    &publication.id,
                    &error.to_string(),
                    unix_seconds_i64(),
                )?;
            }
        }
    }
    Ok(())
}

fn relay_publication_owner_keys(
    context: &RelayPublicationContext,
    owner_pubkey: &str,
) -> Result<Keys, DaemonError> {
    let path = context
        .community_identity_root
        .join(format!("{owner_pubkey}.secret"));
    if path.exists() {
        let secret = fs::read_to_string(path)?;
        let keys = Keys::parse(secret.trim()).map_err(|_| DaemonError::InvalidOwnerSecret)?;
        if keys
            .public_key()
            .to_hex()
            .eq_ignore_ascii_case(owner_pubkey)
        {
            return Ok(keys);
        }
        return Err(DaemonError::InvalidOwnerSecret);
    }
    context
        .legacy_owner_keys
        .clone()
        .filter(|keys| {
            keys.public_key()
                .to_hex()
                .eq_ignore_ascii_case(owner_pubkey)
        })
        .ok_or_else(|| DaemonError::Task(format!("owner key {owner_pubkey} is unavailable")))
}

fn cleanup_unreferenced_projection_owner(
    context: &RelayPublicationContext,
    owner_pubkey: &str,
) -> Result<(), DaemonError> {
    if context
        .store
        .has_pending_relay_publications_for_owner(owner_pubkey)?
        || context
            .store
            .list_communities()?
            .iter()
            .any(|community| community.identity_pubkey.as_deref() == Some(owner_pubkey))
    {
        return Ok(());
    }
    let path = context
        .community_identity_root
        .join(format!("{owner_pubkey}.secret"));
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

struct ReconcileContext {
    application: SqliteLifecycleApplication<LifecycleWake>,
    store: Arc<SqliteStore>,
    config: Arc<DaemonConfig>,
    legacy_owner_keys: Option<Keys>,
    custody: FilesystemAgentIdentityCustody,
    agent_files: AgentFileStore,
    supervisor: LocalProcessAdapter,
    child_identity: (u32, u32),
    relay_publication_sender: Sender<RelayPublicationWork>,
}

fn spawn_reconciliation_worker(
    receiver: Receiver<ReconcileWork>,
    context: ReconcileContext,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("buzz-reconciler".into())
        .spawn(move || loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(ReconcileWork::Operation(operation_id)) => {
                    reconcile_operation_contained(&context, operation_id);
                }
                Ok(ReconcileWork::StartupAgent(agent_id)) => {
                    if let Err(error) = reconcile_startup_agent(&context, agent_id) {
                        eprintln!("startup reconciliation failed for {agent_id}: {error}");
                    }
                }
                Ok(ReconcileWork::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if let Err(error) = observe_dynamic_agents(&context) {
                        eprintln!("agent observation failed: {error}");
                    }
                    if let Err(error) = cleanup_purged_agent_artifacts(&context) {
                        eprintln!("purged-agent artifact cleanup failed: {error}");
                    }
                }
            }
        })
        .expect("failed to spawn reconciliation worker")
}

fn reconcile_operation_contained(
    context: &ReconcileContext,
    operation_id: buzz_server::OperationId,
) {
    if let Err(error) = reconcile_lifecycle_operation(context, operation_id) {
        eprintln!("lifecycle operation {operation_id} failed: {error}");
        let terminal_result = (|| -> Result<(), buzz_server::api::ApplicationError> {
            let operation = context.application.get_operation(operation_id)?;
            if operation.status.is_terminal() {
                return Ok(());
            }
            if operation.status == buzz_server::OperationStatus::Pending {
                context.application.start_operation(operation_id)?;
            }
            context.application.complete_operation(
                operation_id,
                buzz_server::OperationStatus::Failed,
                Some(buzz_server::ErrorCode::Internal),
            )
        })();
        if let Err(persist_error) = terminal_result {
            eprintln!(
                "failed to persist terminal state for lifecycle operation {operation_id}: {persist_error}"
            );
        }
    }
}

fn reconcile_startup_agent(
    context: &ReconcileContext,
    agent_id: buzz_server::AgentId,
) -> Result<(), DaemonError> {
    let agent = context
        .store
        .get_agent(agent_id)?
        .ok_or(StorageError::NotFound)?;
    let kind = match agent.desired_state {
        buzz_server::DesiredAgentState::Enabled => buzz_server::OperationKind::EnableAgent,
        buzz_server::DesiredAgentState::Disabled => buzz_server::OperationKind::DisableAgent,
        buzz_server::DesiredAgentState::Deleted => buzz_server::OperationKind::DeleteAgent,
    };
    let startup = buzz_server::api::OperationResource {
        id: buzz_server::OperationId::new(),
        kind,
        status: buzz_server::OperationStatus::Running,
        agent_id: Some(agent.id),
        error_code: None,
        correlation_id: format!("startup:{}", agent.id),
        created_at: unix_seconds_i64(),
        updated_at: unix_seconds_i64(),
    };
    reconcile_dynamic_lifecycle_operation(context, &startup, false)
}

fn parse_args() -> Result<PathBuf, DaemonError> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--config")) {
        return Err(DaemonError::Usage);
    }
    let path = arguments.next().ok_or(DaemonError::Usage)?;
    if arguments.next().is_some() {
        return Err(DaemonError::Usage);
    }
    Ok(path.into())
}

fn read_secret_file(path: &Path) -> Result<String, DaemonError> {
    let value = fs::read_to_string(path)
        .map_err(|_| DaemonError::MissingSecret(path.display().to_string()))?;
    let value = value.trim_end_matches(['\r', '\n']).to_owned();
    if value.is_empty() {
        return Err(DaemonError::MissingSecret(path.display().to_string()));
    }
    Ok(value)
}

fn secret_generation(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

fn resolve_user(name: &str) -> Result<(u32, u32), DaemonError> {
    let passwd = fs::read_to_string("/etc/passwd")?;
    for line in passwd.lines() {
        let fields: Vec<_> = line.split(':').collect();
        if fields.first() == Some(&name) && fields.len() >= 4 {
            let uid = fields[2]
                .parse()
                .map_err(|_| DaemonError::InvalidConfig("runtime user has invalid uid".into()))?;
            let gid = fields[3]
                .parse()
                .map_err(|_| DaemonError::InvalidConfig("runtime user has invalid gid".into()))?;
            return Ok((uid, gid));
        }
    }
    Err(DaemonError::InvalidConfig(format!(
        "runtime user {name} does not exist"
    )))
}

fn resolve_user_home(name: &str) -> Result<PathBuf, DaemonError> {
    let passwd = fs::read_to_string("/etc/passwd")?;
    for line in passwd.lines() {
        let fields: Vec<_> = line.split(':').collect();
        if fields.first() == Some(&name) && fields.len() >= 6 {
            let home = fields[5];
            if home.starts_with('/') && !home.is_empty() {
                return Ok(PathBuf::from(home));
            }
            return Err(DaemonError::InvalidConfig(
                "runtime user has invalid home directory".into(),
            ));
        }
    }
    Err(DaemonError::InvalidConfig(format!(
        "runtime user {name} does not exist"
    )))
}

fn valid_env_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some('A'..='Z' | '_'))
        && characters.all(|character| matches!(character, 'A'..='Z' | '0'..='9' | '_'))
}

fn path_string(path: &Path) -> Result<String, DaemonError> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| DaemonError::InvalidConfig("paths must be UTF-8".into()))
}

fn unix_seconds_i64() -> i64 {
    i64::try_from(unix_seconds_unchecked()).unwrap_or(i64::MAX)
}

fn unix_seconds_unchecked() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn reconcile_lifecycle_operation(
    context: &ReconcileContext,
    operation_id: buzz_server::OperationId,
) -> Result<(), DaemonError> {
    let operation = context.application.get_operation(operation_id)?;
    if operation.status == buzz_server::OperationStatus::Pending {
        context.application.start_operation(operation_id)?;
    }
    let durable = context.application.get_operation(operation_id)?;
    if durable.status != buzz_server::OperationStatus::Running {
        return Ok(());
    }
    reconcile_dynamic_lifecycle_operation(context, &durable, true)
}

#[derive(Clone, Debug)]
struct AgentFilesystemLayout {
    receipt: PathBuf,
    workspace: PathBuf,
    runtime: PathBuf,
    launch_id: String,
}

fn dynamic_agent_layout(
    config: &DaemonConfig,
    agent_id: buzz_server::AgentId,
) -> Result<AgentFilesystemLayout, DaemonError> {
    let state_root = config
        .state_database
        .parent()
        .ok_or_else(|| DaemonError::InvalidConfig("state database has no parent".into()))?
        .join("agents")
        .join(agent_id.to_string());
    Ok(AgentFilesystemLayout {
        receipt: state_root.join("process-receipt.json"),
        workspace: state_root.join("workspace"),
        runtime: state_root.join("runtime"),
        launch_id: format!("agent-{agent_id}"),
    })
}

fn community_owner_keys(
    context: &ReconcileContext,
    community: &buzz_server::CommunityConfig,
) -> Result<Keys, DaemonError> {
    let Some(pubkey) = community.identity_pubkey.as_deref() else {
        return context.legacy_owner_keys.clone().ok_or_else(|| {
            DaemonError::InvalidConfig(
                "legacy community has no associated identity; rejoin the community".into(),
            )
        });
    };
    let root = context
        .config
        .state_database
        .parent()
        .ok_or_else(|| DaemonError::InvalidConfig("state database has no parent".into()))?
        .join("community-identities");
    let path = root.join(format!("{pubkey}.secret"));
    let secret = fs::read_to_string(path)?;
    let keys = Keys::parse(secret.trim()).map_err(|_| DaemonError::InvalidOwnerSecret)?;
    if !keys.public_key().to_hex().eq_ignore_ascii_case(pubkey) {
        return Err(DaemonError::InvalidOwnerSecret);
    }
    Ok(keys)
}

fn reconcile_dynamic_lifecycle_operation(
    context: &ReconcileContext,
    operation: &buzz_server::api::OperationResource,
    finish_operation: bool,
) -> Result<(), DaemonError> {
    let agent_id = operation.agent_id.ok_or(StorageError::NotFound)?;
    let cached_agent = context
        .store
        .get_agent(agent_id)?
        .ok_or(StorageError::NotFound)?;
    context.agent_files.ensure_agent_file(&cached_agent)?;
    let file = context.agent_files.load_agent(agent_id)?;
    let resolved = context.agent_files.resolve(
        &file,
        cached_agent.community_config_id,
        cached_agent.desired_state,
    )?;
    let agent = resolved.spec.clone();
    context.store.put_agent(&agent, unix_seconds_i64())?;
    let community = context
        .store
        .get_community(agent.community_config_id)?
        .ok_or(StorageError::NotFound)?;
    let layout = dynamic_agent_layout(context.config.as_ref(), agent_id)?;
    let state_root = layout
        .receipt
        .parent()
        .ok_or_else(|| DaemonError::InvalidConfig("agent receipt path has no parent".into()))?;
    fs::create_dir_all(state_root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{chown, PermissionsExt};
        chown(state_root, Some(0), Some(context.child_identity.1))?;
        fs::set_permissions(state_root, fs::Permissions::from_mode(0o710))?;
    }
    let runtime_tmp = layout.runtime.join("tmp");
    for directory in [&layout.workspace, &layout.runtime, &runtime_tmp] {
        fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{chown, PermissionsExt};
            chown(
                directory,
                Some(context.child_identity.0),
                Some(context.child_identity.1),
            )?;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o770))?;
        }
    }
    let identity = context.custody.provision(agent_id)?;
    let agent_keys = context.custody.load(agent_id)?;
    let owner_keys = community_owner_keys(context, &community)?;
    if let Err(error) = buzz_server::relay_projection::sync_agent_profile(
        &community.relay_url,
        &owner_keys,
        &agent_keys,
        &file,
    ) {
        eprintln!("agent profile sync failed for {agent_id}: {error}");
    }
    let agent_pubkey = agent_keys.public_key().to_hex();
    match buzz_server::relay_projection::sync_managed_agent_projection(
        &community.relay_url,
        &owner_keys,
        &agent_keys,
        &resolved,
    ) {
        Ok(()) => context.store.record_relay_projection(
            community.id,
            buzz_server::storage::RelayProjectionKind::ManagedAgent,
            &agent_id.to_string(),
            community.relay_url.as_str(),
            &owner_keys.public_key().to_hex(),
            &agent_pubkey,
            unix_seconds_i64(),
        )?,
        Err(error) => eprintln!("managed-agent projection sync failed for {agent_id}: {error}"),
    }
    if let Some(persona_id) = resolved.persona_id.as_deref() {
        let persona = context.agent_files.load_persona(persona_id)?;
        match buzz_server::relay_projection::sync_persona(
            &community.relay_url,
            &owner_keys,
            &persona,
        ) {
            Ok(()) => context.store.record_relay_projection(
                community.id,
                buzz_server::storage::RelayProjectionKind::Persona,
                &persona.id,
                community.relay_url.as_str(),
                &owner_keys.public_key().to_hex(),
                &buzz_server::relay_projection::persona_d_tag(&persona.id),
                unix_seconds_i64(),
            )?,
            Err(error) => eprintln!("persona projection sync failed for {}: {error}", persona.id),
        }
    }
    let signer = DisposableSigner::from_owner_keys(
        owner_keys.clone(),
        buzz_server::signer::SignerPolicy {
            community_config_id: community.id,
            relay_url: community.relay_url.clone(),
            agent_pubkey: identity.public_key.clone(),
            conditions: context.config.signer_conditions.clone(),
        },
    )?;
    let authorization = signer
        .authorize_agent(&buzz_server::signer::AuthorizeAgentRequest {
            action: "authorize_agent".into(),
            community_config_id: community.id,
            relay_url: community.relay_url.clone(),
            agent_pubkey: identity.public_key,
            conditions: context.config.signer_conditions.clone(),
        })?
        .auth_tag;
    let authorization_generation = secret_generation(&format!(
        "owner={}|community={}|relay={}|agent={}|conditions={}",
        owner_keys.public_key().to_hex(),
        community.id,
        community.relay_url,
        agent_keys.public_key().to_hex(),
        context.config.signer_conditions,
    ));
    let mut dynamic_launch = LaunchSpec::resolve_local(
        &agent,
        &context.config.runtime_catalog,
        LocalLaunchContext {
            launch_id: layout.launch_id.clone(),
            harness: context.config.harness.clone(),
            harness_arguments: context.config.harness_arguments.clone(),
            working_directory: path_string(&context.config.working_directory)?,
            workspace_path: path_string(&layout.workspace)?,
            runtime_path: path_string(&layout.runtime)?,
            process_group_id: layout.launch_id.clone(),
            restart: context.config.restart.clone(),
            health: context.config.health.clone(),
        },
    )?;
    dynamic_launch.environment.insert(
        buzz_server::launch::HARNESS_RELAY_URL_ENV.into(),
        community.relay_url.to_string(),
    );
    dynamic_launch.secret_environment.insert(
        buzz_server::launch::HARNESS_PRIVATE_KEY_ENV.into(),
        SecretRef {
            key: INTERNAL_AGENT_KEY_REFERENCE.into(),
            version: Some(secret_generation(&agent_keys.secret_key().to_secret_hex())),
        },
    );
    dynamic_launch.secret_environment.insert(
        buzz_server::launch::HARNESS_AUTH_TAG_ENV.into(),
        SecretRef {
            key: INTERNAL_AUTHORIZATION_REFERENCE.into(),
            version: Some(authorization_generation),
        },
    );
    apply_resolved_agent_environment(&mut dynamic_launch, &resolved);
    dynamic_launch
        .validate()
        .map_err(buzz_server::LaunchResolutionError::Validation)?;
    let receipts = ReceiptFile::new(layout.receipt.clone());
    let secrets = EnvironmentSecrets {
        authorization,
        agent_private_key: Some(agent_keys.secret_key().to_secret_hex()),
    };
    let reconciler = Reconciler::new(
        context.store.as_ref(),
        &receipts,
        &context.supervisor,
        &secrets,
    );
    let stored_operation = store_operation(operation);
    let outcome = reconciler.reconcile(agent_id, &stored_operation, Some(&dynamic_launch));
    let (mut status, mut error_code) = match outcome {
        Ok(
            buzz_server::reconcile::ReconcileOutcome::FailedPreflight
            | buzz_server::reconcile::ReconcileOutcome::NoPresence,
        ) => (
            buzz_server::OperationStatus::Failed,
            Some(buzz_server::ErrorCode::Internal),
        ),
        Ok(_) => (buzz_server::OperationStatus::Succeeded, None),
        Err(error) => {
            eprintln!("dynamic lifecycle reconciliation failed: {error}");
            (
                buzz_server::OperationStatus::Failed,
                Some(buzz_server::ErrorCode::Internal),
            )
        }
    };
    if status == buzz_server::OperationStatus::Succeeded
        && operation.kind == buzz_server::OperationKind::PurgeAgent
    {
        let scope = buzz_server::storage::RelayProjectionScope {
            community_config_id: community.id,
            relay_url: community.relay_url.to_string(),
            owner_pubkey: owner_keys.public_key().to_hex(),
            d_tag: agent_pubkey.clone(),
        };
        context.store.enqueue_relay_publication(
            buzz_server::storage::RelayPublicationAction::TombstoneManagedAgent,
            &scope,
            &agent_id.to_string(),
            unix_seconds_i64(),
        )?;
        context.store.enqueue_relay_publication(
            buzz_server::storage::RelayPublicationAction::ArchiveIdentity,
            &scope,
            &agent_id.to_string(),
            unix_seconds_i64(),
        )?;
        let _ = context
            .relay_publication_sender
            .send(RelayPublicationWork::Wake);
        if let Err(error) = purge_agent_paths(
            &layout.workspace,
            &layout.runtime,
            &context.config.log_directory,
            &layout.launch_id,
        )
        .and_then(|()| {
            context
                .custody
                .purge(agent_id)
                .map_err(std::io::Error::other)?;
            context
                .agent_files
                .remove_agent(agent_id)
                .map_err(std::io::Error::other)
        }) {
            eprintln!("dynamic purge cleanup failed: {error}");
            status = buzz_server::OperationStatus::Failed;
            error_code = Some(buzz_server::ErrorCode::Internal);
        }
    }
    if finish_operation {
        context
            .application
            .complete_operation(operation.id, status, error_code)?;
    } else if status == buzz_server::OperationStatus::Failed {
        return Err(DaemonError::Task(format!(
            "startup reconciliation failed for dynamic agent {agent_id}: {error_code:?}"
        )));
    }
    Ok(())
}

fn purge_agent_paths(
    workspace: &Path,
    runtime: &Path,
    log_directory: &Path,
    launch_id: &str,
) -> Result<(), std::io::Error> {
    for path in [workspace, runtime] {
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    for suffix in ["stdout.log", "stderr.log"] {
        let path = log_directory.join(format!("{launch_id}.{suffix}"));
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn cleanup_purged_agent_artifacts(context: &ReconcileContext) -> Result<(), DaemonError> {
    for agent_id in context.store.list_purged_agent_ids()? {
        let layout = dynamic_agent_layout(context.config.as_ref(), agent_id)?;
        purge_agent_paths(
            &layout.workspace,
            &layout.runtime,
            &context.config.log_directory,
            &layout.launch_id,
        )?;
        context.custody.purge(agent_id)?;
        if let Some(state_root) = layout.receipt.parent() {
            match fs::remove_dir_all(state_root) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn enqueue_expired_purges<E: LifecycleEffects>(
    application: &SqliteLifecycleApplication<E>,
) -> Result<(), DaemonError> {
    let actor = AuthenticatedPrincipal {
        principal: Principal::UnixPeer {
            uid: 0,
            gid: 0,
            pid: None,
        },
        authority: Authority::Administrator,
    };
    for agent_id in application.expired_retained_agents()? {
        let deadline = application
            .get_agent(agent_id)?
            .purge_after
            .unwrap_or_default();
        let attempt = unix_seconds_i64();
        application.purge_agent(
            &actor,
            &buzz_server::api::AgentCommandRequest {
                metadata: buzz_server::api::CommandMetadata {
                    idempotency_key: format!("retention:{agent_id}:{deadline}:{attempt}"),
                    correlation_id: format!("retention:{agent_id}:{attempt}"),
                },
                agent_id,
            },
        )?;
    }
    Ok(())
}

fn sync_supervisor_logs(
    store: &SqliteStore,
    supervisor: &LocalProcessAdapter,
    agent_id: buzz_server::AgentId,
    launch_id: &str,
) -> Result<(), DaemonError> {
    for (stream, stderr) in [("stdout", false), ("stderr", true)] {
        let message = match supervisor.read_log_tail(launch_id, stderr) {
            Ok(message) => message,
            Err(SupervisorError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if message.is_empty() {
            continue;
        }
        let cursor = format!("{stream}:sha256:{:x}", Sha256::digest(message.as_bytes()));
        store.append_redacted_log(
            agent_id,
            &buzz_server::api::RedactedLogEntry {
                cursor,
                occurred_at: unix_seconds_i64(),
                stream: stream.into(),
                redacted_message: message,
            },
        )?;
    }
    Ok(())
}

fn observe_dynamic_agents(context: &ReconcileContext) -> Result<(), DaemonError> {
    for agent in context.store.list_agents(None)? {
        let layout = dynamic_agent_layout(context.config.as_ref(), agent.id)?;
        let receipts = ReceiptFile::new(layout.receipt);
        if let Some(receipt) = receipts.get_receipt(agent.id)? {
            let observed = context.supervisor.inspect(&receipt)?;
            receipts.put_receipt(&observed)?;
            sync_supervisor_logs(
                context.store.as_ref(),
                &context.supervisor,
                agent.id,
                &layout.launch_id,
            )?;
        }
    }
    Ok(())
}

fn store_operation(resource: &buzz_server::api::OperationResource) -> DurableOperation {
    DurableOperation {
        id: resource.id,
        kind: resource.kind,
        status: resource.status,
        agent_id: resource.agent_id,
        error_code: resource.error_code,
        created_at: resource.created_at,
        updated_at: resource.updated_at,
        correlation_id: resource.correlation_id.clone(),
    }
}

fn agent_file_application_error(
    error: buzz_server::AgentFileError,
) -> buzz_server::api::ApplicationError {
    match error {
        buzz_server::AgentFileError::Validation(error) => {
            buzz_server::api::ApplicationError::Invalid(error)
        }
        buzz_server::AgentFileError::PersonaNotFound(_) => {
            buzz_server::api::ApplicationError::NotFound
        }
        buzz_server::AgentFileError::PersonaReferenced { persona_id, agents } => {
            buzz_server::api::ApplicationError::Conflict(format!(
                "persona {persona_id} is still used by agents {agents}; purge those agents first"
            ))
        }
        buzz_server::AgentFileError::RuntimeRequired(id) => {
            buzz_server::api::ApplicationError::Invalid(buzz_server::ValidationError::new(
                "runtime_id",
                format!("persona {id} has no runtime; provide --runtime"),
            ))
        }
        buzz_server::AgentFileError::StandaloneRuntimeRequired => {
            buzz_server::api::ApplicationError::Invalid(buzz_server::ValidationError::new(
                "runtime_id",
                "is required when no persona is selected",
            ))
        }
        _ => buzz_server::api::ApplicationError::Internal,
    }
}

fn apply_resolved_agent_environment(launch: &mut LaunchSpec, resolved: &ResolvedAgentConfig) {
    launch.environment.insert(
        "BUZZ_ACP_DISPLAY_NAME".into(),
        resolved.spec.display_name.clone(),
    );
    if !resolved.agent_args.is_empty() {
        launch.runtime.arguments.clone_from(&resolved.agent_args);
    }
    if !resolved.spec.system_prompt.is_empty() {
        launch.environment.insert(
            "BUZZ_ACP_SYSTEM_PROMPT".into(),
            resolved.spec.system_prompt.clone(),
        );
    }
    launch
        .environment
        .insert("BUZZ_ACP_AGENTS".into(), resolved.parallelism.to_string());
    launch.environment.insert(
        "BUZZ_ACP_RESPOND_TO".into(),
        resolved.respond_to.as_str().into(),
    );
    if !resolved.respond_to_allowlist.is_empty() {
        launch.environment.insert(
            "BUZZ_ACP_RESPOND_TO_ALLOWLIST".into(),
            resolved.respond_to_allowlist.join(","),
        );
    }
    if let Some(value) = resolved.idle_timeout_seconds {
        launch
            .environment
            .insert("BUZZ_ACP_IDLE_TIMEOUT".into(), value.to_string());
    }
    if let Some(value) = resolved.max_turn_duration_seconds {
        launch
            .environment
            .insert("BUZZ_ACP_MAX_TURN_DURATION".into(), value.to_string());
    }
    if let Some(value) = resolved.model.as_deref() {
        launch
            .environment
            .insert("BUZZ_ACP_MODEL".into(), value.to_owned());
        launch
            .environment
            .entry("BUZZ_AGENT_MODEL".into())
            .or_insert_with(|| value.to_owned());
    }
    if let Some(value) = resolved.provider.as_deref() {
        launch
            .environment
            .entry("BUZZ_AGENT_PROVIDER".into())
            .or_insert_with(|| value.to_owned());
    }
}

#[derive(Debug, thiserror::Error)]
enum DaemonError {
    #[error("usage: buzz-server --config /etc/buzz-server/config.json")]
    Usage,
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("required secret is unavailable: {0}")]
    MissingSecret(String),
    #[error("configured owner secret is invalid")]
    InvalidOwnerSecret,
    #[error("daemon task failed: {0}")]
    Task(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    AgentFile(#[from] buzz_server::AgentFileError),
    #[error(transparent)]
    Catalog(#[from] buzz_server::CatalogError),
    #[error(transparent)]
    Launch(#[from] buzz_server::LaunchResolutionError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error(transparent)]
    Reconcile(#[from] buzz_server::reconcile::ReconcileError),
    #[error(transparent)]
    Application(#[from] buzz_server::api::ApplicationError),
    #[error(transparent)]
    Transport(#[from] buzz_server::transport::TransportError),
    #[error(transparent)]
    SignerProtocol(#[from] buzz_server::signer::SignerError),
    #[error(transparent)]
    Custody(#[from] buzz_server::custody::CustodyError),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_is_strict_and_valid() {
        let source = include_str!("../config/buzz-server.dev.example.json");
        let config: DaemonConfig = serde_json::from_str(source).unwrap();
        config.validate().unwrap();
        let mut value: serde_json::Value = serde_json::from_str(source).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<DaemonConfig>(value).is_err());
    }

    #[test]
    fn dynamic_agent_layout_is_stable_isolated_and_keeps_legacy_paths_untouched() {
        let source = include_str!("../config/buzz-server.dev.example.json");
        let config: DaemonConfig = serde_json::from_str(source).unwrap();
        let first_id = buzz_server::AgentId::new();
        let second_id = buzz_server::AgentId::new();
        let first = dynamic_agent_layout(&config, first_id).unwrap();
        let replay = dynamic_agent_layout(&config, first_id).unwrap();
        let second = dynamic_agent_layout(&config, second_id).unwrap();

        assert_eq!(first.receipt, replay.receipt);
        assert_eq!(first.workspace, replay.workspace);
        assert_ne!(first.receipt, second.receipt);
        assert_ne!(first.workspace, second.workspace);
        assert!(first.workspace.starts_with(
            config
                .state_database
                .parent()
                .unwrap()
                .join("agents")
                .join(first_id.to_string())
        ));
    }
}
