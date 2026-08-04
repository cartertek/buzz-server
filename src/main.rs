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
    community_session::{CommunityAuthorizationVerifier, CommunitySession},
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
    AgentSpec, CommunityConfig, LaunchSpec, LocalLaunchContext, ProcessReceipt, RuntimeCatalog,
    SqliteStore, StorageError,
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
        ] {
            if !path.is_absolute() {
                return Err(DaemonError::InvalidConfig(
                    "all daemon paths must be absolute".into(),
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
        PublicKey::from_hex(&self.expected_agent_pubkey).map_err(|_| {
            DaemonError::InvalidConfig("expected_agent_pubkey must be lowercase Nostr hex".into())
        })?;
        Ok(())
    }
}

const INTERNAL_AUTHORIZATION_REFERENCE: &str = "internal:signer-issued-authorization";

struct EnvironmentSecrets {
    authorization: String,
}

impl SecretResolver for EnvironmentSecrets {
    fn resolve(&self, reference: &SecretRef) -> Result<String, SupervisorError> {
        if reference.key == INTERNAL_AUTHORIZATION_REFERENCE {
            return Ok(self.authorization.clone());
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
    let config_path = parse_args()?;
    let config = DaemonConfig::load(&config_path)?;
    let now = unix_seconds()?;
    let ready_path = config.signer_socket.with_file_name("ready");

    for directory in [
        config.state_database.parent(),
        config.receipt_file.parent(),
        config.signer_socket.parent(),
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
    let signer_policy = buzz_server::signer::SignerPolicy {
        community_config_id: config.community.id,
        relay_url: config.community.relay_url.clone(),
        agent_pubkey: config.expected_agent_pubkey.clone(),
        conditions: config.signer_conditions.clone(),
    };
    let constrained_signer = DisposableSigner::from_owner_keys(owner_keys, signer_policy)?;
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
            version: Some(secret_generation(&authorization)),
        },
    );
    launch
        .validate()
        .map_err(buzz_server::LaunchResolutionError::Validation)?;

    let store = SqliteStore::open(&config.state_database)?;
    store.put_community(&config.community, now as i64)?;
    store.put_agent(&config.agent, now as i64)?;
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
    };
    let reconciler = Reconciler::new(&store, &receipts, &supervisor, &secrets);
    let outcome = reconciler.reconcile_desired(&config.agent, Some(&launch))?;
    eprintln!("local reconciliation: {outcome:?}");

    let signer_server = SignerIpcServer::new(&config.signer_socket, Arc::new(constrained_signer));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
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
    let mut restart_attempts = 0_u32;
    let mut next_restart = Instant::now();

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    enum Completion<T, S> {
        Relay(T),
        Signer(S),
        Signal,
    }
    let completion = loop {
        tokio::select! {
            result = &mut relay_task => break Completion::Relay(result),
            result = &mut signer_task => break Completion::Signer(result),
            signal = tokio::signal::ctrl_c() => { signal?; break Completion::Signal },
            _ = terminate.recv() => break Completion::Signal,
            _ = process_tick.tick() => {},
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
        let should_restart = match observed.as_ref() {
            Some(receipt) if receipt.observed_state.is_terminal() => match config.restart.mode {
                RestartMode::Never => false,
                RestartMode::OnFailure => receipt.exit_code != Some(0),
                RestartMode::Always => true,
            },
            None => true,
            Some(_) => false,
        };
        if should_restart
            && restart_attempts < config.restart.max_attempts
            && Instant::now() >= next_restart
        {
            let outcome = reconciler.reconcile_desired(&config.agent, Some(&launch))?;
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
        Completion::Signal => {
            relay_task.await?;
            signer_task
                .await
                .map_err(|error| DaemonError::Task(error.to_string()))??;
        }
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
    Community(#[from] buzz_server::community_session::CommunitySessionError),
    #[error(transparent)]
    Relay(#[from] buzz_server::relay_adapter::RelayAdapterError),
    #[error(transparent)]
    Signer(#[from] buzz_server::signer_ipc::SignerIpcError),
    #[error(transparent)]
    SignerProtocol(#[from] buzz_server::signer::SignerError),
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
