//! Durable contract for Server-native local process supervision.
//!
//! This module describes processes; it deliberately does not spawn or inspect them.

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    runtime::{CatalogError, PreflightProbe, RuntimeArtifact, RuntimeCatalog, RuntimeCatalogEntry},
    AgentId, AgentSpec, ValidationError,
};

const MAX_ARGUMENTS: usize = 256;
const MAX_ENVIRONMENT: usize = 256;
pub const HARNESS_AGENT_COMMAND_ENV: &str = "BUZZ_ACP_AGENT_COMMAND";
pub const HARNESS_AGENT_ARGS_ENV: &str = "BUZZ_ACP_AGENT_ARGS";
pub const HARNESS_PRIVATE_KEY_ENV: &str = "BUZZ_PRIVATE_KEY";
pub const HARNESS_RELAY_URL_ENV: &str = "BUZZ_RELAY_URL";
pub const HARNESS_AUTH_TAG_ENV: &str = "BUZZ_AUTH_TAG";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalProcessRole {
    AcpBridge,
    AgentRuntime,
}

/// An executable whose package/version can be compared during adoption.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutableIdentity {
    pub path: String,
    pub package_id: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SecretRef {
    /// Identifier understood by the server's secret store; never the secret value.
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartMode {
    Never,
    OnFailure,
    Always,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RestartPolicy {
    pub mode: RestartMode,
    pub max_attempts: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub stable_after_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HealthPolicy {
    Process {
        startup_grace_ms: u64,
    },
    Tcp {
        host: String,
        port: u16,
        startup_grace_ms: u64,
        interval_ms: u64,
        timeout_ms: u64,
    },
}

/// The immutable child ACP runtime that the supervised Buzz harness launches.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedRuntime {
    pub runtime_id: crate::RuntimeId,
    pub executable: ExecutableIdentity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preflight: Option<PreflightProbe>,
}

/// Complete desired input to one directly supervised `buzz-acp` harness.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LaunchSpec {
    /// Stable identity for reconciliation. It must not be a PID.
    pub launch_id: String,
    pub agent_id: AgentId,
    pub role: LocalProcessRole,
    /// Pinned `buzz-acp`/Sprig executable directly owned by the supervisor.
    pub harness: ExecutableIdentity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harness_arguments: Vec<String>,
    /// Pinned child ACP runtime passed to and launched by the harness.
    pub runtime: ResolvedRuntime,
    /// Non-secret values only. Secrets are resolved immediately before spawn.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_environment: BTreeMap<String, SecretRef>,
    pub working_directory: String,
    pub workspace_path: String,
    pub runtime_path: String,
    /// Stable identity shared by related bridge/runtime processes.
    pub process_group_id: String,
    pub restart: RestartPolicy,
    pub health: HealthPolicy,
}

/// Complete execution-affecting identity persisted with a process receipt.
/// A receipt is adoptable only when this value exactly matches current intent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LaunchIdentity {
    pub role: LocalProcessRole,
    pub harness: ExecutableIdentity,
    pub harness_arguments: Vec<String>,
    pub runtime: ResolvedRuntime,
    pub environment: BTreeMap<String, String>,
    pub secret_environment: BTreeMap<String, SecretRef>,
    pub working_directory: String,
    pub workspace_path: String,
    pub runtime_path: String,
    pub restart: RestartPolicy,
    pub health: HealthPolicy,
}

/// Backend-owned values used to turn durable agent intent into one Local launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalLaunchContext {
    pub launch_id: String,
    pub harness: ExecutableIdentity,
    pub harness_arguments: Vec<String>,
    pub working_directory: String,
    pub workspace_path: String,
    pub runtime_path: String,
    pub process_group_id: String,
    pub restart: RestartPolicy,
    pub health: HealthPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LaunchResolutionError {
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("runtime-required secret {0} shadows a non-secret agent environment value")]
    SecretShadow(String),
}

impl LaunchSpec {
    /// Resolves agent intent exactly once against the immutable runtime catalog.
    /// The resulting launch is the sole process-supervisor source of truth.
    pub fn resolve_local(
        agent: &AgentSpec,
        catalog: &RuntimeCatalog,
        context: LocalLaunchContext,
    ) -> Result<Self, LaunchResolutionError> {
        agent.validate()?;
        catalog.validate()?;
        let runtime = catalog.require(&agent.runtime.runtime_id)?;
        Self::from_runtime_entry(agent, runtime, context)
    }

