//! Safe direct-process supervision for the built-in Local backend.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};

use crate::{
    launch::{HealthPolicy, SecretRef},
    LaunchSpec, ObservedProcessState, ProcessReceipt, ValidationError,
};

const LAUNCH_MARKER: &str = "BUZZ_SERVER_LAUNCH_ID";
const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub trait SecretResolver {
    /// Resolve an opaque reference at the final spawn boundary.
    fn resolve(&self, reference: &SecretRef) -> Result<String, SupervisorError>;
}

pub trait ProcessSupervisor {
    fn start(
        &self,
        desired: &LaunchSpec,
        secrets: &dyn SecretResolver,
    ) -> Result<ProcessReceipt, SupervisorError>;
    fn inspect(&self, receipt: &ProcessReceipt) -> Result<ProcessReceipt, SupervisorError>;
    fn stop(&self, receipt: &ProcessReceipt) -> Result<ProcessReceipt, SupervisorError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error(transparent)]
    InvalidSpec(#[from] ValidationError),
    #[error("secret resolution failed")]
    SecretResolution,
    #[error("runtime preflight failed: {0}")]
    Preflight(String),
    #[error("process receipt is not owned by this launch")]
    ReceiptMismatch,
    #[error("process operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("process lock is poisoned")]
    LockPoisoned,
    #[error("system clock precedes the Unix epoch")]
    Clock,
}

/// File-backed log policy. Files are bounded before every launch and reads are
/// tail-bounded and redacted. The supervisor never captures a secret value in
/// an error or diagnostic string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalLogPolicy {
    pub directory: PathBuf,
    pub max_file_bytes: u64,
    pub max_read_bytes: usize,
}

impl LocalLogPolicy {
    pub fn validate(&self) -> Result<(), SupervisorError> {
        if !self.directory.is_absolute()
            || self.max_file_bytes == 0
            || self.max_read_bytes == 0
            || self.max_read_bytes as u64 > self.max_file_bytes
        {
            return Err(SupervisorError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid local log policy",
            )));
        }
        Ok(())
    }
}

pub struct LocalProcessAdapter {
    logs: LocalLogPolicy,
    children: Mutex<BTreeMap<u32, ManagedChild>>,
    stop_timeout: Duration,
    child_identity: Option<(u32, u32)>,
}

struct ManagedChild {
    child: Child,
    receipt: ProcessReceipt,
}

impl ManagedChild {
    fn matches(&self, receipt: &ProcessReceipt) -> bool {
        self.receipt.launch_id == receipt.launch_id
            && self.receipt.agent_id == receipt.agent_id
            && self.receipt.process_group_id == receipt.process_group_id
            && self.receipt.desired == receipt.desired
            && self.receipt.pid == receipt.pid
            && self.receipt.started_at_unix_ms == receipt.started_at_unix_ms
    }
}

