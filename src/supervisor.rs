//! Safe direct-process supervision for the built-in Local backend.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};

use crate::{
    launch::{AppServerLogsStatus, HealthPolicy, ParentWaitOutcome, SecretRef},
    LaunchSpec, ObservedProcessState, ProcessReceipt, ValidationError,
};

#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{kill, killpg, Signal},
    unistd::Pid,
};

const LAUNCH_MARKER: &str = "BUZZ_SERVER_LAUNCH_ID";
const POLL_INTERVAL: Duration = Duration::from_millis(20);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

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
    child_home: Option<PathBuf>,
    agent_identities: Mutex<BTreeMap<crate::AgentId, LocalProcessIdentity>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalProcessIdentity {
    pub uid: u32,
    pub gid: u32,
    pub supplementary_gids: Vec<u32>,
    pub home: PathBuf,
}

struct ManagedChild {
    child: Child,
    receipt: ProcessReceipt,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct LogProvenance {
    stream: String,
    launch_id: String,
    pid: Option<u32>,
    generation: String,
    started_at_unix_ms: u64,
    ended_at_unix_ms: Option<u64>,
    captured_bytes: u64,
    stored_bytes: u64,
    truncated_bytes: u64,
    dropped_bytes: u64,
    redaction: String,
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
    #[cfg(unix)]
    fn command_for_identity(executable: &str, identity: Option<&LocalProcessIdentity>) -> Command {
        let Some(identity) = identity else {
            return Command::new(executable);
        };
        let groups = identity
            .supplementary_gids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut command = Command::new("/usr/bin/setpriv");
        command.args([
            "--reuid",
            &identity.uid.to_string(),
            "--regid",
            &identity.gid.to_string(),
            "--groups",
            &groups,
            "--",
            executable,
        ]);
        command
    }

    pub fn new(
        logs: LocalLogPolicy,
        stop_timeout: Duration,
        child_identity: Option<(u32, u32)>,
        child_home: Option<PathBuf>,
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
            child_home,
            agent_identities: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn set_agent_identity(
        &self,
        agent_id: crate::AgentId,
        identity: LocalProcessIdentity,
    ) -> Result<(), SupervisorError> {
        self.agent_identities
            .lock()
            .map_err(|_| SupervisorError::LockPoisoned)?
            .insert(agent_id, identity);
        Ok(())
    }

    fn identity_for(
        &self,
        agent_id: crate::AgentId,
    ) -> Result<Option<LocalProcessIdentity>, SupervisorError> {
        if let Some(identity) = self
            .agent_identities
            .lock()
            .map_err(|_| SupervisorError::LockPoisoned)?
            .get(&agent_id)
            .cloned()
        {
            return Ok(Some(identity));
        }
        Ok(self.child_identity.map(|(uid, gid)| LocalProcessIdentity {
            uid,
            gid,
            supplementary_gids: vec![gid],
            home: self.child_home.clone().unwrap_or_default(),
        }))
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

    fn prepare_log(
        &self,
        launch_id: &str,
        stderr: bool,
        generation: &str,
    ) -> Result<PathBuf, SupervisorError> {
        let path = self.log_path(launch_id, stderr)?;
        if path.exists() {
            let stem = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("log");
            let rotated = self.logs.directory.join(format!("{stem}.{generation}"));
            fs::rename(&path, rotated)?;
            let metadata = provenance_path(&path);
            if metadata.exists() {
                let rotated_metadata = self
                    .logs
                    .directory
                    .join(format!("{stem}.{generation}.meta.json"));
                fs::rename(metadata, rotated_metadata)?;
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).write(true).truncate(true).read(true);
        #[cfg(unix)]
        options.mode(0o600);
        let _file = options.open(&path)?;
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
        &self,
        desired: &LaunchSpec,
        secrets: &dyn SecretResolver,
        identity: Option<&LocalProcessIdentity>,
    ) -> Result<BTreeMap<String, String>, SupervisorError> {
        let mut environment = desired.environment.clone();
        let runtime_bin = Path::new(&desired.runtime.executable.path)
            .parent()
            .and_then(Path::to_str)
            .unwrap_or("/usr/bin");
        environment
            .entry("PATH".into())
            .or_insert_with(|| format!("{runtime_bin}:/usr/local/bin:/usr/bin:/bin"));
        environment.entry("HOME".into()).or_insert_with(|| {
            identity
                .map(|identity| &identity.home)
                .or(self.child_home.as_ref())
                .and_then(|path| path.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| desired.runtime_path.clone())
        });
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
        identity: Option<&LocalProcessIdentity>,
    ) -> Result<(), SupervisorError> {
        let Some(probe) = &desired.runtime.preflight else {
            return Ok(());
        };
        #[cfg(unix)]
        let mut command = Self::command_for_identity(&probe.command, identity);
        #[cfg(not(unix))]
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
            let expected_command = format!(
                "{}#{LAUNCH_MARKER}={}",
                receipt.desired.harness.path, receipt.launch_id
            );
            let command_matches = receipt.command_path.as_deref().is_some_and(|command| {
                command == expected_command || command == receipt.desired.harness.path
            });
            command_matches
                && receipt.process_start_ticks.is_some()
                && Self::process_start_ticks(receipt.pid) == receipt.process_start_ticks
                && Self::process_command(receipt.pid).as_deref() == receipt.command_path.as_deref()
                && Self::process_has_launch_marker(receipt.pid, &receipt.launch_id)
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
            let signal = match signal {
                "-TERM" => Signal::SIGTERM,
                "-KILL" => Signal::SIGKILL,
                _ => {
                    return Err(SupervisorError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unsupported process-group signal",
                    )))
                }
            };
            let pid = i32::try_from(pid).map_err(|_| {
                SupervisorError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "process id exceeds platform range",
                ))
            })?;
            match killpg(Pid::from_raw(pid), signal) {
                Ok(()) | Err(Errno::ESRCH) => Ok(()),
                Err(error) => Err(SupervisorError::Io(io::Error::from_raw_os_error(
                    error as i32,
                ))),
            }
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
        #[cfg(unix)]
        {
            let pid = i32::try_from(pid).map_err(|_| {
                SupervisorError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "process id exceeds platform range",
                ))
            })?;
            match kill(Pid::from_raw(pid), None) {
                Ok(()) | Err(Errno::EPERM) => Ok(true),
                Err(Errno::ESRCH) => Ok(false),
                Err(error) => Err(SupervisorError::Io(io::Error::from_raw_os_error(
                    error as i32,
                ))),
            }
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