    fn from_runtime_entry(
        agent: &AgentSpec,
        runtime: &RuntimeCatalogEntry,
        context: LocalLaunchContext,
    ) -> Result<Self, LaunchResolutionError> {
        if agent
            .runtime
            .environment
            .keys()
            .any(|key| reserved_harness_environment_key(key))
        {
            return Err(ValidationError::new(
                "runtime.environment",
                "must not override backend-owned Buzz ACP environment",
            )
            .into());
        }
        let (package_id, sha256) = match &runtime.artifact {
            RuntimeArtifact::LocalExecutable { sha256 } => (runtime.id.to_string(), sha256.clone()),
            RuntimeArtifact::Package {
                manager,
                name,
                version: _,
            } => (format!("{manager}:{name}"), None),
        };
        let resolved_preflight = runtime.preflight.clone();
        let mut secret_environment = BTreeMap::new();
        for required in &runtime.required_secrets {
            if agent
                .runtime
                .environment
                .contains_key(&required.environment_key)
            {
                return Err(LaunchResolutionError::SecretShadow(
                    required.environment_key.clone(),
                ));
            }
            secret_environment.insert(
                required.environment_key.clone(),
                SecretRef {
                    key: required.secret_name.clone(),
                    version: None,
                },
            );
        }
        let launch = Self {
            launch_id: context.launch_id,
            agent_id: agent.id,
            role: LocalProcessRole::AcpBridge,
            harness: context.harness,
            harness_arguments: context.harness_arguments,
            runtime: ResolvedRuntime {
                runtime_id: runtime.id.clone(),
                executable: ExecutableIdentity {
                    path: runtime.command.clone(),
                    package_id,
                    version: runtime.version.clone(),
                    sha256,
                },
                arguments: runtime.arguments.clone(),
                preflight: resolved_preflight,
            },
            environment: agent.runtime.environment.clone(),
            secret_environment,
            working_directory: context.working_directory,
            workspace_path: context.workspace_path,
            runtime_path: context.runtime_path,
            process_group_id: context.process_group_id,
            restart: context.restart,
            health: context.health,
        };
        launch.validate()?;
        Ok(launch)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        nonempty("launch_id", &self.launch_id, 160)?;
        nonempty("process_group_id", &self.process_group_id, 160)?;
        if self.role != LocalProcessRole::AcpBridge {
            return Err(ValidationError::new(
                "role",
                "Local LaunchSpec must directly supervise the ACP bridge",
            ));
        }
        self.harness.validate()?;
        self.runtime.validate()?;
        absolute_path("working_directory", &self.working_directory)?;
        absolute_path("workspace_path", &self.workspace_path)?;
        absolute_path("runtime_path", &self.runtime_path)?;
        if self.harness_arguments.len() + self.runtime.arguments.len() > MAX_ARGUMENTS
            || self
                .harness_arguments
                .iter()
                .chain(&self.runtime.arguments)
                .any(|value| value.contains('\0'))
        {
            return Err(ValidationError::new(
                "arguments",
                "must contain at most 256 NUL-free values",
            ));
        }
        if self.environment.len() + self.secret_environment.len() > MAX_ENVIRONMENT {
            return Err(ValidationError::new(
                "environment",
                "must contain at most 256 entries",
            ));
        }
        for (key, value) in &self.environment {
            if !valid_environment_key(key) || value.contains('\0') {
                return Err(ValidationError::new(
                    "environment",
                    "contains an invalid name or NUL byte",
                ));
            }
            if self.secret_environment.contains_key(key) {
                return Err(ValidationError::new(
                    "secret_environment",
                    "must not shadow a non-secret environment entry",
                ));
            }
        }
        for (key, secret) in &self.secret_environment {
            if !valid_environment_key(key) {
                return Err(ValidationError::new(
                    "secret_environment",
                    "contains an invalid environment variable name",
                ));
            }
            secret.validate()?;
        }
        self.restart.validate()?;
        self.health.validate()?;
        self.harness_runtime_environment()?;
        Ok(())
    }

