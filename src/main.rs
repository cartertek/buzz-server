//! Minimal Buzz Server development daemon.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use buzz_core::{Keys, PublicKey};
use buzz_server::{
    api::LifecycleApplication,
    application::{LifecycleEffects, SqliteLifecycleApplication},
    auth::{
        AuthenticatedPrincipal, Authority, Nip98AuthorityPolicy, Principal, UnixAuthorityPolicy,
    },
    community_session::{CommunityAuthorizationVerifier, CommunitySession},
    custody::{AgentIdentityCustody, FilesystemAgentIdentityCustody},
    launch::{ExecutableIdentity, HealthPolicy, RestartMode, RestartPolicy, SecretRef},
    reconcile::{ProcessReceiptRepository, Reconciler},
    relay_adapter::{
        BuzzWsFactory, CommunityRelayAdapter, RelayAdapterConfig, RelayAdapterObserver,
        SystemRelayClock,
    },
    signer::DisposableSigner,
    signer_ipc::SignerIpcServer,
    supervisor::{
        LocalLogPolicy, LocalProcessAdapter, ProcessSupervisor, SecretResolver, SupervisorError,
    },
    transport::{
        LifecycleJsonRouter, SqliteReplayGuard, TlsLifecycleServer, TlsNip98Authenticator,
        UnixLifecycleServer,
    },
    AgentSpec, CommunityConfig, DurableOperation, LaunchSpec, LocalLaunchContext, ProcessReceipt,
    RuntimeCatalog, SqliteStore, StorageError,
};
use nostr::Tag;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonConfig {
    state_database: PathBuf,
    receipt_file: PathBuf,
    signer_socket: PathBuf,
    log_directory: PathBuf,
    working_directory: PathBuf,
    workspace_path: PathBuf,
    runtime_path: PathBuf,
    agent_secret_env: String,
    owner_secret_file: PathBuf,
    runtime_user: String,
    expected_agent_pubkey: String,
    signer_conditions: String,
    community: CommunityConfig,
    agent: AgentSpec,
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
        self.community
            .validate()
            .map_err(|error| DaemonError::InvalidConfig(error.to_string()))?;
        self.agent
            .validate()
            .map_err(|error| DaemonError::InvalidConfig(error.to_string()))?;
        self.runtime_catalog.validate()?;
        if self.agent.community_config_id != self.community.id {
            return Err(DaemonError::InvalidConfig(
                "agent community does not match configured community".into(),
            ));
        }
        for path in [
            &self.state_database,
            &self.receipt_file,
            &self.signer_socket,
            &self.log_directory,
            &self.working_directory,
            &self.workspace_path,
            &self.runtime_path,
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
        if !valid_env_name(&self.agent_secret_env) {
            return Err(DaemonError::InvalidConfig(
                "credential references must be environment variable names".into(),
            ));
        }
        if self.owner_secret_file.parent()
            != Some(Path::new("/run/credentials/buzz-server.service"))
        {
            return Err(DaemonError::InvalidConfig(
                "owner_secret_file must be inside the systemd credential directory".into(),
            ));
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
                "receipt_file",
                &self.receipt_file,
                Path::new("/var/lib/buzz-server"),
            ),
            (
                "signer_socket",
                &self.signer_socket,
                Path::new("/run/buzz-server"),
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
            (
                "workspace_path",
                &self.workspace_path,
                Path::new("/var/lib/buzz-server"),
            ),
            (
                "runtime_path",
                &self.runtime_path,
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
        if self.workspace_path == Path::new("/var/lib/buzz-server")
            || self.runtime_path == Path::new("/var/lib/buzz-server")
            || self.workspace_path == self.runtime_path
        {
            return Err(DaemonError::InvalidConfig(
                "workspace_path and runtime_path must be distinct agent-specific subdirectories"
                    .into(),
            ));
        }
        PublicKey::from_hex(&self.expected_agent_pubkey).map_err(|_| {
            DaemonError::InvalidConfig("expected_agent_pubkey must be lowercase Nostr hex".into())
        })?;
        Ok(())
    }
}

#[derive(Clone)]
struct LifecycleWake(tokio::sync::mpsc::UnboundedSender<buzz_server::OperationId>);

impl LifecycleEffects for LifecycleWake {
    fn operation_ready(
        &self,
        operation: &DurableOperation,
    ) -> Result<(), buzz_server::api::ApplicationError> {
        self.0
            .send(operation.id)
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

struct SharedAuthorizationVerifier;

impl CommunityAuthorizationVerifier for SharedAuthorizationVerifier {
    type Error = ();

    fn verify(
        &self,
        _: buzz_server::CommunityConfigId,
        expected_agent: &PublicKey,
        authorization: &str,
    ) -> Result<(), Self::Error> {
        buzz_sdk::nip_oa::verify_auth_tag(authorization, expected_agent)
            .map(|_| ())
            .map_err(|_| ())
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

struct StatusObserver {
    readiness: Arc<CombinedReadiness>,
}

struct CombinedReadiness {
    ready_path: PathBuf,
    relay_ready: AtomicBool,
    process_ready: AtomicBool,
}

impl CombinedReadiness {
    fn update_file(&self) {
        if self.relay_ready.load(Ordering::Acquire) && self.process_ready.load(Ordering::Acquire) {
            let _ = fs::write(&self.ready_path, b"ready\n");
        } else {
            let _ = fs::remove_file(&self.ready_path);
        }
    }

    fn set_process_ready(&self, ready: bool) {
        self.process_ready.store(ready, Ordering::Release);
        self.update_file();
    }
}

impl RelayAdapterObserver for StatusObserver {
    fn readiness_changed(&mut self, readiness: buzz_server::community_session::CommunityReadiness) {
        eprintln!("community readiness: {readiness:?}");
        self.readiness.relay_ready.store(
            readiness == buzz_server::community_session::CommunityReadiness::Ready,
            Ordering::Release,
        );
        self.readiness.update_file();
    }

    fn transport_error(&mut self, message: &str) {
        eprintln!("relay transport error: {message}");
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
    let now = unix_seconds()?;
    let ready_path = config.signer_socket.with_file_name("ready");

    for directory in [
        config.state_database.parent(),
        config.receipt_file.parent(),
        config.signer_socket.parent(),
        config.lifecycle_api.unix_socket.parent(),
        Some(config.log_directory.as_path()),
        Some(config.workspace_path.as_path()),
        Some(config.runtime_path.as_path()),
    ] {
        fs::create_dir_all(
            directory.ok_or_else(|| {
                DaemonError::InvalidConfig("configured path has no parent".into())
            })?,
        )?;
    }
    let child_identity = resolve_user(&config.runtime_user)?;
    #[cfg(unix)]
    for directory in [&config.workspace_path, &config.runtime_path] {
        use std::os::unix::fs::{chown, PermissionsExt};
        chown(directory, Some(child_identity.0), Some(child_identity.1))?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    if let Some(signer_directory) = config.signer_socket.parent() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(signer_directory, fs::Permissions::from_mode(0o700))?;
        }
    }
    if let Some(lifecycle_directory) = config.lifecycle_api.unix_socket.parent() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The socket authenticates every caller with SO_PEERCRED. Non-root configured draft
            // submitters need directory traversal, but no caller may modify directory entries.
            fs::set_permissions(lifecycle_directory, fs::Permissions::from_mode(0o711))?;
        }
    }
    clear_readiness_file(&ready_path)?;

    let mut launch = LaunchSpec::resolve_local(
        &config.agent,
        &config.runtime_catalog,
        LocalLaunchContext {
            launch_id: format!("agent-{}", config.agent.id),
            harness: config.harness.clone(),
            harness_arguments: config.harness_arguments.clone(),
            working_directory: path_string(&config.working_directory)?,
            workspace_path: path_string(&config.workspace_path)?,
            runtime_path: path_string(&config.runtime_path)?,
            process_group_id: format!("agent-{}", config.agent.id),
            restart: config.restart.clone(),
            health: config.health.clone(),
        },
    )?;
    let agent_secret = read_secret_env(&config.agent_secret_env)?;
    let agent_secret_generation = secret_generation(&agent_secret);
    let agent_keys = Keys::parse(&agent_secret).map_err(|_| DaemonError::InvalidAgentSecret)?;
    drop(agent_secret);
    if agent_keys.public_key().to_hex() != config.expected_agent_pubkey {
        return Err(DaemonError::InvalidAgentSecret);
    }
    let owner_secret = read_secret_file(&config.owner_secret_file)?;
    let owner_keys = Keys::parse(&owner_secret).map_err(|_| DaemonError::InvalidOwnerSecret)?;
    drop(owner_secret);
    let authorization_generation = secret_generation(&format!(
        "owner={}|community={}|relay={}|agent={}|conditions={}",
        owner_keys.public_key().to_hex(),
        config.community.id,
        config.community.relay_url,
        config.expected_agent_pubkey,
        config.signer_conditions,
    ));
    let signer_policy = buzz_server::signer::SignerPolicy {
        community_config_id: config.community.id,
        relay_url: config.community.relay_url.clone(),
        agent_pubkey: config.expected_agent_pubkey.clone(),
        conditions: config.signer_conditions.clone(),
    };
    let constrained_signer = DisposableSigner::from_owner_keys(owner_keys.clone(), signer_policy)?;
    let authorization = constrained_signer
        .authorize_agent(&buzz_server::signer::AuthorizeAgentRequest {
            action: "authorize_agent".into(),
            community_config_id: config.community.id,
            relay_url: config.community.relay_url.clone(),
            agent_pubkey: config.expected_agent_pubkey.clone(),
            conditions: config.signer_conditions.clone(),
        })?
        .auth_tag;
    let auth_parts = buzz_sdk::nip_oa::parse_auth_tag(&authorization)
        .map_err(|_| DaemonError::InvalidAuthorization)?;
    let auth_tag = Tag::parse(auth_parts).map_err(|_| DaemonError::InvalidAuthorization)?;
    let mut community_session = CommunitySession::new(
        config.community.id,
        config.community.relay_url.clone(),
        agent_keys.public_key(),
        now,
    )?;
    community_session.verify_authorization(&SharedAuthorizationVerifier, &authorization)?;
    launch.environment.insert(
        buzz_server::launch::HARNESS_RELAY_URL_ENV.into(),
        config.community.relay_url.to_string(),
    );
    launch.secret_environment.insert(
        buzz_server::launch::HARNESS_PRIVATE_KEY_ENV.into(),
        SecretRef {
            key: config.agent_secret_env.clone(),
            version: Some(agent_secret_generation),
        },
    );
    launch.secret_environment.insert(
        buzz_server::launch::HARNESS_AUTH_TAG_ENV.into(),
        SecretRef {
            key: INTERNAL_AUTHORIZATION_REFERENCE.into(),
            // The signed tag carries a fresh timestamp and signature on every
            // daemon start. Its generation is the stable signer authority and
            // policy, so an equivalent re-sign does not look like process
            // identity drift and defeat restart adoption.
            version: Some(authorization_generation),
        },
    );
    launch
        .validate()
        .map_err(buzz_server::LaunchResolutionError::Validation)?;

    let store = Arc::new(SqliteStore::open(&config.state_database)?);
    store.put_community(&config.community, now as i64)?;
    if store.get_agent(config.agent.id)?.is_none() && !store.is_agent_purged(config.agent.id)? {
        store.put_agent(&config.agent, now as i64)?;
    }
    let supervisor = LocalProcessAdapter::new(
        LocalLogPolicy {
            directory: config.log_directory.clone(),
            max_file_bytes: 10 * 1024 * 1024,
            max_read_bytes: 64 * 1024,
        },
        Duration::from_secs(10),
        Some(child_identity),
    )?;
    let receipts = ReceiptFile::new(config.receipt_file.clone());
    let secrets = EnvironmentSecrets {
        authorization: authorization.clone(),
        agent_private_key: None,
    };
    let reconciler = Reconciler::new(store.as_ref(), &receipts, &supervisor, &secrets);
    let custody_root = config
        .state_database
        .parent()
        .ok_or_else(|| DaemonError::InvalidConfig("state database has no parent".into()))?
        .join("identities");
    let custody = FilesystemAgentIdentityCustody::new(custody_root, 0);
    let (operation_tx, mut operation_rx) = tokio::sync::mpsc::unbounded_channel();
    let lifecycle_application = SqliteLifecycleApplication::new(
        Arc::clone(&store),
        Arc::new(LifecycleWake(operation_tx.clone())),
        unix_seconds_i64,
    )
    .with_retention_seconds(config.lifecycle_api.retention_seconds);
    // Resume durable commands before applying steady-state desired intent. Otherwise an old
    // disable/delete can be temporarily undone by startup reconciliation.
    for operation in store.nonterminal_operations()? {
        reconcile_lifecycle_operation(
            &lifecycle_application,
            &reconciler,
            store.as_ref(),
            operation.id,
            config.agent.id,
            &mut launch,
            &config,
            &owner_keys,
            &custody,
            &supervisor,
            child_identity,
        )?;
    }
    if let Some(agent) = store.get_agent(config.agent.id)? {
        let outcome = reconciler.reconcile_desired(&agent, Some(&launch))?;
        eprintln!("local reconciliation: {outcome:?}");
    }
    for agent in store.list_agents(None)? {
        if agent.id == config.agent.id {
            continue;
        }
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
        reconcile_dynamic_lifecycle_operation(
            &lifecycle_application,
            store.as_ref(),
            &startup,
            &config,
            &owner_keys,
            &custody,
            &supervisor,
            child_identity,
            false,
        )?;
    }

    let signer_server = SignerIpcServer::new(&config.signer_socket, Arc::new(constrained_signer));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
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
    let mut signer_task = tokio::spawn({
        let signer_shutdown = shutdown_rx.clone();
        async move { signer_server.run(signer_shutdown).await }
    });
    let relay = CommunityRelayAdapter {
        factory: BuzzWsFactory {
            keys: agent_keys,
            authorization_tag: Some(auth_tag),
        },
        clock: SystemRelayClock,
        config: RelayAdapterConfig::default(),
    };
    let readiness = Arc::new(CombinedReadiness {
        ready_path: ready_path.clone(),
        relay_ready: AtomicBool::new(false),
        process_ready: AtomicBool::new(false),
    });
    let mut observer = StatusObserver {
        readiness: Arc::clone(&readiness),
    };
    let relay_task = relay.run(&mut community_session, &mut observer, shutdown_rx);
    tokio::pin!(relay_task);
    let mut process_tick = tokio::time::interval(Duration::from_secs(1));
    process_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut retention_tick = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
    retention_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut restart_attempts = 0_u32;
    let mut next_restart = Instant::now();

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    enum Completion<T, S> {
        Relay(T),
        Signer(S),
        Lifecycle(Result<(), buzz_server::transport::TransportError>),
        Signal,
    }
    let completion = loop {
        tokio::select! {
            result = &mut relay_task => break Completion::Relay(result),
            result = &mut signer_task => break Completion::Signer(result),
            result = &mut unix_lifecycle_task => break Completion::Lifecycle(result.map_err(|error| buzz_server::transport::TransportError::Task(error.to_string()))?),
            result = async { tls_lifecycle_task.as_mut().expect("guarded").await }, if tls_lifecycle_task.is_some() => break Completion::Lifecycle(result.map_err(|error| buzz_server::transport::TransportError::Task(error.to_string()))?),
            signal = tokio::signal::ctrl_c() => { signal?; break Completion::Signal },
            _ = terminate.recv() => break Completion::Signal,
            Some(operation_id) = operation_rx.recv() => {
                reconcile_lifecycle_operation(
                    &lifecycle_application,
                    &reconciler,
                    &store,
                    operation_id,
                    config.agent.id,
                    &mut launch,
                    &config,
                    &owner_keys,
                    &custody,
                    &supervisor,
                    child_identity,
                )?;
            }
            _ = process_tick.tick() => {
                observe_dynamic_agents(store.as_ref(), &supervisor, &config)?;
            },
            _ = retention_tick.tick() => {
                enqueue_expired_purges(&lifecycle_application)?;
            },
        }
        let mut process_ready = false;
        let durable = receipts.get_receipt(config.agent.id)?;
        let observed = durable
            .as_ref()
            .map(|receipt| supervisor.inspect(receipt))
            .transpose()?;
        if let Some(observed) = &observed {
            receipts.put_receipt(observed)?;
            process_ready = observed.observed_state == buzz_server::ObservedProcessState::Healthy;
            if process_ready
                && unix_millis()?.saturating_sub(observed.started_at_unix_ms)
                    >= config.restart.stable_after_ms
            {
                restart_attempts = 0;
            }
        }
        readiness.set_process_ready(process_ready);
        let desired_agent = store.get_agent(config.agent.id)?;
        if desired_agent.is_some() {
            sync_supervisor_logs(&store, &supervisor, config.agent.id, &launch.launch_id)?;
        }
        let should_restart = desired_agent
            .as_ref()
            .is_some_and(|agent| agent.desired_state == buzz_server::DesiredAgentState::Enabled)
            && match observed.as_ref() {
                Some(receipt) if receipt.observed_state.is_terminal() => {
                    match config.restart.mode {
                        RestartMode::Never => false,
                        RestartMode::OnFailure => receipt.exit_code != Some(0),
                        RestartMode::Always => true,
                    }
                }
                None => true,
                Some(_) => false,
            };
        if should_restart
            && restart_attempts < config.restart.max_attempts
            && Instant::now() >= next_restart
        {
            let outcome = reconciler.reconcile_desired(
                desired_agent
                    .as_ref()
                    .expect("restart requires desired agent"),
                Some(&launch),
            )?;
            restart_attempts = restart_attempts.saturating_add(1);
            let shift = restart_attempts.saturating_sub(1).min(31);
            let delay = config
                .restart
                .initial_backoff_ms
                .saturating_mul(1_u64 << shift)
                .min(config.restart.max_backoff_ms);
            next_restart = Instant::now() + Duration::from_millis(delay);
            eprintln!("local reconciliation: {outcome:?}");
        }
    };
    let _ = shutdown_tx.send(true);
    let _ = fs::remove_file(&ready_path);
    let intentional_shutdown = matches!(completion, Completion::Signal);
    if should_stop_managed_agent(intentional_shutdown) {
        if let Some(receipt) = receipts.get_receipt(config.agent.id)? {
            let stopped = supervisor.stop(&receipt)?;
            receipts.put_receipt(&stopped)?;
        }
    }
    match completion {
        Completion::Relay(result) => {
            result?;
            signer_task
                .await
                .map_err(|error| DaemonError::Task(error.to_string()))??;
        }
        Completion::Signer(result) => {
            result.map_err(|error| DaemonError::Task(error.to_string()))??;
            relay_task.await?;
        }
        Completion::Lifecycle(result) => {
            result?;
            relay_task.await?;
            signer_task
                .await
                .map_err(|error| DaemonError::Task(error.to_string()))??;
        }
        Completion::Signal => {
            relay_task.await?;
            signer_task
                .await
                .map_err(|error| DaemonError::Task(error.to_string()))??;
        }
    }
    unix_lifecycle_task.abort();
    if let Some(task) = tls_lifecycle_task {
        task.abort();
    }
    Ok(())
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

fn read_secret_env(name: &str) -> Result<String, DaemonError> {
    std::env::var(name).map_err(|_| DaemonError::MissingSecret(name.to_owned()))
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

fn valid_env_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some('A'..='Z' | '_'))
        && characters.all(|character| matches!(character, 'A'..='Z' | '0'..='9' | '_'))
}

fn should_stop_managed_agent(intentional_shutdown: bool) -> bool {
    !intentional_shutdown
}

fn path_string(path: &Path) -> Result<String, DaemonError> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| DaemonError::InvalidConfig("paths must be UTF-8".into()))
}

fn unix_seconds() -> Result<u64, DaemonError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| DaemonError::Clock)?
        .as_secs())
}