    #[cfg(target_os = "linux")]
    fn process_has_launch_marker(pid: u32, launch_id: &str) -> bool {
        let environment = fs::read(format!("/proc/{pid}/environ")).ok();
        environment.is_some_and(|environment| {
            let expected = format!("{LAUNCH_MARKER}={launch_id}");
            environment
                .split(|byte| *byte == 0)
                .any(|entry| entry == expected.as_bytes())
        })
    }
}

impl ProcessSupervisor for LocalProcessAdapter {
    fn start(
        &self,
        desired: &LaunchSpec,
        secrets: &dyn SecretResolver,
    ) -> Result<ProcessReceipt, SupervisorError> {
        desired.validate()?;
        let identity = self.identity_for(desired.agent_id)?;
        let generation = format!(
            "{}-{}",
            unix_millis()?,
            NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
        );
        let mut environment = self.resolve_environment(desired, secrets, identity.as_ref())?;
        let app_server_logs = configure_app_server_logs(
            &mut environment,
            &self.logs.directory,
            &desired.launch_id,
            &generation,
        );
        self.run_preflight(desired, &environment, identity.as_ref())?;

        let stdout_path = self.prepare_log(&desired.launch_id, false, &generation)?;
        let stderr_path = self.prepare_log(&desired.launch_id, true, &generation)?;
        #[cfg(unix)]
        let mut command = Self::command_for_identity(&desired.harness.path, identity.as_ref());
        #[cfg(not(unix))]
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
            if identity.is_none() {
                command.arg0(format!(
                    "{}#{LAUNCH_MARKER}={}",
                    desired.harness.path, desired.launch_id
                ));
            }
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let pid = child.id();
        if let Some(stdout) = child.stdout.take() {
            write_log_provenance(&stdout_path, "stdout", desired, &generation, pid);
            spawn_log_drain(stdout, stdout_path, self.logs.max_file_bytes);
        }
        if let Some(stderr) = child.stderr.take() {
            write_log_provenance(&stderr_path, "stderr", desired, &generation, pid);
            spawn_log_drain(stderr, stderr_path, self.logs.max_file_bytes);
        }
        let receipt = ProcessReceipt {
            launch_id: desired.launch_id.clone(),
            generation: Some(generation.clone()),
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
            command_path: Some(if identity.is_some() {
                desired.harness.path.clone()
            } else {
                format!(
                    "{}#{LAUNCH_MARKER}={}",
                    desired.harness.path, desired.launch_id
                )
            }),
            #[cfg(not(target_os = "linux"))]
            command_path: None,
            observed_state: ObservedProcessState::Starting,
            exit_code: None,
            wait_outcome: None,
            ended_at_unix_ms: None,
            duration_ms: None,
            failure: None,
            app_server_logs,
            lifecycle: None,
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
                observe_wait(&mut observed, status);
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
            observe_wait(&mut observed, status);
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

fn configure_app_server_logs(
    environment: &mut BTreeMap<String, String>,
    root: &Path,
    launch_id: &str,
    generation: &str,
) -> AppServerLogsStatus {
    if let Some(path) = environment.get("APP_SERVER_LOGS") {
        return if fs::create_dir_all(path).is_ok() && fs::metadata(path).is_ok_and(|m| m.is_dir()) {
            AppServerLogsStatus::Configured
        } else {
            AppServerLogsStatus::Unwritable
        };
    }
    let path = root.join(format!("{launch_id}.{generation}.app-server-logs"));
    if fs::create_dir_all(&path).is_ok()
        && fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir())
    {
        environment.insert("APP_SERVER_LOGS".into(), path.display().to_string());
        AppServerLogsStatus::Configured
    } else {
        AppServerLogsStatus::Unwritable
    }
}

fn provenance_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.meta.json",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("log")
    ))
}