    /// Verified Buzz ACP runtime-selection environment applied by the process
    /// supervisor after configured non-secret values.
    pub fn harness_runtime_environment(&self) -> Result<BTreeMap<String, String>, ValidationError> {
        if self
            .runtime
            .arguments
            .iter()
            .any(|argument| argument.contains(','))
        {
            return Err(ValidationError::new(
                "runtime.arguments",
                "Buzz ACP comma-separated agent arguments must not contain commas",
            ));
        }
        Ok(BTreeMap::from([
            (
                HARNESS_AGENT_COMMAND_ENV.to_owned(),
                self.runtime.executable.path.clone(),
            ),
            (
                HARNESS_AGENT_ARGS_ENV.to_owned(),
                self.runtime.arguments.join(","),
            ),
        ]))
    }

    /// Determines whether a durable receipt describes this exact desired launch.
    #[must_use]
    pub fn can_adopt(&self, receipt: &ProcessReceipt) -> bool {
        receipt.launch_id == self.launch_id
            && receipt.agent_id == self.agent_id
            && receipt.process_group_id == self.process_group_id
            && receipt.desired == self.identity()
            && receipt.pid > 0
            && !receipt.observed_state.is_terminal()
    }

    #[must_use]
    pub fn identity(&self) -> LaunchIdentity {
        LaunchIdentity {
            role: self.role,
            harness: self.harness.clone(),
            harness_arguments: self.harness_arguments.clone(),
            runtime: self.runtime.clone(),
            environment: self.environment.clone(),
            secret_environment: self.secret_environment.clone(),
            working_directory: self.working_directory.clone(),
            workspace_path: self.workspace_path.clone(),
            runtime_path: self.runtime_path.clone(),
            restart: self.restart.clone(),
            health: self.health.clone(),
        }
    }
}

impl LaunchIdentity {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.role != LocalProcessRole::AcpBridge {
            return Err(ValidationError::new(
                "role",
                "launch identity must describe a supervised ACP bridge",
            ));
        }
        self.harness.validate()?;
        self.runtime.validate()?;
        if self.harness_arguments.len() + self.runtime.arguments.len() > MAX_ARGUMENTS
            || self
                .harness_arguments
                .iter()
                .chain(&self.runtime.arguments)
                .any(|argument| argument.contains('\0'))
        {
            return Err(ValidationError::new(
                "arguments",
                "must contain at most 256 NUL-free values",
            ));
        }
        absolute_path("working_directory", &self.working_directory)?;
        absolute_path("workspace_path", &self.workspace_path)?;
        absolute_path("runtime_path", &self.runtime_path)?;
        for (key, value) in &self.environment {
            if !valid_environment_key(key)
                || value.contains('\0')
                || self.secret_environment.contains_key(key)
            {
                return Err(ValidationError::new(
                    "environment",
                    "contains an invalid or shadowed value",
                ));
            }
        }
        for (key, secret) in &self.secret_environment {
            if !valid_environment_key(key) {
                return Err(ValidationError::new(
                    "secret_environment",
                    "contains an invalid environment variable name",
                ));
            }
            secret.validate()?;
        }
        self.restart.validate()?;
        self.health.validate()
    }
}

impl ExecutableIdentity {
    fn validate(&self) -> Result<(), ValidationError> {
        absolute_path("executable.path", &self.path)?;
        nonempty("executable.package_id", &self.package_id, 160)?;
        nonempty("executable.version", &self.version, 160)?;
        if matches!(self.version.as_str(), "latest" | "stable" | "current")
            || self.version.contains(['*', '^', '~'])
        {
            return Err(ValidationError::new(
                "executable.version",
                "must be an immutable exact version",
            ));
        }
        if self.sha256.as_ref().is_some_and(|digest| {
            digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        }) {
            return Err(ValidationError::new(
                "executable.sha256",
                "must be 64 lowercase hexadecimal characters",
            ));
        }
        Ok(())
    }
}