fn unix_seconds_i64() -> i64 {
    i64::try_from(unix_seconds_unchecked()).unwrap_or(i64::MAX)
}

fn unix_seconds_unchecked() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[allow(clippy::too_many_arguments)]
fn reconcile_lifecycle_operation<E: LifecycleEffects>(
    application: &SqliteLifecycleApplication<E>,
    reconciler: &Reconciler<'_, SqliteStore, ReceiptFile, LocalProcessAdapter>,
    store: &SqliteStore,
    operation_id: buzz_server::OperationId,
    configured_agent_id: buzz_server::AgentId,
    launch: &mut LaunchSpec,
    config: &DaemonConfig,
    owner_keys: &Keys,
    custody: &FilesystemAgentIdentityCustody,
    supervisor: &LocalProcessAdapter,
    child_identity: (u32, u32),
) -> Result<(), DaemonError> {
    let operation = application.get_operation(operation_id)?;
    if operation.status == buzz_server::OperationStatus::Pending {
        application.start_operation(operation_id)?;
    }
    let durable = application.get_operation(operation_id)?;
    if durable.status != buzz_server::OperationStatus::Running {
        return Ok(());
    }
    let target_agent_id = durable.agent_id.ok_or(StorageError::NotFound)?;
    if target_agent_id != configured_agent_id {
        return reconcile_dynamic_lifecycle_operation(
            application,
            store,
            &durable,
            config,
            owner_keys,
            custody,
            supervisor,
            child_identity,
            true,
        );
    }
    let agent = store
        .get_agent(configured_agent_id)?
        .ok_or(StorageError::NotFound)?;
    let mut next_launch = LaunchSpec::resolve_local(
        &agent,
        &config.runtime_catalog,
        LocalLaunchContext {
            launch_id: launch.launch_id.clone(),
            harness: config.harness.clone(),
            harness_arguments: config.harness_arguments.clone(),
            working_directory: launch.working_directory.clone(),
            workspace_path: launch.workspace_path.clone(),
            runtime_path: launch.runtime_path.clone(),
            process_group_id: launch.process_group_id.clone(),
            restart: config.restart.clone(),
            health: config.health.clone(),
        },
    )?;
    for key in [
        buzz_server::launch::HARNESS_RELAY_URL_ENV,
        buzz_server::launch::HARNESS_PRIVATE_KEY_ENV,
        buzz_server::launch::HARNESS_AUTH_TAG_ENV,
    ] {
        if let Some(value) = launch.environment.get(key) {
            next_launch.environment.insert(key.into(), value.clone());
        }
        if let Some(value) = launch.secret_environment.get(key) {
            next_launch
                .secret_environment
                .insert(key.into(), value.clone());
        }
    }
    next_launch
        .validate()
        .map_err(buzz_server::LaunchResolutionError::Validation)?;
    *launch = next_launch;
    let stored = reconciler.reconcile(
        configured_agent_id,
        &store_operation(&durable),
        Some(launch),
    );
    let (mut status, mut error_code) = match stored {
        Ok(
            buzz_server::reconcile::ReconcileOutcome::FailedPreflight
            | buzz_server::reconcile::ReconcileOutcome::NoPresence,
        ) => (
            buzz_server::OperationStatus::Failed,
            Some(buzz_server::ErrorCode::Internal),
        ),
        Ok(_) => (buzz_server::OperationStatus::Succeeded, None),
        Err(error) => {
            eprintln!("lifecycle reconciliation failed: {error}");
            (
                buzz_server::OperationStatus::Failed,
                Some(buzz_server::ErrorCode::Internal),
            )
        }
    };
    if status == buzz_server::OperationStatus::Succeeded
        && durable.kind == buzz_server::OperationKind::PurgeAgent
    {
        if let Err(error) = purge_agent_files(config, &launch.launch_id) {
            eprintln!("purge cleanup failed: {error}");
            status = buzz_server::OperationStatus::Failed;
            error_code = Some(buzz_server::ErrorCode::Internal);
        }
    }
    application.complete_operation(operation_id, status, error_code)?;
    Ok(())
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

#[allow(clippy::too_many_arguments)]
fn reconcile_dynamic_lifecycle_operation<E: LifecycleEffects>(
    application: &SqliteLifecycleApplication<E>,
    store: &SqliteStore,
    operation: &buzz_server::api::OperationResource,
    config: &DaemonConfig,
    owner_keys: &Keys,
    custody: &FilesystemAgentIdentityCustody,
    supervisor: &LocalProcessAdapter,
    child_identity: (u32, u32),
    finish_operation: bool,
) -> Result<(), DaemonError> {
    let agent_id = operation.agent_id.ok_or(StorageError::NotFound)?;
    let agent = store.get_agent(agent_id)?.ok_or(StorageError::NotFound)?;
    let community = store
        .get_community(agent.community_config_id)?
        .ok_or(StorageError::NotFound)?;
    let layout = dynamic_agent_layout(config, agent_id)?;
    for directory in [&layout.workspace, &layout.runtime] {
        fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{chown, PermissionsExt};
            chown(directory, Some(child_identity.0), Some(child_identity.1))?;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
    }
    let identity = custody.provision(agent_id)?;
    let agent_keys = custody.load(agent_id)?;
    let signer = DisposableSigner::from_owner_keys(
        owner_keys.clone(),
        buzz_server::signer::SignerPolicy {
            community_config_id: community.id,
            relay_url: community.relay_url.clone(),
            agent_pubkey: identity.public_key.clone(),
            conditions: config.signer_conditions.clone(),
        },
    )?;
    let authorization = signer
        .authorize_agent(&buzz_server::signer::AuthorizeAgentRequest {
            action: "authorize_agent".into(),
            community_config_id: community.id,
            relay_url: community.relay_url.clone(),
            agent_pubkey: identity.public_key,
            conditions: config.signer_conditions.clone(),
        })?
        .auth_tag;
    let authorization_generation = secret_generation(&format!(
        "owner={}|community={}|relay={}|agent={}|conditions={}",
        owner_keys.public_key().to_hex(),
        community.id,
        community.relay_url,
        agent_keys.public_key().to_hex(),
        config.signer_conditions,
    ));
    let mut dynamic_launch = LaunchSpec::resolve_local(
        &agent,
        &config.runtime_catalog,
        LocalLaunchContext {
            launch_id: layout.launch_id.clone(),
            harness: config.harness.clone(),
            harness_arguments: config.harness_arguments.clone(),
            working_directory: path_string(&config.working_directory)?,
            workspace_path: path_string(&layout.workspace)?,
            runtime_path: path_string(&layout.runtime)?,
            process_group_id: layout.launch_id.clone(),
            restart: config.restart.clone(),
            health: config.health.clone(),
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
    dynamic_launch
        .validate()
        .map_err(buzz_server::LaunchResolutionError::Validation)?;
    let receipts = ReceiptFile::new(layout.receipt.clone());
    let secrets = EnvironmentSecrets {
        authorization,
        agent_private_key: Some(agent_keys.secret_key().to_secret_hex()),
    };
    let reconciler = Reconciler::new(store, &receipts, supervisor, &secrets);
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
        if let Err(error) = purge_agent_paths(
            &layout.workspace,
            &layout.runtime,
            &config.log_directory,
            &layout.launch_id,
        )
        .and_then(|()| custody.purge(agent_id).map_err(std::io::Error::other))
        {
            eprintln!("dynamic purge cleanup failed: {error}");
            status = buzz_server::OperationStatus::Failed;
            error_code = Some(buzz_server::ErrorCode::Internal);
        }
    }
    if finish_operation {
        application.complete_operation(operation.id, status, error_code)?;
    } else if status == buzz_server::OperationStatus::Failed {
        return Err(DaemonError::Task(format!(
            "startup reconciliation failed for dynamic agent {agent_id}: {error_code:?}"
        )));
    }
    Ok(())
}

fn purge_agent_files(config: &DaemonConfig, launch_id: &str) -> Result<(), std::io::Error> {
    purge_agent_paths(
        &config.workspace_path,
        &config.runtime_path,
        &config.log_directory,
        launch_id,
    )
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

fn observe_dynamic_agents(
    store: &SqliteStore,
    supervisor: &LocalProcessAdapter,
    config: &DaemonConfig,
) -> Result<(), DaemonError> {
    for agent in store.list_agents(None)? {
        if agent.id == config.agent.id {
            continue;
        }
        let layout = dynamic_agent_layout(config, agent.id)?;
        let receipts = ReceiptFile::new(layout.receipt);
        if let Some(receipt) = receipts.get_receipt(agent.id)? {
            let observed = supervisor.inspect(&receipt)?;
            receipts.put_receipt(&observed)?;
            sync_supervisor_logs(store, supervisor, agent.id, &layout.launch_id)?;
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

fn unix_millis() -> Result<u64, DaemonError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| DaemonError::Clock)?
            .as_millis(),
    )
    .map_err(|_| DaemonError::Clock)
}

fn clear_readiness_file(path: &Path) -> Result<(), std::io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Debug, thiserror::Error)]
enum DaemonError {
    #[error("usage: buzz-server --config /etc/buzz-server/config.json")]
    Usage,
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("required secret environment variable is unavailable: {0}")]
    MissingSecret(String),
    #[error("configured agent secret does not match the configured agent identity")]
    InvalidAgentSecret,
    #[error("configured owner secret is invalid")]
    InvalidOwnerSecret,
    #[error("authorization tag is invalid")]
    InvalidAuthorization,
    #[error("system clock is before the Unix epoch")]
    Clock,
    #[error("daemon task failed: {0}")]
    Task(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Storage(#[from] StorageError),
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
    Community(#[from] buzz_server::community_session::CommunitySessionError),
    #[error(transparent)]
    Relay(#[from] buzz_server::relay_adapter::RelayAdapterError),
    #[error(transparent)]
    Signer(#[from] buzz_server::signer_ipc::SignerIpcError),
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
        assert_eq!(config.community.id.as_uuid().get_version_num(), 7);
        assert_eq!(config.agent.id.as_uuid().get_version_num(), 7);
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
        assert_ne!(first.runtime, config.runtime_path);
        assert_ne!(first.receipt, config.receipt_file);
        assert!(first.workspace.starts_with(
            config
                .state_database
                .parent()
                .unwrap()
                .join("agents")
                .join(first_id.to_string())
        ));
    }

    #[test]
    fn readiness_file_only_exists_while_session_is_ready() {
        let directory = tempfile::tempdir().unwrap();
        let ready_path = directory.path().join("ready");
        let mut observer = StatusObserver {
            readiness: Arc::new(CombinedReadiness {
                ready_path: ready_path.clone(),
                relay_ready: AtomicBool::new(false),
                process_ready: AtomicBool::new(true),
            }),
        };
        observer.readiness_changed(buzz_server::community_session::CommunityReadiness::Pending);
        assert!(!ready_path.exists());
        observer.readiness_changed(buzz_server::community_session::CommunityReadiness::Ready);
        assert!(ready_path.exists());
        observer.readiness_changed(buzz_server::community_session::CommunityReadiness::Degraded);
        assert!(!ready_path.exists());
    }

    #[test]
    fn startup_clears_stale_readiness_from_an_unclean_exit() {
        let directory = tempfile::tempdir().unwrap();
        let ready_path = directory.path().join("ready");
        fs::write(&ready_path, b"ready\n").unwrap();

        clear_readiness_file(&ready_path).unwrap();

        assert!(!ready_path.exists());
        clear_readiness_file(&ready_path).unwrap();
    }

    #[test]
    fn intentional_daemon_restart_preserves_the_managed_agent_for_adoption() {
        assert!(!should_stop_managed_agent(true));
        assert!(should_stop_managed_agent(false));
    }
}