fn write_log_provenance(
    path: &Path,
    stream: &str,
    desired: &LaunchSpec,
    generation: &str,
    pid: u32,
) {
    let provenance = LogProvenance {
        stream: stream.to_owned(),
        launch_id: desired.launch_id.clone(),
        pid: Some(pid),
        generation: generation.to_owned(),
        started_at_unix_ms: unix_millis().unwrap_or_default(),
        ended_at_unix_ms: None,
        captured_bytes: 0,
        stored_bytes: 0,
        truncated_bytes: 0,
        dropped_bytes: 0,
        redaction: "capture".into(),
    };
    if let Ok(payload) = serde_json::to_vec(&provenance) {
        let _ = fs::write(provenance_path(path), payload);
    }
}

fn exit_description(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_owned(),
        |code| format!("exit {code}"),
    )
}

fn observe_wait(receipt: &mut ProcessReceipt, status: ExitStatus) {
    receipt.ended_at_unix_ms = unix_millis().ok();
    receipt.duration_ms = receipt
        .ended_at_unix_ms
        .map(|ended| ended.saturating_sub(receipt.started_at_unix_ms));
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    receipt.wait_outcome = Some({
        #[cfg(unix)]
        if let Some(signal) = status.signal() {
            ParentWaitOutcome::Signaled { signal }
        } else {
            ParentWaitOutcome::Exited {
                code: status.code().unwrap_or(-1),
            }
        }
        #[cfg(not(unix))]
        {
            ParentWaitOutcome::Exited {
                code: status.code().unwrap_or(-1),
            }
        }
    });
}

fn redact_log(input: &str) -> String {
    // Child runtimes receive credentials. Their arbitrary output cannot be
    // reliably classified by key-shaped heuristics, especially across read
    // boundaries, so persisted diagnostics record only that output occurred.
    let mut redacted = String::new();
    for line in input.lines() {
        let diagnostic = sanitized_child_diagnostic(line);
        redacted.push_str(diagnostic.as_deref().unwrap_or("[REDACTED CHILD OUTPUT]"));
        redacted.push('\n');
    }
    redacted
}

fn sanitized_child_diagnostic(line: &str) -> Option<String> {
    let line = line.trim();
    if matches!(
        line,
        "[CHILD INITIALIZATION ERROR]" | "[CHILD ERROR class=sqlite_state_initialization_failed]"
    ) {
        return Some(line.to_owned());
    }
    if let Some(diagnostic) = sanitized_structured_task_diagnostic(line) {
        return Some(diagnostic);
    }
    for prefix in ["[CHILD ADAPTER ERROR code=", "[CHILD PROCESS EXIT code="] {
        if line
            .strip_prefix(prefix)
            .and_then(|value| value.strip_suffix(']'))
            .is_some_and(valid_error_code)
        {
            return Some(line.to_owned());
        }
    }

    if line.starts_with("agent initialize failed:") {
        let mut diagnostic = "[CHILD INITIALIZATION ERROR]".to_owned();
        if let Some(code) = numeric_value_after(line, "Agent reported error (code ") {
            diagnostic.push_str(&format!("\n[CHILD ADAPTER ERROR code={code}]"));
        }
        if let Some(code) = numeric_value_after(line, "process has exited with code ") {
            diagnostic.push_str(&format!("\n[CHILD PROCESS EXIT code={code}]"));
        }
        return Some(diagnostic);
    }
    if let Some(diagnostic) = classified_task_diagnostic(line) {
        return Some(diagnostic);
    }
    if let Some(code) = numeric_value_after(line, "Agent reported error (code ") {
        let mut diagnostic = format!("[CHILD ADAPTER ERROR code={code}]");
        if let Some(exit_code) = numeric_value_after(line, "process has exited with code ") {
            diagnostic.push_str(&format!("\n[CHILD PROCESS EXIT code={exit_code}]"));
        }
        return Some(diagnostic);
    }
    if let Some(code) = numeric_value_after(line, "process has exited with code ") {
        return Some(format!("[CHILD PROCESS EXIT code={code}]"));
    }
    if line.starts_with("Error: failed to initialize sqlite state runtime") {
        return Some("[CHILD ERROR class=sqlite_state_initialization_failed]".to_owned());
    }
    None
}