impl ResolvedRuntime {
    fn validate(&self) -> Result<(), ValidationError> {
        self.executable.validate()?;
        if self.arguments.len() > MAX_ARGUMENTS
            || self
                .arguments
                .iter()
                .any(|argument| argument.contains('\0'))
        {
            return Err(ValidationError::new(
                "runtime.arguments",
                "must contain at most 256 NUL-free values",
            ));
        }
        if let Some(preflight) = &self.preflight {
            absolute_path("runtime.preflight.command", &preflight.command)?;
            if preflight.timeout_seconds == 0 || preflight.timeout_seconds > 300 {
                return Err(ValidationError::new(
                    "runtime.preflight.timeout_seconds",
                    "must be between 1 and 300 seconds",
                ));
            }
            if preflight.arguments.len() > MAX_ARGUMENTS
                || preflight
                    .arguments
                    .iter()
                    .any(|argument| argument.contains('\0'))
            {
                return Err(ValidationError::new(
                    "runtime.preflight.arguments",
                    "must contain at most 256 NUL-free values",
                ));
            }
        }
        Ok(())
    }
}

impl SecretRef {
    fn validate(&self) -> Result<(), ValidationError> {
        nonempty("secret_environment.key", &self.key, 256)?;
        if self.key.contains(char::is_whitespace) || self.key.contains('\0') {
            return Err(ValidationError::new(
                "secret_environment.key",
                "must not contain whitespace or NUL bytes",
            ));
        }
        if self
            .version
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 160)
        {
            return Err(ValidationError::new(
                "secret_environment.version",
                "must contain 1 to 160 characters when present",
            ));
        }
        Ok(())
    }
}

impl RestartPolicy {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.max_backoff_ms < self.initial_backoff_ms {
            return Err(ValidationError::new(
                "restart.max_backoff_ms",
                "must be greater than or equal to initial_backoff_ms",
            ));
        }
        if self.mode == RestartMode::Never && self.max_attempts != 0 {
            return Err(ValidationError::new(
                "restart.max_attempts",
                "must be zero when restart mode is never",
            ));
        }
        Ok(())
    }
}

impl HealthPolicy {
    fn validate(&self) -> Result<(), ValidationError> {
        if let Self::Tcp {
            host,
            port,
            interval_ms,
            timeout_ms,
            ..
        } = self
        {
            if host != "127.0.0.1" && host != "::1" && host != "localhost" {
                return Err(ValidationError::new(
                    "health.host",
                    "local process health checks must use a loopback host",
                ));
            }
            if *port == 0 || *interval_ms == 0 || *timeout_ms == 0 || timeout_ms > interval_ms {
                return Err(ValidationError::new(
                    "health",
                    "TCP port and intervals must be non-zero and timeout must not exceed interval",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedProcessState {
    Starting,
    Healthy,
    Unhealthy,
    Stopping,
    Exited,
    Lost,
}

impl ObservedProcessState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Exited | Self::Lost)
    }

    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        if matches!((self, next), (Self::Starting, Self::Starting))
            || matches!((self, next), (Self::Healthy, Self::Healthy))
            || matches!((self, next), (Self::Unhealthy, Self::Unhealthy))
            || matches!((self, next), (Self::Stopping, Self::Stopping))
            || matches!((self, next), (Self::Exited, Self::Exited))
            || matches!((self, next), (Self::Lost, Self::Lost))
        {
            return true;
        }
        matches!(
            (self, next),
            (
                Self::Starting,
                Self::Healthy | Self::Unhealthy | Self::Stopping | Self::Exited | Self::Lost
            ) | (
                Self::Healthy,
                Self::Unhealthy | Self::Stopping | Self::Exited | Self::Lost
            ) | (
                Self::Unhealthy,
                Self::Healthy | Self::Stopping | Self::Exited | Self::Lost
            ) | (Self::Stopping, Self::Exited | Self::Lost)
        )
    }
}

/// Durable evidence returned by a process adapter and used for later adoption.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessReceipt {
    pub launch_id: String,
    pub agent_id: AgentId,
    pub process_group_id: String,
    pub desired: LaunchIdentity,
    pub pid: u32,
    pub started_at_unix_ms: u64,
    /// Linux `/proc/<pid>/stat` start-time ticks prevent PID-reuse adoption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start_ticks: Option<u64>,
    /// Original argv[0] observed at spawn, checked again before re-adoption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_path: Option<String>,
    pub observed_state: ObservedProcessState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl ProcessReceipt {
    pub fn validate(&self) -> Result<(), ValidationError> {
        nonempty("launch_id", &self.launch_id, 160)?;
        nonempty("process_group_id", &self.process_group_id, 160)?;
        self.desired.validate()?;
        if self.pid == 0 {
            return Err(ValidationError::new("pid", "must be non-zero"));
        }
        if self.process_start_ticks == Some(0)
            || self.command_path.as_deref().is_some_and(str::is_empty)
        {
            return Err(ValidationError::new(
                "process_identity",
                "contains an invalid live-process identity",
            ));
        }
        if self.exit_code.is_some() && !self.observed_state.is_terminal() {
            return Err(ValidationError::new(
                "exit_code",
                "may only be present for a terminal process",
            ));
        }
        Ok(())
    }

    pub fn observe(
        &mut self,
        state: ObservedProcessState,
        exit_code: Option<i32>,
    ) -> Result<(), ValidationError> {
        if !self.observed_state.can_transition_to(state) {
            return Err(ValidationError::new(
                "observed_state",
                "contains an invalid process lifecycle transition",
            ));
        }
        if exit_code.is_some() && !state.is_terminal() {
            return Err(ValidationError::new(
                "exit_code",
                "may only be present for a terminal process",
            ));
        }
        self.observed_state = state;
        self.exit_code = exit_code;
        Ok(())
    }
}

fn nonempty(field: &'static str, value: &str, max: usize) -> Result<(), ValidationError> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(ValidationError::new(
            field,
            format!("must contain 1 to {max} NUL-free characters"),
        ));
    }
    Ok(())
}

