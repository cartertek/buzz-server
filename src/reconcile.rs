//! Idempotent reconciliation of durable agent intent with Local processes.

use crate::{
    storage::{AgentRepository, DurableOperation, StorageError},
    AgentId, AgentSpec, DesiredAgentState, LaunchSpec, OperationStatus, ProcessReceipt,
};

use crate::supervisor::{ProcessSupervisor, SecretResolver, SupervisorError};

/// Persistence hook for process receipts. Implementations must durably commit a
/// receipt before an operation is reported successful.
pub trait ProcessReceiptRepository {
    fn get_receipt(&self, agent_id: AgentId) -> Result<Option<ProcessReceipt>, StorageError>;
    fn put_receipt(&self, receipt: &ProcessReceipt) -> Result<(), StorageError>;
    fn delete_receipt(&self, agent_id: AgentId) -> Result<(), StorageError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileOutcome {
    Deferred,
    Unchanged,
    Adopted,
    Started,
    Stopped,
    /// Preflight failed before a harness process was created. There is no new
    /// process presence and therefore no receipt to persist.
    FailedPreflight,
    /// Spawn failed before a receipt was produced. Callers must not announce
    /// presence or transition an operation to success.
    NoPresence,
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error("operation does not target the requested agent")]
    OperationAgentMismatch,
    #[error("enabled agent requires a resolved Local launch")]
    MissingLaunch,
    #[error("resolved launch does not target the requested agent")]
    LaunchAgentMismatch,
}

pub struct Reconciler<'a, A, R, S> {
    agents: &'a A,
    receipts: &'a R,
    supervisor: &'a S,
    secrets: &'a dyn SecretResolver,
}