fn sanitized_structured_task_diagnostic(line: &str) -> Option<String> {
    let fields = line
        .strip_prefix("[CHILD TASK ERROR ")?
        .strip_suffix(']')?
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields.is_empty() {
        return None;
    }

    let mut class = None;
    let mut code = None;
    let mut exit_code = None;
    let mut action = None;
    let mut phase = None;
    let mut rule = None;
    let mut retry_at = None;
    let mut retry_after_seconds = None;
    for field in fields {
        let (key, value) = field.split_once('=')?;
        match key {
            "class" if class.is_none() && valid_task_class(value) => class = Some(value),
            "code" if code.is_none() && valid_error_code(value) => code = Some(value),
            "exit_code" if exit_code.is_none() && valid_error_code(value) => {
                exit_code = Some(value)
            }
            "action" if action.is_none() && valid_task_action(value) => action = Some(value),
            "phase" if phase.is_none() && valid_task_phase(value) => phase = Some(value),
            "rule" if rule.is_none() && valid_task_rule(value) => rule = Some(value),
            "retry_at" if retry_at.is_none() && valid_retry_at(value) => retry_at = Some(value),
            "retry_after_seconds"
                if retry_after_seconds.is_none() && valid_retry_after_seconds(value) =>
            {
                retry_after_seconds = Some(value)
            }
            _ => return None,
        }
    }
    let (Some(class), Some(action), Some(phase), Some(rule)) = (class, action, phase, rule) else {
        return None;
    };
    let mut diagnostic = format!("[CHILD TASK ERROR class={class}");
    if let Some(code) = code {
        diagnostic.push_str(&format!(" code={code}"));
    }
    if let Some(exit_code) = exit_code {
        diagnostic.push_str(&format!(" exit_code={exit_code}"));
    }
    diagnostic.push_str(&format!(" action={action} phase={phase} rule={rule}"));
    if let Some(retry_at) = retry_at {
        diagnostic.push_str(&format!(" retry_at={retry_at}"));
    }
    if let Some(seconds) = retry_after_seconds {
        diagnostic.push_str(&format!(" retry_after_seconds={seconds}"));
    }
    diagnostic.push(']');
    Some(diagnostic)
}

fn classified_task_diagnostic(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let adapter_line = line.contains("Agent reported error (code ")
        || line.starts_with("agent task failed:")
        || line.starts_with("agent turn failed:")
        || line.starts_with("agent request failed:");
    if !adapter_line {
        return None;
    }

    let (class, action, phase, rule) = if contains_any(
        &lower,
        &[
            "unauthorized",
            "authentication failed",
            "refresh token",
            "reauthenticate",
        ],
    ) {
        (
            "unauthorized",
            "reauthenticate",
            "task",
            "provider_unauthorized",
        )
    } else if contains_any(
        &lower,
        &[
            "usage limit",
            "usage-limit",
            "rate limit",
            "quota",
            "too many requests",
        ],
    ) {
        ("usage_limit", "retry", "task", "provider_usage_limit")
    } else if contains_any(
        &lower,
        &[
            "no codex session",
            "session has not been initialized",
            "failed to create session",
            "session creation failed",
            "no active session",
            "pre-session",
            "presession",
            "before turn",
            "pre-turn",
            "preturn",
        ],
    ) {
        ("no_session", "retry", "pre_session", "provider_no_session")
    } else {
        return None;
    };

    let code = numeric_value_after(line, "Agent reported error (code ");
    let mut diagnostic = format!("[CHILD TASK ERROR class={class}");
    if let Some(code) = code {
        diagnostic.push_str(&format!(" code={code}"));
    }
    if let Some(exit_code) = numeric_value_after(line, "process has exited with code ") {
        diagnostic.push_str(&format!(" exit_code={exit_code}"));
    }
    diagnostic.push_str(&format!(" action={action} phase={phase} rule={rule}"));
    if let Some(retry_at) = sanitized_retry_at(line) {
        diagnostic.push_str(&format!(" retry_at={retry_at}"));
    }
    if let Some(seconds) = sanitized_retry_after_seconds(line) {
        diagnostic.push_str(&format!(" retry_after_seconds={seconds}"));
    }
    diagnostic.push(']');
    Some(diagnostic)
}

fn contains_any(input: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| input.contains(needle))
}

fn valid_task_class(value: &str) -> bool {
    matches!(value, "unauthorized" | "usage_limit" | "no_session")
}

fn valid_task_action(value: &str) -> bool {
    matches!(value, "reauthenticate" | "retry" | "reauthorize")
}

fn valid_task_phase(value: &str) -> bool {
    matches!(value, "task" | "pre_session" | "pre_turn")
}

fn valid_task_rule(value: &str) -> bool {
    matches!(
        value,
        "provider_unauthorized" | "provider_usage_limit" | "provider_no_session"
    )
}