fn absolute_path(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.contains('\0') || !Path::new(value).is_absolute() {
        return Err(ValidationError::new(
            field,
            "must be an absolute NUL-free path",
        ));
    }
    Ok(())
}

fn valid_environment_key(key: &str) -> bool {
    let mut characters = key.chars();
    matches!(characters.next(), Some('A'..='Z' | '_'))
        && characters.all(|character| matches!(character, 'A'..='Z' | '0'..='9' | '_'))
}

fn reserved_harness_environment_key(key: &str) -> bool {
    matches!(
        key,
        HARNESS_AGENT_COMMAND_ENV
            | HARNESS_AGENT_ARGS_ENV
            | HARNESS_PRIVATE_KEY_ENV
            | HARNESS_RELAY_URL_ENV
            | HARNESS_AUTH_TAG_ENV
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommunityConfigId, RuntimeId, RuntimeSpec, SecretReference};

    fn spec() -> LaunchSpec {
        LaunchSpec {
            launch_id: "agent-main-acp-v1".into(),
            agent_id: AgentId::new(),
            role: LocalProcessRole::AcpBridge,
            harness: ExecutableIdentity {
                path: "/opt/buzz/bin/buzz-acp".into(),
                package_id: "buzz-acp".into(),
                version: "1.2.3".into(),
                sha256: Some("a".repeat(64)),
            },
            harness_arguments: vec!["serve".into()],
            runtime: ResolvedRuntime {
                runtime_id: "codex-acp".parse().unwrap(),
                executable: ExecutableIdentity {
                    path: "/opt/buzz/bin/codex-acp".into(),
                    package_id: "npm:@zed-industries/codex-acp".into(),
                    version: "0.8.0".into(),
                    sha256: None,
                },
                arguments: vec!["--stdio".into()],
                preflight: None,
            },
            environment: BTreeMap::from([("RUST_LOG".into(), "info".into())]),
            secret_environment: BTreeMap::from([(
                "BUZZ_PRIVATE_KEY".into(),
                SecretRef {
                    key: "agents/main/nostr".into(),
                    version: Some("3".into()),
                },
            )]),
            working_directory: "/srv/buzz".into(),
            workspace_path: "/srv/buzz/workspaces/main".into(),
            runtime_path: "/srv/buzz/runtime/main".into(),
            process_group_id: "agent-main-v1".into(),
            restart: RestartPolicy {
                mode: RestartMode::OnFailure,
                max_attempts: 5,
                initial_backoff_ms: 250,
                max_backoff_ms: 30_000,
                stable_after_ms: 60_000,
            },
            health: HealthPolicy::Process {
                startup_grace_ms: 5_000,
            },
        }
    }

    fn receipt(spec: &LaunchSpec) -> ProcessReceipt {
        ProcessReceipt {
            launch_id: spec.launch_id.clone(),
            agent_id: spec.agent_id,
            process_group_id: spec.process_group_id.clone(),
            desired: spec.identity(),
            pid: 42,
            started_at_unix_ms: 1_700_000_000_000,
            process_start_ticks: Some(1),
            command_path: Some(spec.harness.path.clone()),
            observed_state: ObservedProcessState::Healthy,
            exit_code: None,
        }
    }

    #[test]
    fn valid_contract_has_stable_wire_round_trip() {
        let spec = spec();
        spec.validate().unwrap();
        let value = serde_json::to_value(&spec).unwrap();
        assert_eq!(value["role"], "acp_bridge");
        assert_eq!(value["health"]["kind"], "process");
        assert_eq!(serde_json::from_value::<LaunchSpec>(value).unwrap(), spec);
    }

    #[test]
    fn harness_runtime_selection_is_backend_owned_and_exact() {
        let mut launch = spec();
        launch.runtime.arguments = vec!["acp".into(), "--stdio".into()];
        let environment = launch.harness_runtime_environment().unwrap();
        assert_eq!(
            environment[HARNESS_AGENT_COMMAND_ENV],
            "/opt/buzz/bin/codex-acp"
        );
        assert_eq!(environment[HARNESS_AGENT_ARGS_ENV], "acp,--stdio");
    }

    #[test]
    fn rejects_secret_shadowing_and_unpinned_executables() {
        let mut launch = spec();
        launch
            .environment
            .insert("BUZZ_PRIVATE_KEY".into(), "leak".into());
        assert_eq!(launch.validate().unwrap_err().field, "secret_environment");

        let mut launch = spec();
        launch.harness.version.clear();
        assert_eq!(launch.validate().unwrap_err().field, "executable.version");
    }

    #[test]
    fn adoption_requires_exact_live_identity() {
        let launch = spec();
        let mut durable = receipt(&launch);
        assert!(launch.can_adopt(&durable));
        durable.desired.runtime.executable.version = "1.2.4".into();
        assert!(!launch.can_adopt(&durable));
        durable.desired = launch.identity();
        durable
            .desired
            .environment
            .insert("RUST_LOG".into(), "debug".into());
        assert!(!launch.can_adopt(&durable));
        durable.desired = launch.identity();
        durable.observed_state = ObservedProcessState::Exited;
        durable.exit_code = Some(1);
        assert!(!launch.can_adopt(&durable));
    }

    #[test]
    fn terminal_receipts_cannot_be_resurrected() {
        let launch = spec();
        let mut durable = receipt(&launch);
        durable
            .observe(ObservedProcessState::Exited, Some(1))
            .unwrap();
        assert!(durable
            .observe(ObservedProcessState::Starting, None)
            .is_err());
    }

    #[test]
    fn local_resolution_uses_catalog_identity_arguments_and_secret_references() {
        let agent = AgentSpec {
            id: AgentId::new(),
            community_config_id: CommunityConfigId::new(),
            display_name: "Codex".into(),
            system_prompt: "Build and verify.".into(),
            runtime: RuntimeSpec {
                runtime_id: RuntimeId::parse("codex-acp").unwrap(),
                environment: BTreeMap::from([("RUST_LOG".into(), "info".into())]),
            },
            desired_state: crate::DesiredAgentState::Enabled,
        };
        let runtime = RuntimeCatalogEntry {
            id: RuntimeId::parse("codex-acp").unwrap(),
            version: "1.2.3".into(),
            artifact: RuntimeArtifact::Package {
                manager: "npm".into(),
                name: "@zed-industries/codex-acp".into(),
                version: "1.2.3".into(),
            },
            command: "/opt/buzz/bin/codex-acp".into(),
            arguments: vec!["--stdio".into()],
            preflight: Some(PreflightProbe {
                timeout_seconds: 15,
                command: "/ignored/catalog/probe".into(),
                arguments: vec!["ignored".into()],
            }),
            required_secrets: vec![SecretReference {
                environment_key: "OPENAI_API_KEY".into(),
                secret_name: "agents/codex/openai".into(),
            }],
        };
        let catalog = RuntimeCatalog {
            runtimes: vec![runtime],
        };
        let base = spec();
        let resolved = LaunchSpec::resolve_local(
            &agent,
            &catalog,
            LocalLaunchContext {
                launch_id: base.launch_id,
                harness: base.harness,
                harness_arguments: base.harness_arguments,
                working_directory: base.working_directory,
                workspace_path: base.workspace_path,
                runtime_path: base.runtime_path,
                process_group_id: base.process_group_id,
                restart: base.restart,
                health: base.health,
            },
        )
        .unwrap();
        assert_eq!(resolved.harness.path, "/opt/buzz/bin/buzz-acp");
        assert_eq!(resolved.role, LocalProcessRole::AcpBridge);
        assert_eq!(resolved.runtime.executable.path, "/opt/buzz/bin/codex-acp");
        assert_eq!(resolved.runtime.executable.version, "1.2.3");
        assert_eq!(resolved.runtime.arguments, ["--stdio"]);
        assert_eq!(
            resolved.runtime.preflight.as_ref().unwrap().arguments,
            ["ignored"]
        );
        assert_eq!(
            resolved.runtime.preflight.as_ref().unwrap().command,
            "/ignored/catalog/probe"
        );
        assert_eq!(
            resolved.secret_environment["OPENAI_API_KEY"].key,
            "agents/codex/openai"
        );
        assert!(!resolved.environment.contains_key("OPENAI_API_KEY"));

        let mut plaintext_agent = agent.clone();
        plaintext_agent
            .runtime
            .environment
            .insert("OPENAI_API_KEY".into(), "plaintext".into());
        assert!(matches!(
            LaunchSpec::resolve_local(
                &plaintext_agent,
                &catalog,
                LocalLaunchContext {
                    launch_id: resolved.launch_id.clone(),
                    harness: resolved.harness.clone(),
                    harness_arguments: resolved.harness_arguments.clone(),
                    working_directory: resolved.working_directory.clone(),
                    workspace_path: resolved.workspace_path.clone(),
                    runtime_path: resolved.runtime_path.clone(),
                    process_group_id: resolved.process_group_id.clone(),
                    restart: resolved.restart.clone(),
                    health: resolved.health.clone(),
                },
            ),
            Err(LaunchResolutionError::SecretShadow(key)) if key == "OPENAI_API_KEY"
        ));

        let mut overriding_agent = agent.clone();
        overriding_agent.runtime.environment.insert(
            HARNESS_RELAY_URL_ENV.into(),
            "wss://attacker.invalid".into(),
        );
        assert!(matches!(
            LaunchSpec::resolve_local(
                &overriding_agent,
                &catalog,
                LocalLaunchContext {
                    launch_id: resolved.launch_id.clone(),
                    harness: resolved.harness.clone(),
                    harness_arguments: resolved.harness_arguments.clone(),
                    working_directory: resolved.working_directory.clone(),
                    workspace_path: resolved.workspace_path.clone(),
                    runtime_path: resolved.runtime_path.clone(),
                    process_group_id: resolved.process_group_id.clone(),
                    restart: resolved.restart.clone(),
                    health: resolved.health.clone(),
                },
            ),
            Err(LaunchResolutionError::Validation(error))
                if error.field == "runtime.environment"
        ));

        let mut local_catalog = catalog;
        local_catalog.runtimes[0].artifact = RuntimeArtifact::LocalExecutable {
            sha256: Some("b".repeat(64)),
        };
        let local = LaunchSpec::resolve_local(
            &agent,
            &local_catalog,
            LocalLaunchContext {
                launch_id: resolved.launch_id,
                harness: resolved.harness,
                harness_arguments: resolved.harness_arguments,
                working_directory: resolved.working_directory,
                workspace_path: resolved.workspace_path,
                runtime_path: resolved.runtime_path,
                process_group_id: resolved.process_group_id,
                restart: resolved.restart,
                health: resolved.health,
            },
        )
        .unwrap();
        assert_eq!(local.runtime.executable.package_id, "codex-acp");
        assert_eq!(local.runtime.executable.sha256, Some("b".repeat(64)));
    }
}