impl<'a, A, R, S> Reconciler<'a, A, R, S>
where
    A: AgentRepository,
    R: ProcessReceiptRepository,
    S: ProcessSupervisor,
{
    pub fn new(
        agents: &'a A,
        receipts: &'a R,
        supervisor: &'a S,
        secrets: &'a dyn SecretResolver,
    ) -> Self {
        Self {
            agents,
            receipts,
            supervisor,
            secrets,
        }
    }

    /// Reconciles one durable operation. Pending work is deferred, terminal
    /// work is never replayed, and only Running work may mutate process state.
    pub fn reconcile(
        &self,
        agent_id: AgentId,
        operation: &DurableOperation,
        launch: Option<&LaunchSpec>,
    ) -> Result<ReconcileOutcome, ReconcileError> {
        if operation.agent_id != Some(agent_id) {
            return Err(ReconcileError::OperationAgentMismatch);
        }
        match operation.status {
            OperationStatus::Pending => return Ok(ReconcileOutcome::Deferred),
            status if status.is_terminal() => return Ok(ReconcileOutcome::Unchanged),
            OperationStatus::Running => {}
            _ => return Ok(ReconcileOutcome::Deferred),
        }
        let agent = self
            .agents
            .get_agent(agent_id)?
            .ok_or(StorageError::NotFound)?;
        self.reconcile_desired(&agent, launch)
    }

    /// Startup/reopen entry point. It applies durable desired state without an
    /// operation replay while retaining exact receipt adoption semantics.
    pub fn reconcile_desired(
        &self,
        agent: &AgentSpec,
        launch: Option<&LaunchSpec>,
    ) -> Result<ReconcileOutcome, ReconcileError> {
        agent
            .validate()
            .map_err(|error| StorageError::InvalidData(error.to_string()))?;
        let receipt = self.receipts.get_receipt(agent.id)?;
        match agent.desired_state {
            DesiredAgentState::Disabled | DesiredAgentState::Deleted => {
                self.ensure_stopped(agent.id, receipt.as_ref())
            }
            DesiredAgentState::Enabled => {
                let desired = launch.ok_or(ReconcileError::MissingLaunch)?;
                if desired.agent_id != agent.id {
                    return Err(ReconcileError::LaunchAgentMismatch);
                }
                self.ensure_started(desired, receipt.as_ref())
            }
        }
    }

    fn ensure_started(
        &self,
        desired: &LaunchSpec,
        receipt: Option<&ProcessReceipt>,
    ) -> Result<ReconcileOutcome, ReconcileError> {
        if let Some(durable) = receipt {
            let observed = self.supervisor.inspect(durable)?;
            self.receipts.put_receipt(&observed)?;
            if desired.can_adopt(&observed) {
                return Ok(ReconcileOutcome::Adopted);
            }
            if !observed.observed_state.is_terminal() {
                let stopped = self.supervisor.stop(&observed)?;
                self.receipts.put_receipt(&stopped)?;
            }
        }

        match self.supervisor.start(desired, self.secrets) {
            Ok(started) => {
                // This write is the presence boundary: callers may only publish
                // presence after the durable receipt has been committed.
                self.receipts.put_receipt(&started)?;
                Ok(ReconcileOutcome::Started)
            }
            Err(SupervisorError::Preflight(_)) => Ok(ReconcileOutcome::FailedPreflight),
            Err(SupervisorError::SecretResolution | SupervisorError::InvalidSpec(_)) => {
                Ok(ReconcileOutcome::NoPresence)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn ensure_stopped(
        &self,
        agent_id: AgentId,
        receipt: Option<&ProcessReceipt>,
    ) -> Result<ReconcileOutcome, ReconcileError> {
        let Some(durable) = receipt else {
            return Ok(ReconcileOutcome::Unchanged);
        };
        let observed = self.supervisor.inspect(durable)?;
        if observed.observed_state.is_terminal() {
            self.receipts.delete_receipt(agent_id)?;
            return Ok(ReconcileOutcome::Unchanged);
        }
        let stopped = self.supervisor.stop(&observed)?;
        self.receipts.put_receipt(&stopped)?;
        self.receipts.delete_receipt(agent_id)?;
        Ok(ReconcileOutcome::Stopped)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use super::*;
    use crate::{
        launch::{
            ExecutableIdentity, HealthPolicy, LocalProcessRole, ResolvedRuntime, RestartMode,
            RestartPolicy, SecretRef,
        },
        CommunityConfigId, OperationId, OperationKind, RuntimeId, RuntimeSpec,
    };

    struct AgentMemory(Mutex<Option<AgentSpec>>);

    impl AgentRepository for AgentMemory {
        fn put_agent(&self, spec: &AgentSpec, _now: i64) -> Result<(), StorageError> {
            *self.0.lock().unwrap() = Some(spec.clone());
            Ok(())
        }

        fn get_agent(&self, id: AgentId) -> Result<Option<AgentSpec>, StorageError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .clone()
                .filter(|agent| agent.id == id))
        }
    }

    #[derive(Default)]
    struct ReceiptMemory(Mutex<Option<ProcessReceipt>>);

    impl ProcessReceiptRepository for ReceiptMemory {
        fn get_receipt(&self, agent_id: AgentId) -> Result<Option<ProcessReceipt>, StorageError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .clone()
                .filter(|receipt| receipt.agent_id == agent_id))
        }

        fn put_receipt(&self, receipt: &ProcessReceipt) -> Result<(), StorageError> {
            *self.0.lock().unwrap() = Some(receipt.clone());
            Ok(())
        }

        fn delete_receipt(&self, _agent_id: AgentId) -> Result<(), StorageError> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    enum StartFailure {
        Preflight,
        NoPresence,
    }

    #[derive(Default)]
    struct FakeSupervisor {
        starts: Mutex<u32>,
        stops: Mutex<u32>,
        failure: Mutex<Option<StartFailure>>,
    }

    impl ProcessSupervisor for FakeSupervisor {
        fn start(
            &self,
            desired: &LaunchSpec,
            _secrets: &dyn SecretResolver,
        ) -> Result<ProcessReceipt, SupervisorError> {
            *self.starts.lock().unwrap() += 1;
            match self.failure.lock().unwrap().take() {
                Some(StartFailure::Preflight) => {
                    return Err(SupervisorError::Preflight("exit 1".into()));
                }
                Some(StartFailure::NoPresence) => {
                    return Err(SupervisorError::SecretResolution);
                }
                None => {}
            }
            Ok(ProcessReceipt {
                launch_id: desired.launch_id.clone(),
                agent_id: desired.agent_id,
                process_group_id: desired.process_group_id.clone(),
                desired: desired.identity(),
                pid: 42,
                started_at_unix_ms: 1,
                process_start_ticks: Some(1),
                command_path: Some(desired.harness.path.clone()),
                observed_state: crate::ObservedProcessState::Healthy,
                exit_code: None,
            })
        }

        fn inspect(&self, receipt: &ProcessReceipt) -> Result<ProcessReceipt, SupervisorError> {
            Ok(receipt.clone())
        }

        fn stop(&self, receipt: &ProcessReceipt) -> Result<ProcessReceipt, SupervisorError> {
            *self.stops.lock().unwrap() += 1;
            let mut stopped = receipt.clone();
            stopped
                .observe(crate::ObservedProcessState::Exited, Some(0))
                .unwrap();
            Ok(stopped)
        }
    }

    struct NoSecrets;

    impl SecretResolver for NoSecrets {
        fn resolve(&self, _reference: &SecretRef) -> Result<String, SupervisorError> {
            Err(SupervisorError::SecretResolution)
        }
    }

    fn agent(state: DesiredAgentState) -> AgentSpec {
        AgentSpec {
            id: AgentId::new(),
            community_config_id: CommunityConfigId::new(),
            display_name: "Test agent".into(),
            system_prompt: "Test safely.".into(),
            runtime: RuntimeSpec {
                runtime_id: RuntimeId::parse("test-runtime").unwrap(),
                environment: BTreeMap::new(),
            },
            desired_state: state,
        }
    }

    fn launch(agent_id: AgentId, version: &str) -> LaunchSpec {
        LaunchSpec {
            launch_id: "test-launch".into(),
            agent_id,
            role: LocalProcessRole::AcpBridge,
            harness: ExecutableIdentity {
                path: "/bin/true".into(),
                package_id: "test-harness".into(),
                version: "1".into(),
                sha256: None,
            },
            harness_arguments: Vec::new(),
            runtime: ResolvedRuntime {
                runtime_id: RuntimeId::parse("test-runtime").unwrap(),
                executable: ExecutableIdentity {
                    path: "/bin/true".into(),
                    package_id: "test-runtime".into(),
                    version: version.into(),
                    sha256: None,
                },
                arguments: Vec::new(),
                preflight: None,
            },
            environment: BTreeMap::new(),
            secret_environment: BTreeMap::new(),
            working_directory: "/tmp".into(),
            workspace_path: "/tmp".into(),
            runtime_path: "/tmp".into(),
            process_group_id: "test-group".into(),
            restart: RestartPolicy {
                mode: RestartMode::Never,
                max_attempts: 0,
                initial_backoff_ms: 0,
                max_backoff_ms: 0,
                stable_after_ms: 0,
            },
            health: HealthPolicy::Process {
                startup_grace_ms: 0,
            },
        }
    }

    fn operation(agent_id: AgentId) -> DurableOperation {
        DurableOperation {
            id: OperationId::new(),
            kind: OperationKind::EnableAgent,
            status: OperationStatus::Running,
            agent_id: Some(agent_id),
            correlation_id: String::new(),
            error_code: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn restart_reopens_and_adopts_without_duplicate_start() {
        let agent = agent(DesiredAgentState::Enabled);
        let desired = launch(agent.id, "1");
        let agents = AgentMemory(Mutex::new(Some(agent.clone())));
        let receipts = ReceiptMemory::default();
        let supervisor = FakeSupervisor::default();
        let reconciler = Reconciler::new(&agents, &receipts, &supervisor, &NoSecrets);
        assert_eq!(
            reconciler
                .reconcile(agent.id, &operation(agent.id), Some(&desired))
                .unwrap(),
            ReconcileOutcome::Started
        );
        assert_eq!(
            reconciler
                .reconcile_desired(&agent, Some(&desired))
                .unwrap(),
            ReconcileOutcome::Adopted
        );
        assert_eq!(*supervisor.starts.lock().unwrap(), 1);
    }

    #[test]
    fn identity_drift_stops_then_replaces_process() {
        let agent = agent(DesiredAgentState::Enabled);
        let old = launch(agent.id, "1");
        let desired = launch(agent.id, "2");
        let agents = AgentMemory(Mutex::new(Some(agent.clone())));
        let receipts = ReceiptMemory::default();
        let supervisor = FakeSupervisor::default();
        receipts
            .put_receipt(&supervisor.start(&old, &NoSecrets).unwrap())
            .unwrap();
        let reconciler = Reconciler::new(&agents, &receipts, &supervisor, &NoSecrets);
        assert_eq!(
            reconciler
                .reconcile_desired(&agent, Some(&desired))
                .unwrap(),
            ReconcileOutcome::Started
        );
        assert_eq!(*supervisor.stops.lock().unwrap(), 1);
        assert_eq!(*supervisor.starts.lock().unwrap(), 2);
    }

    #[test]
    fn disabled_state_stops_and_removes_presence() {
        let agent = agent(DesiredAgentState::Disabled);
        let desired = launch(agent.id, "1");
        let agents = AgentMemory(Mutex::new(Some(agent.clone())));
        let receipts = ReceiptMemory::default();
        let supervisor = FakeSupervisor::default();
        receipts
            .put_receipt(&supervisor.start(&desired, &NoSecrets).unwrap())
            .unwrap();
        let reconciler = Reconciler::new(&agents, &receipts, &supervisor, &NoSecrets);
        assert_eq!(
            reconciler.reconcile_desired(&agent, None).unwrap(),
            ReconcileOutcome::Stopped
        );
        assert!(receipts.get_receipt(agent.id).unwrap().is_none());
    }

    #[test]
    fn failed_preflight_and_spawn_never_create_presence() {
        for (failure, expected) in [
            (StartFailure::Preflight, ReconcileOutcome::FailedPreflight),
            (StartFailure::NoPresence, ReconcileOutcome::NoPresence),
        ] {
            let agent = agent(DesiredAgentState::Enabled);
            let desired = launch(agent.id, "1");
            let agents = AgentMemory(Mutex::new(Some(agent.clone())));
            let receipts = ReceiptMemory::default();
            let supervisor = FakeSupervisor::default();
            *supervisor.failure.lock().unwrap() = Some(failure);
            let reconciler = Reconciler::new(&agents, &receipts, &supervisor, &NoSecrets);
            assert_eq!(
                reconciler
                    .reconcile_desired(&agent, Some(&desired))
                    .unwrap(),
                expected
            );
            assert!(receipts.get_receipt(agent.id).unwrap().is_none());
        }
    }
}