fn valid_retry_at(value: &str) -> bool {
    value.len() == 20
        && value.ends_with('Z')
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7) && byte == b'-'
                || matches!(index, 10) && byte == b'T'
                || matches!(index, 13 | 16) && byte == b':'
                || matches!(index, 19) && byte == b'Z'
                || !matches!(index, 4 | 7 | 10 | 13 | 16 | 19) && byte.is_ascii_digit()
        })
}

fn valid_retry_after_seconds(value: &str) -> bool {
    value.parse::<u32>().is_ok_and(|seconds| seconds <= 604_800)
}

fn sanitized_retry_at(line: &str) -> Option<&str> {
    for marker in ["retry_at=", "retry at ", "retry after "] {
        let Some(value) = line.split_once(marker).map(|(_, value)| value) else {
            continue;
        };
        let Some(candidate) = value.get(..20) else {
            continue;
        };
        if valid_retry_at(candidate) {
            return Some(candidate);
        }
    }
    None
}

fn sanitized_retry_after_seconds(line: &str) -> Option<&str> {
    for marker in ["retry_after_seconds=", "retry-after-seconds="] {
        let Some(value) = line.split_once(marker).map(|(_, value)| value) else {
            continue;
        };
        let end = value
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(value.len());
        let candidate = &value[..end];
        if valid_retry_after_seconds(candidate) {
            return Some(candidate);
        }
    }
    None
}

fn numeric_value_after<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let value = line.split_once(marker)?.1;
    let end = value
        .find(|character: char| !character.is_ascii_digit() && character != '-')
        .unwrap_or(value.len());
    let value = &value[..end];
    valid_error_code(value).then_some(value)
}

fn valid_error_code(value: &str) -> bool {
    !value.is_empty() && value.len() <= 11 && value.parse::<i32>().is_ok()
}

fn spawn_log_drain<R>(reader: R, path: PathBuf, max_bytes: u64)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        const MAX_DIAGNOSTIC_LINE_BYTES: usize = 4096;
        let mut reader = reader;
        let mut buffer = [0_u8; 8192];
        let mut pending = Vec::with_capacity(MAX_DIAGNOSTIC_LINE_BYTES);
        let mut discarding_line = false;
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => {
                    if !discarding_line && !pending.is_empty() {
                        let sanitized = redact_log(&String::from_utf8_lossy(&pending));
                        let _ = append_sanitized_log(&path, max_bytes, &sanitized);
                    }
                    return;
                }
                Err(_) => return,
                Ok(read) => read,
            };
            for byte in &buffer[..read] {
                if discarding_line {
                    if *byte == b'\n' {
                        discarding_line = false;
                    }
                    continue;
                }
                if *byte == b'\n' {
                    let sanitized = redact_log(&String::from_utf8_lossy(&pending));
                    if append_sanitized_log(&path, max_bytes, &sanitized).is_err() {
                        return;
                    }
                    pending.clear();
                } else if pending.len() < MAX_DIAGNOSTIC_LINE_BYTES {
                    pending.push(*byte);
                } else {
                    if append_sanitized_log(&path, max_bytes, "[REDACTED CHILD OUTPUT]\n").is_err()
                    {
                        return;
                    }
                    pending.clear();
                    discarding_line = true;
                }
            }
        }
    });
}

fn append_sanitized_log(path: &Path, max_bytes: u64, sanitized: &str) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    let sanitized = sanitized.as_bytes();
    let existing_len = file.metadata()?.len();
    let mut rotated_provenance = None;
    if existing_len.saturating_add(sanitized.len() as u64) > max_bytes {
        drop(file);
        rotated_provenance = fs::read(provenance_path(path))
            .ok()
            .and_then(|payload| serde_json::from_slice::<LogProvenance>(&payload).ok());
        let mut segment = 0_u64;
        let rotated = loop {
            let candidate = path.with_file_name(format!(
                "{}.segment-{segment}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("log")
            ));
            if !candidate.exists() {
                break candidate;
            }
            segment += 1;
        };
        fs::rename(path, rotated)?;
        let metadata = provenance_path(path);
        if metadata.exists() {
            let rotated_metadata = metadata.with_file_name(format!(
                "{}.segment-{segment}.meta.json",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("log")
            ));
            let _ = fs::rename(metadata, rotated_metadata);
        }
        if let Some(mut provenance) = rotated_provenance {
            provenance.started_at_unix_ms = unix_millis().unwrap_or_default();
            provenance.ended_at_unix_ms = None;
            provenance.captured_bytes = 0;
            provenance.stored_bytes = 0;
            provenance.truncated_bytes = 0;
            provenance.dropped_bytes = 0;
            if let Ok(payload) = serde_json::to_vec(&provenance) {
                fs::write(provenance_path(path), payload)?;
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true).read(true);
        #[cfg(unix)]
        options.mode(0o600);
        file = options.open(path)?;
    }
    let keep = usize::try_from(max_bytes.min(sanitized.len() as u64)).unwrap_or(sanitized.len());
    file.write_all(&sanitized[sanitized.len() - keep..])?;
    update_log_provenance(
        path,
        sanitized.len() as u64,
        keep as u64,
        (sanitized.len() - keep) as u64,
        0,
    )
}