impl LocalProcessAdapter {
    pub fn new(
        logs: LocalLogPolicy,
        stop_timeout: Duration,
        child_identity: Option<(u32, u32)>,
    ) -> Result<Self, SupervisorError> {
        logs.validate()?;
        if stop_timeout.is_zero() {
            return Err(SupervisorError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "stop timeout must be non-zero",
            )));
        }
        fs::create_dir_all(&logs.directory)?;
        Ok(Self {
            logs,
            children: Mutex::new(BTreeMap::new()),
            stop_timeout,
            child_identity,
        })
    }

    /// Reads a bounded log tail and masks common credential-bearing forms.
    pub fn read_log_tail(&self, launch_id: &str, stderr: bool) -> Result<String, SupervisorError> {
        let path = self.log_path(launch_id, stderr)?;
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
            Err(error) => return Err(error.into()),
        };
        let length = file.metadata()?.len();
        let start = length.saturating_sub(self.logs.max_read_bytes as u64);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::with_capacity(self.logs.max_read_bytes);
        file.take(self.logs.max_read_bytes as u64)
            .read_to_end(&mut bytes)?;
        Ok(redact_log(&String::from_utf8_lossy(&bytes)))
    }

    fn prepare_log(&self, launch_id: &str, stderr: bool) -> Result<PathBuf, SupervisorError> {
        let path = self.log_path(launch_id, stderr)?;
        let mut options = OpenOptions::new();
        options.create(true).append(true).read(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&path)?;
        if file.metadata()?.len() >= self.logs.max_file_bytes {
            file.set_len(0)?;
        }
        Ok(path)
    }

    fn log_path(&self, launch_id: &str, stderr: bool) -> Result<PathBuf, SupervisorError> {
        if launch_id.is_empty()
            || launch_id.len() > 160
            || !launch_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(SupervisorError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "launch id is not safe for a log file name",
            )));
        }
        Ok(self.logs.directory.join(format!(
            "{launch_id}.{}",
            if stderr { "stderr.log" } else { "stdout.log" }
        )))
    }

    fn resolve_environment(
        desired: &LaunchSpec,
        secrets: &dyn SecretResolver,
    ) -> Result<BTreeMap<String, String>, SupervisorError> {
        let mut environment = desired.environment.clone();
        let runtime_bin = Path::new(&desired.runtime.executable.path)
            .parent()
            .and_then(Path::to_str)
            .unwrap_or("/usr/bin");
        environment
            .entry("PATH".into())
            .or_insert_with(|| format!("{runtime_bin}:/usr/local/bin:/usr/bin:/bin"));
        environment
            .entry("HOME".into())
            .or_insert_with(|| desired.runtime_path.clone());
        environment
            .entry("CODEX_HOME".into())
            .or_insert_with(|| format!("{}/codex-home", desired.runtime_path));
        environment
            .entry("TMPDIR".into())
            .or_insert_with(|| format!("{}/tmp", desired.runtime_path));
        for (name, reference) in &desired.secret_environment {
            let value = secrets
                .resolve(reference)
                .map_err(|_| SupervisorError::SecretResolution)?;
            if value.contains('\0') {
                return Err(SupervisorError::SecretResolution);
            }
            environment.insert(name.clone(), value);
        }
        environment.extend(desired.harness_runtime_environment()?);
        environment.insert(LAUNCH_MARKER.to_owned(), desired.launch_id.clone());
        Ok(environment)
    }

    fn run_preflight(
        &self,
        desired: &LaunchSpec,
        environment: &BTreeMap<String, String>,
    ) -> Result<(), SupervisorError> {
        let Some(probe) = &desired.runtime.preflight else {
            return Ok(());
        };
        let mut command = Command::new(&probe.command);
        command
            .args(&probe.arguments)
            .current_dir(&desired.working_directory)
            .env_clear()
            .envs(environment.iter().filter(|(name, _)| {
                name.as_str() != crate::launch::HARNESS_PRIVATE_KEY_ENV
                    && name.as_str() != crate::launch::HARNESS_AUTH_TAG_ENV
            }))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            command.process_group(0);
            if let Some((uid, gid)) = self.child_identity {
                command.uid(uid).gid(gid);
            }
        }
        let mut child = command.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(u64::from(probe.timeout_seconds));
        loop {
            if let Some(status) = child.try_wait()? {
                #[cfg(unix)]
                let _ = Self::signal_group(child.id(), "-KILL");
                return if status.success() {
                    Ok(())
                } else {
                    Err(SupervisorError::Preflight(exit_description(status)))
                };
            }
            if Instant::now() >= deadline {
                #[cfg(unix)]
                Self::signal_group(child.id(), "-KILL")?;
                #[cfg(not(unix))]
                child.kill()?;
                let _ = child.wait();
                return Err(SupervisorError::Preflight("timed out".to_owned()));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn receipt_owned(receipt: &ProcessReceipt) -> bool {
        #[cfg(target_os = "linux")]
        {
            // `/proc/<pid>/environ` is intentionally unreadable after the
            // harness drops to another uid under the hardened systemd unit.
            // Put the immutable launch marker in argv[0], which remains
            // readable cross-uid, and bind it to the configured harness path.
            let expected_command = format!(
                "{}#{LAUNCH_MARKER}={}",
                receipt.desired.harness.path, receipt.launch_id
            );
            receipt.command_path.as_deref() == Some(expected_command.as_str())
                && receipt.process_start_ticks.is_some()
                && Self::process_start_ticks(receipt.pid) == receipt.process_start_ticks
                && Self::process_command(receipt.pid).as_deref() == receipt.command_path.as_deref()
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Cross-platform PID existence is insufficient to prove ownership.
            // Re-adoption is intentionally unavailable until a platform-native
            // marker probe is supplied.
            let _ = receipt;
            false
        }
    }

    fn managed_receipt(&self, receipt: &ProcessReceipt) -> Result<bool, SupervisorError> {
        Ok(self
            .children
            .lock()
            .map_err(|_| SupervisorError::LockPoisoned)?
            .get(&receipt.pid)
            .is_some_and(|managed| managed.matches(receipt)))
    }

    fn signal_group(pid: u32, signal: &str) -> Result<(), SupervisorError> {
        #[cfg(unix)]
        {
            let status = Command::new("/bin/kill")
                .args([signal, "--", &format!("-{pid}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !status.success() && Self::process_exists(pid)? {
                return Err(SupervisorError::Io(io::Error::other(
                    "failed to signal process group",
                )));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, signal);
            Err(SupervisorError::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "process-group signaling is not implemented on this platform",
            )))
        }
    }

    fn process_exists(pid: u32) -> Result<bool, SupervisorError> {
        #[cfg(target_os = "linux")]
        {
            Ok(Path::new(&format!("/proc/{pid}")).exists())
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            let status = Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            Ok(status.success())
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Err(SupervisorError::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "process inspection is not implemented on this platform",
            )))
        }
    }

    fn health_ready(receipt: &ProcessReceipt) -> bool {
        let elapsed = unix_millis()
            .unwrap_or_default()
            .saturating_sub(receipt.started_at_unix_ms);
        match &receipt.desired.health {
            HealthPolicy::Process { startup_grace_ms } => elapsed >= *startup_grace_ms,
            HealthPolicy::Tcp {
                host,
                port,
                startup_grace_ms,
                timeout_ms,
                ..
            } => {
                if elapsed < *startup_grace_ms {
                    return false;
                }
                let timeout = Duration::from_millis(*timeout_ms);
                (host.as_str(), *port)
                    .to_socket_addrs()
                    .ok()
                    .into_iter()
                    .flatten()
                    .any(|address: SocketAddr| {
                        TcpStream::connect_timeout(&address, timeout).is_ok()
                    })
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn process_start_ticks(pid: u32) -> Option<u64> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_name = stat.rsplit_once(") ")?.1;
        after_name.split_whitespace().nth(19)?.parse().ok()
    }

    #[cfg(target_os = "linux")]
    fn process_command(pid: u32) -> Option<String> {
        let command = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let first = command.split(|byte| *byte == 0).next()?;
        (!first.is_empty()).then(|| String::from_utf8_lossy(first).into_owned())
    }
}

impl ProcessSupervisor for LocalProcessAdapter {
    fn start(
        &self,
        desired: &LaunchSpec,
        secrets: &dyn SecretResolver,
    ) -> Result<ProcessReceipt, SupervisorError> {
        desired.validate()?;
        let environment = Self::resolve_environment(desired, secrets)?;
        self.run_preflight(desired, &environment)?;

        let stdout_path = self.prepare_log(&desired.launch_id, false)?;
        let stderr_path = self.prepare_log(&desired.launch_id, true)?;
        let mut command = Command::new(&desired.harness.path);
        command
            .args(&desired.harness_arguments)
            .current_dir(&desired.working_directory)
            .env_clear()
            .envs(&environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            command.arg0(format!(
                "{}#{LAUNCH_MARKER}={}",
                desired.harness.path, desired.launch_id
            ));
            command.process_group(0);
            if let Some((uid, gid)) = self.child_identity {
                command.uid(uid).gid(gid);
            }
        }
        let mut child = command.spawn()?;
        let pid = child.id();
        if let Some(stdout) = child.stdout.take() {
            spawn_log_drain(stdout, stdout_path, self.logs.max_file_bytes);
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_drain(stderr, stderr_path, self.logs.max_file_bytes);
        }
        let receipt = ProcessReceipt {
            launch_id: desired.launch_id.clone(),
            agent_id: desired.agent_id,
            process_group_id: desired.process_group_id.clone(),
            desired: desired.identity(),
            pid,
            started_at_unix_ms: unix_millis()?,
            #[cfg(target_os = "linux")]
            process_start_ticks: Self::process_start_ticks(pid),
            #[cfg(not(target_os = "linux"))]
            process_start_ticks: None,
            #[cfg(target_os = "linux")]
            command_path: Self::process_command(pid),
            #[cfg(not(target_os = "linux"))]
            command_path: None,
            observed_state: ObservedProcessState::Starting,
            exit_code: None,
        };
        receipt.validate()?;
        self.children
            .lock()
            .map_err(|_| SupervisorError::LockPoisoned)?
            .insert(
                pid,
                ManagedChild {
                    child,
                    receipt: receipt.clone(),
                },
            );
        Ok(receipt)
    }

    fn inspect(&self, receipt: &ProcessReceipt) -> Result<ProcessReceipt, SupervisorError> {
        receipt.validate()?;
        let mut observed = receipt.clone();
        let mut children = self
            .children
            .lock()
            .map_err(|_| SupervisorError::LockPoisoned)?;
        if let Some(managed) = children.get_mut(&receipt.pid) {
            if !managed.matches(receipt) {
                return Err(SupervisorError::ReceiptMismatch);
            }
            if let Some(status) = managed.child.try_wait()? {
                observed.observe(ObservedProcessState::Exited, status.code())?;
                children.remove(&receipt.pid);
            } else if observed.observed_state == ObservedProcessState::Starting
                && Self::health_ready(&observed)
            {
                #[cfg(target_os = "linux")]
                {
                    observed.process_start_ticks = Self::process_start_ticks(receipt.pid);
                    observed.command_path = Self::process_command(receipt.pid);
                }
                observed.observe(ObservedProcessState::Healthy, None)?;
                managed.receipt = observed.clone();
            }
            return Ok(observed);
        }
        drop(children);
        if Self::receipt_owned(receipt) && Self::process_exists(receipt.pid)? {
            if observed.observed_state == ObservedProcessState::Starting
                && Self::health_ready(&observed)
            {
                observed.observe(ObservedProcessState::Healthy, None)?;
            }
        } else if !observed.observed_state.is_terminal() {
            observed.observe(ObservedProcessState::Lost, None)?;
        }
        Ok(observed)
    }

    fn stop(&self, receipt: &ProcessReceipt) -> Result<ProcessReceipt, SupervisorError> {
        let mut observed = self.inspect(receipt)?;
        if observed.observed_state.is_terminal() {
            return Ok(observed);
        }
        if !self.managed_receipt(receipt)? && !Self::receipt_owned(receipt) {
            return Err(SupervisorError::ReceiptMismatch);
        }
        observed.observe(ObservedProcessState::Stopping, None)?;
        Self::signal_group(receipt.pid, "-TERM")?;
        let deadline = Instant::now() + self.stop_timeout;
        while Self::process_exists(receipt.pid)? && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        if Self::process_exists(receipt.pid)? {
            Self::signal_group(receipt.pid, "-KILL")?;
        }
        if let Some(mut managed) = self
            .children
            .lock()
            .map_err(|_| SupervisorError::LockPoisoned)?
            .remove(&receipt.pid)
        {
            let status = managed.child.wait()?;
            observed.observe(ObservedProcessState::Exited, status.code())?;
        } else {
            observed.observe(ObservedProcessState::Lost, None)?;
        }
        Ok(observed)
    }
}

fn unix_millis() -> Result<u64, SupervisorError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SupervisorError::Clock)?;
    u64::try_from(duration.as_millis()).map_err(|_| SupervisorError::Clock)
}