fn update_log_provenance(
    path: &Path,
    captured_bytes: u64,
    stored_bytes: u64,
    truncated_bytes: u64,
    dropped_bytes: u64,
) -> io::Result<()> {
    let metadata = provenance_path(path);
    let Ok(payload) = fs::read(&metadata) else {
        return Ok(());
    };
    let Ok(mut provenance) = serde_json::from_slice::<LogProvenance>(&payload) else {
        return Ok(());
    };
    provenance.captured_bytes = provenance.captured_bytes.saturating_add(captured_bytes);
    provenance.stored_bytes = provenance.stored_bytes.saturating_add(stored_bytes);
    provenance.truncated_bytes = provenance.truncated_bytes.saturating_add(truncated_bytes);
    provenance.dropped_bytes = provenance.dropped_bytes.saturating_add(dropped_bytes);
    fs::write(
        metadata,
        serde_json::to_vec(&provenance).map_err(io::Error::other)?,
    )
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
    fn configured_child_home_is_default_and_codex_home_is_only_explicit() {
        let directory = tempfile::tempdir().unwrap();
        let desired = launch(directory.path(), "true");
        let home = directory.path().join("runtime-user-home");
        let adapter = LocalProcessAdapter::new(
            LocalLogPolicy {
                directory: directory.path().join("logs-home"),
                max_file_bytes: 1024,
                max_read_bytes: 1024,
            },
            Duration::from_secs(2),
            None,
            Some(home.clone()),
        )
        .unwrap();

        let environment = adapter
            .resolve_environment(&desired, &TestSecrets, None)
            .unwrap();
        assert_eq!(environment.get("HOME").map(String::as_str), home.to_str());
        assert!(!environment.contains_key("CODEX_HOME"));

        let mut explicit = desired;
        explicit
            .environment
            .insert("CODEX_HOME".into(), "/explicit/codex".into());
        let environment = adapter
            .resolve_environment(&explicit, &TestSecrets, None)
            .unwrap();
        assert_eq!(
            environment.get("CODEX_HOME").map(String::as_str),
            Some("/explicit/codex")
        );
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

        let environment = adapter(directory.path(), 1024)
            .resolve_environment(&desired, &TestSecrets, None)
            .unwrap();
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
    fn preserves_only_allowlisted_child_initialization_diagnostics() {
        assert_eq!(
            redact_log(
                "agent initialize failed: Agent reported error (code 1001): Codex process has exited with code 1: token=secret\n"
            ),
            concat!(
                "[CHILD INITIALIZATION ERROR]\n",
                "[CHILD ADAPTER ERROR code=1001]\n",
                "[CHILD PROCESS EXIT code=1]\n",
            )
        );
        let input = concat!(
            "agent initialize failed: Agent reported error (code 1001): secret-value\n",
            "Agent reported error (code 1001): Codex process has exited with code 1: token=secret\n",
            "Error: failed to initialize sqlite state runtime under /secret/home\n",
            "agent initialize failed but not a diagnostic: secret-value\n",
            "Error: token=secret-value\n",
        );

        let redacted = redact_log(input);

        assert!(redacted.contains("[CHILD INITIALIZATION ERROR]"));
        assert!(redacted.contains("[CHILD ADAPTER ERROR code=1001]"));
        assert!(redacted.contains("[CHILD PROCESS EXIT code=1]"));
        assert!(redacted.contains("[CHILD ERROR class=sqlite_state_initialization_failed]"));
        assert_eq!(redacted.matches("[REDACTED CHILD OUTPUT]").count(), 2);
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("/secret/home"));
    }

    #[test]
    fn classifies_provider_task_failures_without_retaining_child_text() {
        let input = concat!(
            "Agent reported error (code -32603): Unauthorized: refresh token revoked; token=secret\n",
            "Agent reported error (code 429): usage limit exceeded; retry after 2026-09-10T01:02:03Z account=user@example.com\n",
            "agent task failed: no Codex session exists; credential_payload=secret\n",
        );

        let redacted = redact_log(input);

        assert!(redacted.contains(
            "[CHILD TASK ERROR class=unauthorized code=-32603 action=reauthenticate phase=task rule=provider_unauthorized]"
        ));
        assert!(redacted.contains(
            "[CHILD TASK ERROR class=usage_limit code=429 action=retry phase=task rule=provider_usage_limit retry_at=2026-09-10T01:02:03Z]"
        ));
        assert!(redacted.contains(
            "[CHILD TASK ERROR class=no_session action=retry phase=pre_session rule=provider_no_session]"
        ));
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("user@example.com"));
        assert_eq!(redact_log(&redacted), redacted);
    }

    #[test]
    fn keeps_classified_lines_and_redacts_unknown_lines_in_mixed_input() {
        let redacted = redact_log(concat!(
            "Agent reported error (code -32603): unauthorized refresh token=secret\n",
            "model output account=user@example.com token=secret\n",
        ));

        assert!(redacted.contains("[CHILD TASK ERROR class=unauthorized"));
        assert!(redacted.contains("[REDACTED CHILD OUTPUT]"));
        assert!(!redacted.contains("user@example.com"));
        assert!(!redacted.contains("secret"));
    }

    #[test]
    fn classified_task_failure_does_not_change_healthy_process_state() {
        let directory = tempfile::tempdir().unwrap();
        let adapter = adapter(directory.path(), 4096);
        let receipt = adapter
            .start(&launch(directory.path(), "sleep 5"), &NoSecrets)
            .unwrap();

        let mut observed = receipt.clone();
        for _ in 0..50 {
            observed = adapter.inspect(&receipt).unwrap();
            if observed.observed_state == ObservedProcessState::Healthy {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(observed.observed_state, ObservedProcessState::Healthy);

        let diagnostic =
            redact_log("Agent reported error (code -32603): unauthorized refresh token=secret\n");
        assert!(diagnostic.contains("class=unauthorized"));
        assert_eq!(
            adapter.inspect(&observed).unwrap().observed_state,
            ObservedProcessState::Healthy
        );
        adapter.stop(&observed).unwrap();
    }

    #[test]
    fn classified_diagnostic_survives_supported_log_persistence() {
        let store = crate::SqliteStore::open_in_memory().unwrap();
        let community = crate::CommunityConfig::new(
            "Engineering",
            url::Url::parse("wss://relay.example.test").unwrap(),
        )
        .unwrap();
        store.put_community(&community, 1).unwrap();
        let agent = crate::AgentSpec {
            id: crate::AgentId::new(),
            community_config_id: community.id,
            display_name: "Builder".into(),
            system_prompt: "Build safely.".into(),
            runtime: crate::RuntimeSpec {
                runtime_id: "codex-acp".parse().unwrap(),
                environment: BTreeMap::new(),
            },
            desired_state: crate::DesiredAgentState::Enabled,
        };
        store.put_agent(&agent, 1).unwrap();
        let message =
            redact_log("Agent reported error (code -32603): unauthorized refresh token=secret\n");
        store
            .append_redacted_log(
                agent.id,
                &crate::api::RedactedLogEntry {
                    cursor: "stderr:1".into(),
                    occurred_at: 2,
                    stream: "stderr".into(),
                    redacted_message: message.clone(),
                },
            )
            .unwrap();

        let entries = store.agent_logs(agent.id, None, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].redacted_message, message);
        assert!(entries[0]
            .redacted_message
            .contains("class=unauthorized code=-32603"));
        assert!(!entries[0].redacted_message.contains("secret"));
    }

    #[test]
    fn reconstructs_pre_session_failure_and_immediate_exit_from_supported_surfaces() {
        let directory = tempfile::tempdir().unwrap();
        let adapter = adapter(directory.path(), 4096);
        let script = "printf '%s\\n' '[CHILD TASK ERROR class=no_session action=retry phase=pre_session rule=provider_no_session]'; exit 17";
        let first = adapter
            .start(&launch(directory.path(), script), &NoSecrets)
            .unwrap();
        let first_observed = wait_for_exit(&adapter, &first);
        assert_eq!(first_observed.launch_id, "safe-test-launch");
        assert!(first_observed.generation.is_some());
        assert_eq!(first_observed.exit_code, Some(17));
        assert!(first_observed.wait_outcome.is_some());
        assert!(first_observed.duration_ms.is_some());
        assert_eq!(
            first_observed.app_server_logs,
            AppServerLogsStatus::Configured
        );

        let first_log = directory.path().join("logs/safe-test-launch.stdout.log");
        let mut first_text = String::new();
        for _ in 0..50 {
            first_text = fs::read_to_string(&first_log).unwrap_or_default();
            if first_text.contains("class=no_session") {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(first_text.contains("class=no_session"));
        assert!(first_text.contains("phase=pre_session"));
        assert!(first_text.contains("rule=provider_no_session"));

        let second = adapter
            .start(&launch(directory.path(), script), &NoSecrets)
            .unwrap();
        let second_observed = wait_for_exit(&adapter, &second);
        assert_ne!(first_observed.generation, second_observed.generation);
        assert_eq!(
            second_observed.app_server_logs,
            AppServerLogsStatus::Configured
        );
        let rotated = fs::read_dir(directory.path().join("logs"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("safe-test-launch.stdout.log."))
            })
            .expect("prior launch log was not rotated");
        assert!(fs::read_to_string(rotated)
            .unwrap()
            .contains("class=no_session"));

        let mut unwritable = BTreeMap::new();
        let marker = directory.path().join("not-a-directory");
        fs::write(&marker, b"file").unwrap();
        unwritable.insert(
            "APP_SERVER_LOGS".into(),
            marker.join("child").display().to_string(),
        );
        assert_eq!(
            configure_app_server_logs(
                &mut unwritable,
                directory.path(),
                "safe-test-launch",
                "unwritable"
            ),
            AppServerLogsStatus::Unwritable
        );
    }

    #[test]
    fn rejects_unsafe_task_classification_and_retry_metadata() {
        let input = concat!(
            "Provider said unauthorized: token=secret\n",
            "Agent reported error (code 429): usage limit; retry_at=tomorrow account=secret\n",
            "[CHILD TASK ERROR class=unauthorized code=-32603 action=reauthenticate phase=task rule=provider_unauthorized secret=leak]\n",
        );

        let redacted = redact_log(input);

        assert!(redacted.contains(
            "[CHILD TASK ERROR class=usage_limit code=429 action=retry phase=task rule=provider_usage_limit]"
        ));
        assert_eq!(redacted.matches("[REDACTED CHILD OUTPUT]").count(), 2);
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("tomorrow"));
        assert!(!redacted.contains("account"));
    }

    #[test]
    fn preserves_diagnostics_when_child_writes_them_across_read_boundaries() {
        struct ChunkedReader<R>(R);

        impl<R: Read> Read for ChunkedReader<R> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let limit = buffer.len().min(7);
                self.0.read(&mut buffer[..limit])
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stderr.log");
        let input = ChunkedReader(io::Cursor::new(concat!(
            "Agent reported error (code 1001): process has exited with code 1\n",
            "Error: failed to initialize sqlite state runtime under /secret/home\n",
        )));
        spawn_log_drain(input, path.clone(), 4096);

        for _ in 0..50 {
            if fs::read_to_string(&path).is_ok_and(|contents| contents.lines().count() >= 3) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let persisted = fs::read_to_string(path).unwrap();
        assert!(persisted.contains("[CHILD ADAPTER ERROR code=1001]"));
        assert!(persisted.contains("[CHILD PROCESS EXIT code=1]"));
        assert!(persisted.contains("[CHILD ERROR class=sqlite_state_initialization_failed]"));
        assert!(!persisted.contains("secret"));
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
        assert_eq!(exited.exit_code, Some(0));
        assert!(exited.duration_ms.is_some());
        assert!(matches!(
            exited.wait_outcome,
            Some(ParentWaitOutcome::Exited { code: 0 })
        ));
        thread::sleep(Duration::from_millis(50));
        let stdout = adapter.read_log_tail(&desired.launch_id, false).unwrap();
        let stderr = adapter.read_log_tail(&desired.launch_id, true).unwrap();
        assert!(stdout.len() <= 128);
        assert!(stderr.len() <= 128);
        assert!(stderr.contains("[REDACTED CHILD OUTPUT]"));
        assert!(!stderr.contains("credential"));
    }

    #[test]
    fn sequential_starts_rotate_the_stable_live_log() {
        let directory = tempfile::tempdir().unwrap();
        let adapter = adapter(directory.path(), 4096);
        let desired = launch(directory.path(), "echo '[CHILD INITIALIZATION ERROR]'");
        let first = adapter.start(&desired, &NoSecrets).unwrap();
        let _ = wait_for_exit(&adapter, &first);
        let live_path = adapter.log_path(&desired.launch_id, false).unwrap();
        assert!((0..500).any(|_| {
            let drained = fs::read_to_string(&live_path)
                .map(|contents| contents.contains("[CHILD INITIALIZATION ERROR]"))
                .unwrap_or(false);
            if !drained {
                thread::sleep(Duration::from_millis(20));
            }
            drained
        }));
        let mut second_spec = desired.clone();
        second_spec.harness_arguments = vec![
            "-c".into(),
            "echo '[CHILD ERROR class=sqlite_state_initialization_failed]'".into(),
        ];
        let second = adapter.start(&second_spec, &NoSecrets).unwrap();
        let _ = wait_for_exit(&adapter, &second);
        let live = (0..500)
            .find_map(|_| {
                let contents = fs::read_to_string(&live_path).unwrap();
                if contents.contains("[CHILD ERROR class=sqlite_state_initialization_failed]") {
                    Some(contents)
                } else {
                    thread::sleep(Duration::from_millis(20));
                    None
                }
            })
            .expect("second launch log is drained");
        assert!(live.contains("[CHILD ERROR class=sqlite_state_initialization_failed]"));
        assert!(!live.contains("[CHILD INITIALIZATION ERROR]"));
        let rotated = fs::read_dir(directory.path().join("logs"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(".stdout.log.")
                    && !path.to_string_lossy().ends_with(".meta.json")
            })
            .expect("first launch log is rotated");
        assert!(fs::read_to_string(rotated)
            .unwrap()
            .contains("[CHILD INITIALIZATION ERROR]"));
        assert_ne!(first.generation, second.generation);
    }
}