fn exit_description(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_owned(),
        |code| format!("exit {code}"),
    )
}

fn redact_log(input: &str) -> String {
    // Child runtimes receive credentials. Their arbitrary output cannot be
    // reliably classified by key-shaped heuristics, especially across read
    // boundaries, so persisted diagnostics record only that output occurred.
    if input.is_empty() {
        String::new()
    } else {
        "[REDACTED CHILD OUTPUT]\n".to_owned()
    }
}

fn spawn_log_drain<R>(mut reader: R, path: PathBuf, max_bytes: u64)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            let mut options = OpenOptions::new();
            options.create(true).append(true).read(true);
            #[cfg(unix)]
            options.mode(0o600);
            let Ok(mut file) = options.open(&path) else {
                return;
            };
            let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
                return;
            };
            let sanitized = redact_log(&String::from_utf8_lossy(&buffer[..read]));
            let sanitized = sanitized.as_bytes();
            if length.saturating_add(sanitized.len() as u64) > max_bytes && file.set_len(0).is_err()
            {
                return;
            }
            let keep =
                usize::try_from(max_bytes.min(sanitized.len() as u64)).unwrap_or(sanitized.len());
            if file
                .write_all(&sanitized[sanitized.len() - keep..])
                .is_err()
            {
                return;
            }
        }
    });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        launch::{
            ExecutableIdentity, HealthPolicy, LocalProcessRole, ResolvedRuntime, RestartMode,
            RestartPolicy,
        },
        AgentId, RuntimeId,
    };

    struct NoSecrets;

    impl SecretResolver for NoSecrets {
        fn resolve(&self, _reference: &SecretRef) -> Result<String, SupervisorError> {
            Err(SupervisorError::SecretResolution)
        }
    }

    struct TestSecrets;

    impl SecretResolver for TestSecrets {
        fn resolve(&self, reference: &SecretRef) -> Result<String, SupervisorError> {
            match reference.key.as_str() {
                "agent-key" => Ok("agent-secret-value".into()),
                "authorization" => Ok("signed-authorization-value".into()),
                _ => Err(SupervisorError::SecretResolution),
            }
        }
    }

    fn launch(directory: &Path, script: &str) -> LaunchSpec {
        LaunchSpec {
            launch_id: "safe-test-launch".into(),
            agent_id: AgentId::new(),
            role: LocalProcessRole::AcpBridge,
            harness: ExecutableIdentity {
                path: "/bin/sh".into(),
                package_id: "system:sh".into(),
                version: "1".into(),
                sha256: None,
            },
            harness_arguments: vec!["-c".into(), script.into()],
            runtime: ResolvedRuntime {
                runtime_id: RuntimeId::parse("test-runtime").unwrap(),
                executable: ExecutableIdentity {
                    path: "/bin/true".into(),
                    package_id: "system:true".into(),
                    version: "1".into(),
                    sha256: None,
                },
                arguments: Vec::new(),
                preflight: None,
            },
            environment: BTreeMap::new(),
            secret_environment: BTreeMap::new(),
            working_directory: directory.display().to_string(),
            workspace_path: directory.display().to_string(),
            runtime_path: directory.display().to_string(),
            process_group_id: "safe-test-group".into(),
            restart: RestartPolicy {
                mode: RestartMode::OnFailure,
                max_attempts: 2,
                initial_backoff_ms: 1,
                max_backoff_ms: 2,
                stable_after_ms: 1,
            },
            health: HealthPolicy::Process {
                startup_grace_ms: 1,
            },
        }
    }

    fn adapter(directory: &Path, max_bytes: u64) -> LocalProcessAdapter {
        LocalProcessAdapter::new(
            LocalLogPolicy {
                directory: directory.join("logs"),
                max_file_bytes: max_bytes,
                max_read_bytes: usize::try_from(max_bytes).unwrap(),
            },
            Duration::from_secs(2),
            None,
        )
        .unwrap()
    }

    fn wait_for_exit(adapter: &LocalProcessAdapter, receipt: &ProcessReceipt) -> ProcessReceipt {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let observed = adapter.inspect(receipt).unwrap();
            if observed.observed_state.is_terminal() {
                return observed;
            }
            assert!(Instant::now() < deadline, "test process did not exit");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn starts_adopts_and_stops_an_owned_process_group() {
        let directory = tempfile::tempdir().unwrap();
        let adapter = adapter(directory.path(), 4096);
        let desired = launch(directory.path(), "sleep 5");
        let receipt = adapter.start(&desired, &NoSecrets).unwrap();
        thread::sleep(Duration::from_millis(2));
        let observed = adapter.inspect(&receipt).unwrap();
        assert_eq!(observed.observed_state, ObservedProcessState::Healthy);
        assert!(desired.can_adopt(&observed));
        let stopped = adapter.stop(&observed).unwrap();
        assert!(stopped.observed_state.is_terminal());
    }

    #[test]
    fn harness_is_bare_and_runtime_selection_uses_verified_environment() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("invocation.txt");
        let script = format!(
            "printf '%s|%s|%s' \"$0\" \"$BUZZ_ACP_AGENT_COMMAND\" \"$BUZZ_ACP_AGENT_ARGS\" > {}",
            output.display()
        );
        let adapter = adapter(directory.path(), 4096);
        let mut desired = launch(directory.path(), &script);
        desired.runtime.arguments = vec!["acp".into()];
        let receipt = adapter.start(&desired, &NoSecrets).unwrap();
        let _ = wait_for_exit(&adapter, &receipt);
        let invocation = fs::read_to_string(output).unwrap();
        let fields: Vec<_> = invocation.split('|').collect();
        assert_ne!(fields[0], desired.runtime.executable.path);
        assert_eq!(fields[1], desired.runtime.executable.path);
        assert_eq!(fields[2], "acp");
    }

    #[test]
    fn authoritative_harness_environment_is_exact_and_receipts_do_not_serialize_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let mut desired = launch(directory.path(), "true");
        desired.runtime.arguments = vec!["acp".into()];
        desired.environment.insert(
            crate::launch::HARNESS_RELAY_URL_ENV.into(),
            "wss://relay.example.test/".into(),
        );
        desired.secret_environment.insert(
            crate::launch::HARNESS_PRIVATE_KEY_ENV.into(),
            SecretRef {
                key: "agent-key".into(),
                version: None,
            },
        );
        desired.secret_environment.insert(
            crate::launch::HARNESS_AUTH_TAG_ENV.into(),
            SecretRef {
                key: "authorization".into(),
                version: None,
            },
        );

        let environment = LocalProcessAdapter::resolve_environment(&desired, &TestSecrets).unwrap();
        assert_eq!(
            environment[crate::launch::HARNESS_PRIVATE_KEY_ENV],
            "agent-secret-value"
        );
        assert_eq!(
            environment[crate::launch::HARNESS_AUTH_TAG_ENV],
            "signed-authorization-value"
        );
        assert_eq!(
            environment[crate::launch::HARNESS_RELAY_URL_ENV],
            "wss://relay.example.test/"
        );
        assert_eq!(
            environment[crate::launch::HARNESS_AGENT_COMMAND_ENV],
            "/bin/true"
        );
        assert_eq!(environment[crate::launch::HARNESS_AGENT_ARGS_ENV], "acp");

        let serialized = serde_json::to_string(&desired).unwrap();
        assert!(!serialized.contains("agent-secret-value"));
        assert!(!serialized.contains("signed-authorization-value"));
    }

    #[test]
    fn preflight_does_not_receive_agent_identity_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("preflight-environment.txt");
        let probe_script = directory.path().join("probe.sh");
        fs::write(
            &probe_script,
            format!(
                "printf '%s|%s|%s' \"${{BUZZ_PRIVATE_KEY-unset}}\" \"${{BUZZ_AUTH_TAG-unset}}\" \"${{RUNTIME_TOKEN-unset}}\" > {}\n",
                output.display()
            ),
        )
        .unwrap();
        let adapter = adapter(directory.path(), 4096);
        let mut desired = launch(directory.path(), "true");
        desired.runtime.preflight = Some(crate::runtime::PreflightProbe {
            command: "/bin/sh".into(),
            arguments: vec![probe_script.display().to_string()],
            timeout_seconds: 2,
        });
        desired.secret_environment.insert(
            crate::launch::HARNESS_PRIVATE_KEY_ENV.into(),
            SecretRef {
                key: "agent-key".into(),
                version: None,
            },
        );
        desired.secret_environment.insert(
            crate::launch::HARNESS_AUTH_TAG_ENV.into(),
            SecretRef {
                key: "authorization".into(),
                version: None,
            },
        );
        desired
            .environment
            .insert("RUNTIME_TOKEN".into(), "runtime-visible".into());

        let receipt = adapter.start(&desired, &TestSecrets).unwrap();
        let _ = wait_for_exit(&adapter, &receipt);
        assert_eq!(
            fs::read_to_string(output).unwrap(),
            "unset|unset|runtime-visible"
        );
    }

    #[test]
    fn marker_mismatch_is_never_adopted() {
        let directory = tempfile::tempdir().unwrap();
        let adapter = adapter(directory.path(), 4096);
        let desired = launch(directory.path(), "sleep 5");
        let receipt = adapter.start(&desired, &NoSecrets).unwrap();
        let mut forged = receipt.clone();
        forged.launch_id = "different-launch".into();
        assert!(matches!(
            adapter.inspect(&forged),
            Err(SupervisorError::ReceiptMismatch)
        ));
        adapter.stop(&receipt).unwrap();
    }

    #[test]
    fn reports_process_exit_and_bounds_and_redacts_logs() {
        let directory = tempfile::tempdir().unwrap();
        let adapter = adapter(directory.path(), 128);
        let desired = launch(
            directory.path(),
            "i=0; while [ $i -lt 100 ]; do echo ordinary-output; i=$((i+1)); done; echo token=credential >&2",
        );
        let receipt = adapter.start(&desired, &NoSecrets).unwrap();
        let exited = wait_for_exit(&adapter, &receipt);
        assert_eq!(exited.observed_state, ObservedProcessState::Exited);
        thread::sleep(Duration::from_millis(50));
        let stdout = adapter.read_log_tail(&desired.launch_id, false).unwrap();
        let stderr = adapter.read_log_tail(&desired.launch_id, true).unwrap();
        assert!(stdout.len() <= 128);
        assert!(stderr.len() <= 128);
        assert!(stderr.contains("[REDACTED CHILD OUTPUT]"));
        assert!(!stderr.contains("credential"));
    }
}
