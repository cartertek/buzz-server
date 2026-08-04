//! SQLite-backed durable state and repository operations.

use std::{
    path::Path,
    str::FromStr,
    sync::{Mutex, MutexGuard},
};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::{
    AgentId, AgentSpec, CommunityConfig, CommunityConfigId, ErrorCode, OperationId, OperationKind,
    OperationStatus,
};

const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_initial.sql"))];

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("persistence failure: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("stored data is invalid: {0}")]
    InvalidData(String),
    #[error("record not found")]
    NotFound,
    #[error("record conflicts with existing state: {0}")]
    Conflict(String),
    #[error("storage lock is poisoned")]
    LockPoisoned,
}

impl From<serde_json::Error> for StorageError {
    fn from(error: serde_json::Error) -> Self {
        Self::InvalidData(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableOperation {
    pub id: OperationId,
    pub kind: OperationKind,
    pub status: OperationStatus,
    pub agent_id: Option<AgentId>,
    pub error_code: Option<ErrorCode>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuditRecord {
    pub sequence: i64,
    pub occurred_at: i64,
    pub actor_principal: String,
    pub authentication_method: String,
    pub community_config_id: Option<CommunityConfigId>,
    pub operation_id: Option<OperationId>,
    pub correlation_id: String,
    pub idempotency_key: Option<String>,
    pub action: String,
    pub subject_type: String,
    pub subject_id: Option<String>,
    pub outcome: String,
    /// A deliberately opaque, pre-redacted description. Raw request bodies and credentials
    /// must never be supplied here.
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAuditRecord<'a> {
    pub occurred_at: i64,
    pub actor_principal: &'a str,
    pub authentication_method: &'a str,
    pub community_config_id: Option<CommunityConfigId>,
    pub operation_id: Option<OperationId>,
    pub correlation_id: &'a str,
    pub idempotency_key: Option<&'a str>,
    pub action: &'a str,
    pub subject_type: &'a str,
    pub subject_id: Option<&'a str>,
    pub outcome: &'a str,
    pub redacted_detail: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyRecord {
    pub scope: String,
    pub key: String,
    pub request_hash: String,
    pub operation_id: OperationId,
    pub created_at: i64,
}

/// Persistence boundary for community configuration state.
pub trait CommunityRepository {
    fn put_community(&self, config: &CommunityConfig, now: i64) -> Result<(), StorageError>;
    fn get_community(&self, id: CommunityConfigId)
        -> Result<Option<CommunityConfig>, StorageError>;
}

/// Persistence boundary for desired agent state.
pub trait AgentRepository {
    fn put_agent(&self, spec: &AgentSpec, now: i64) -> Result<(), StorageError>;
    fn get_agent(&self, id: AgentId) -> Result<Option<AgentSpec>, StorageError>;
}

/// Persistence boundary for durable operation state machines.
pub trait OperationRepository {
    fn create_operation(&self, operation: &DurableOperation) -> Result<(), StorageError>;
    fn get_operation(&self, id: OperationId) -> Result<Option<DurableOperation>, StorageError>;
    fn transition_operation(
        &self,
        id: OperationId,
        next: OperationStatus,
        error_code: Option<ErrorCode>,
        now: i64,
    ) -> Result<(), StorageError>;
}

/// Append/query boundary for security audit history.
pub trait AuditRepository {
    fn append_audit(&self, record: NewAuditRecord<'_>) -> Result<i64, StorageError>;
    fn audit_for_subject(
        &self,
        subject_type: &str,
        subject_id: &str,
    ) -> Result<Vec<AuditRecord>, StorageError>;
}

/// Atomic replay boundary for idempotent commands.
pub trait IdempotencyRepository {
    fn claim_idempotency(&self, record: &IdempotencyRecord) -> Result<OperationId, StorageError>;
}

pub struct SqliteStore {
    connection: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, StorageError> {
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        run_migrations(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StorageError> {
        self.connection
            .lock()
            .map_err(|_| StorageError::LockPoisoned)
    }

    pub fn put_community(&self, config: &CommunityConfig, now: i64) -> Result<(), StorageError> {
        config
            .validate()
            .map_err(|e| StorageError::InvalidData(e.to_string()))?;
        let document = serde_json::to_string(config)?;
        self.connection()?.execute(
            "INSERT INTO community_configs(id, document, updated_at) VALUES(?1, ?2, ?3) \
             ON CONFLICT(id) DO UPDATE SET document=excluded.document, updated_at=excluded.updated_at",
            params![config.id.to_string(), document, now],
        )?;
        Ok(())
    }

    pub fn get_community(
        &self,
        id: CommunityConfigId,
    ) -> Result<Option<CommunityConfig>, StorageError> {
        let document: Option<String> = self
            .connection()?
            .query_row(
                "SELECT document FROM community_configs WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        document
            .map(|value| serde_json::from_str(&value).map_err(StorageError::from))
            .transpose()
    }

    pub fn put_agent(&self, spec: &AgentSpec, now: i64) -> Result<(), StorageError> {
        spec.validate()
            .map_err(|e| StorageError::InvalidData(e.to_string()))?;
        let document = serde_json::to_string(spec)?;
        self.connection()?.execute(
            "INSERT INTO agent_specs(id, community_config_id, document, updated_at) VALUES(?1, ?2, ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET community_config_id=excluded.community_config_id, document=excluded.document, updated_at=excluded.updated_at",
            params![spec.id.to_string(), spec.community_config_id.to_string(), document, now],
        ).map_err(map_constraint)?;
        Ok(())
    }

    pub fn get_agent(&self, id: AgentId) -> Result<Option<AgentSpec>, StorageError> {
        let document: Option<String> = self
            .connection()?
            .query_row(
                "SELECT document FROM agent_specs WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        document
            .map(|value| serde_json::from_str(&value).map_err(StorageError::from))
            .transpose()
    }

    pub fn create_operation(&self, operation: &DurableOperation) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT INTO operations(id, kind, status, agent_id, error_code, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![operation.id.to_string(), json_name(operation.kind)?, json_name(operation.status)?, operation.agent_id.map(|id| id.to_string()), operation.error_code.map(json_name).transpose()?, operation.created_at, operation.updated_at],
        ).map_err(map_constraint)?;
        Ok(())
    }

    pub fn get_operation(&self, id: OperationId) -> Result<Option<DurableOperation>, StorageError> {
        self.connection()?.query_row(
            "SELECT id, kind, status, agent_id, error_code, created_at, updated_at FROM operations WHERE id=?1",
            [id.to_string()], decode_operation,
        ).optional().map_err(StorageError::from)
    }

    pub fn transition_operation(
        &self,
        id: OperationId,
        next: OperationStatus,
        error_code: Option<ErrorCode>,
        now: i64,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT status FROM operations WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let current: OperationStatus = current
            .ok_or(StorageError::NotFound)
            .and_then(|v| parse_json_name(&v))?;
        if !current.can_transition_to(next) {
            return Err(StorageError::Conflict(format!(
                "invalid operation transition from {current:?} to {next:?}"
            )));
        }
        transaction.execute(
            "UPDATE operations SET status=?2, error_code=?3, updated_at=?4 WHERE id=?1",
            params![
                id.to_string(),
                json_name(next)?,
                error_code.map(json_name).transpose()?,
                now
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn append_audit(&self, record: NewAuditRecord<'_>) -> Result<i64, StorageError> {
        if record.redacted_detail.is_some_and(contains_secret_marker) {
            return Err(StorageError::InvalidData(
                "audit detail contains a credential-like marker".into(),
            ));
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO audit_records(occurred_at, actor_principal, authentication_method, community_config_id, operation_id, correlation_id, idempotency_key, action, subject_type, subject_id, outcome, detail) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.occurred_at,
                record.actor_principal,
                record.authentication_method,
                record.community_config_id.map(|id| id.to_string()),
                record.operation_id.map(|id| id.to_string()),
                record.correlation_id,
                record.idempotency_key,
                record.action,
                record.subject_type,
                record.subject_id,
                record.outcome,
                record.redacted_detail,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn audit_for_subject(
        &self,
        subject_type: &str,
        subject_id: &str,
    ) -> Result<Vec<AuditRecord>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT sequence, occurred_at, actor_principal, authentication_method, community_config_id, operation_id, correlation_id, idempotency_key, action, subject_type, subject_id, outcome, detail FROM audit_records WHERE subject_type=?1 AND subject_id=?2 ORDER BY sequence")?;
        let rows = statement.query_map(params![subject_type, subject_id], |row| {
            let community_config_id: Option<String> = row.get(4)?;
            let operation_id: Option<String> = row.get(5)?;
            let invalid = |error: StorageError| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            };
            Ok(AuditRecord {
                sequence: row.get(0)?,
                occurred_at: row.get(1)?,
                actor_principal: row.get(2)?,
                authentication_method: row.get(3)?,
                community_config_id: community_config_id
                    .map(|value| parse_id(&value))
                    .transpose()
                    .map_err(&invalid)?,
                operation_id: operation_id
                    .map(|value| parse_id(&value))
                    .transpose()
                    .map_err(&invalid)?,
                correlation_id: row.get(6)?,
                idempotency_key: row.get(7)?,
                action: row.get(8)?,
                subject_type: row.get(9)?,
                subject_id: row.get(10)?,
                outcome: row.get(11)?,
                detail: row.get(12)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn claim_idempotency(
        &self,
        record: &IdempotencyRecord,
    ) -> Result<OperationId, StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT request_hash, operation_id FROM idempotency_keys WHERE scope=?1 AND key=?2",
                params![record.scope, record.key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((hash, operation_id)) = existing {
            if hash != record.request_hash {
                return Err(StorageError::Conflict(
                    "idempotency key was reused with a different request".into(),
                ));
            }
            return parse_id(&operation_id);
        }
        transaction.execute("INSERT INTO idempotency_keys(scope, key, request_hash, operation_id, created_at) VALUES(?1, ?2, ?3, ?4, ?5)", params![record.scope, record.key, record.request_hash, record.operation_id.to_string(), record.created_at]).map_err(map_constraint)?;
        transaction.commit()?;
        Ok(record.operation_id)
    }
}

impl CommunityRepository for SqliteStore {
    fn put_community(&self, config: &CommunityConfig, now: i64) -> Result<(), StorageError> {
        SqliteStore::put_community(self, config, now)
    }

    fn get_community(
        &self,
        id: CommunityConfigId,
    ) -> Result<Option<CommunityConfig>, StorageError> {
        SqliteStore::get_community(self, id)
    }
}

impl AgentRepository for SqliteStore {
    fn put_agent(&self, spec: &AgentSpec, now: i64) -> Result<(), StorageError> {
        SqliteStore::put_agent(self, spec, now)
    }

    fn get_agent(&self, id: AgentId) -> Result<Option<AgentSpec>, StorageError> {
        SqliteStore::get_agent(self, id)
    }
}

impl OperationRepository for SqliteStore {
    fn create_operation(&self, operation: &DurableOperation) -> Result<(), StorageError> {
        SqliteStore::create_operation(self, operation)
    }

    fn get_operation(&self, id: OperationId) -> Result<Option<DurableOperation>, StorageError> {
        SqliteStore::get_operation(self, id)
    }

    fn transition_operation(
        &self,
        id: OperationId,
        next: OperationStatus,
        error_code: Option<ErrorCode>,
        now: i64,
    ) -> Result<(), StorageError> {
        SqliteStore::transition_operation(self, id, next, error_code, now)
    }
}

impl AuditRepository for SqliteStore {
    fn append_audit(&self, record: NewAuditRecord<'_>) -> Result<i64, StorageError> {
        SqliteStore::append_audit(self, record)
    }

    fn audit_for_subject(
        &self,
        subject_type: &str,
        subject_id: &str,
    ) -> Result<Vec<AuditRecord>, StorageError> {
        SqliteStore::audit_for_subject(self, subject_type, subject_id)
    }
}

impl IdempotencyRepository for SqliteStore {
    fn claim_idempotency(&self, record: &IdempotencyRecord) -> Result<OperationId, StorageError> {
        SqliteStore::claim_idempotency(self, record)
    }
}

fn run_migrations(connection: &mut Connection) -> Result<(), StorageError> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")?;
    for &(version, sql) in MIGRATIONS {
        let applied: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=?1)",
            [version],
            |r| r.get(0),
        )?;
        if !applied {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(sql)?;
            transaction.execute(
                "INSERT INTO schema_migrations(version) VALUES(?1)",
                [version],
            )?;
            transaction.commit()?;
        }
    }
    Ok(())
}

fn map_constraint(error: rusqlite::Error) -> StorageError {
    match error {
        rusqlite::Error::SqliteFailure(ref inner, _)
            if inner.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
                || inner.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                || inner.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
        {
            StorageError::Conflict("constraint violation".into())
        }
        other => StorageError::Database(other),
    }
}

fn json_name<T: Serialize>(value: T) -> Result<String, StorageError> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| StorageError::InvalidData("expected string enum".into()))
}

fn parse_json_name<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, StorageError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned())).map_err(StorageError::from)
}

fn parse_id<T: FromStr>(value: &str) -> Result<T, StorageError>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|e: T::Err| StorageError::InvalidData(e.to_string()))
}

fn decode_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<DurableOperation> {
    let id: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let status: String = row.get(2)?;
    let agent_id: Option<String> = row.get(3)?;
    let error_code: Option<String> = row.get(4)?;
    let invalid = |e: StorageError| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    };
    Ok(DurableOperation {
        id: parse_id(&id).map_err(&invalid)?,
        kind: parse_json_name(&kind).map_err(&invalid)?,
        status: parse_json_name(&status).map_err(&invalid)?,
        agent_id: agent_id
            .map(|v| parse_id(&v))
            .transpose()
            .map_err(&invalid)?,
        error_code: error_code
            .map(|v| parse_json_name(&v))
            .transpose()
            .map_err(&invalid)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn contains_secret_marker(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    [
        "authorization",
        "private_key",
        "private-key",
        "secret=",
        "token=",
        "password=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;
    use url::Url;

    use super::*;
    use crate::{DesiredAgentState, RuntimeSpec};

    fn community() -> CommunityConfig {
        CommunityConfig::new(
            "Engineering",
            Url::parse("wss://relay.example.test").unwrap(),
        )
        .unwrap()
    }

    fn agent(community_config_id: CommunityConfigId) -> AgentSpec {
        AgentSpec {
            id: AgentId::new(),
            community_config_id,
            display_name: "Builder".into(),
            system_prompt: "Build and test changes.".into(),
            runtime: RuntimeSpec {
                runtime_id: "codex-acp".parse().unwrap(),
                environment: BTreeMap::new(),
            },
            desired_state: DesiredAgentState::Enabled,
        }
    }

    #[test]
    fn state_survives_restart_and_migrations_are_idempotent() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("buzz.sqlite3");
        let config = community();
        let spec = agent(config.id);
        {
            let store = SqliteStore::open(&path).unwrap();
            store.put_community(&config, 10).unwrap();
            store.put_agent(&spec, 11).unwrap();
        }
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.get_community(config.id).unwrap(), Some(config));
        assert_eq!(store.get_agent(spec.id).unwrap(), Some(spec));
        let connection = store.connection().unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(journal_mode, "wal");
        assert_eq!(foreign_keys, 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn operation_transitions_and_idempotency_are_durable() {
        let store = SqliteStore::open_in_memory().unwrap();
        let operation = DurableOperation {
            id: OperationId::new(),
            kind: OperationKind::CreateAgent,
            status: OperationStatus::Pending,
            agent_id: None,
            error_code: None,
            created_at: 1,
            updated_at: 1,
        };
        store.create_operation(&operation).unwrap();
        store
            .transition_operation(operation.id, OperationStatus::Running, None, 2)
            .unwrap();
        assert_eq!(
            store.get_operation(operation.id).unwrap().unwrap().status,
            OperationStatus::Running
        );

        let record = IdempotencyRecord {
            scope: "create-agent".into(),
            key: "request-1".into(),
            request_hash: "sha256:abc".into(),
            operation_id: operation.id,
            created_at: 1,
        };
        assert_eq!(store.claim_idempotency(&record).unwrap(), operation.id);
        assert_eq!(store.claim_idempotency(&record).unwrap(), operation.id);
        let conflicting = IdempotencyRecord {
            request_hash: "sha256:different".into(),
            ..record
        };
        assert!(matches!(
            store.claim_idempotency(&conflicting),
            Err(StorageError::Conflict(_))
        ));
    }

    #[test]
    fn audit_rejects_credential_like_details() {
        let store = SqliteStore::open_in_memory().unwrap();
        let config = community();
        let spec = agent(config.id);
        let operation_id = OperationId::new();
        store.put_community(&config, 1).unwrap();
        store.put_agent(&spec, 1).unwrap();
        let subject_id = spec.id.to_string();
        let bad = NewAuditRecord {
            occurred_at: 1,
            actor_principal: "owner:npub1example",
            authentication_method: "nip98",
            community_config_id: Some(config.id),
            operation_id: Some(operation_id),
            correlation_id: "correlation-1",
            idempotency_key: Some("request-1"),
            action: "agent.create",
            subject_type: "agent",
            subject_id: Some(&subject_id),
            outcome: "denied",
            redacted_detail: Some("authorization: bearer value"),
        };
        assert!(matches!(
            store.append_audit(bad.clone()),
            Err(StorageError::InvalidData(_))
        ));
        let good = NewAuditRecord {
            redacted_detail: Some("request payload redacted"),
            ..bad
        };
        let sequence = store.append_audit(good).unwrap();
        assert_eq!(sequence, 1);

        let connection = store.connection().unwrap();
        connection
            .execute("DELETE FROM agent_specs WHERE id=?1", [&subject_id])
            .unwrap();
        connection
            .execute(
                "DELETE FROM community_configs WHERE id=?1",
                [config.id.to_string()],
            )
            .unwrap();
        drop(connection);

        let records = store.audit_for_subject("agent", &subject_id).unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.actor_principal, "owner:npub1example");
        assert_eq!(record.authentication_method, "nip98");
        assert_eq!(record.community_config_id, Some(config.id));
        assert_eq!(record.operation_id, Some(operation_id));
        assert_eq!(record.correlation_id, "correlation-1");
        assert_eq!(record.idempotency_key.as_deref(), Some("request-1"));
        assert_eq!(record.detail.as_deref(), Some("request payload redacted"));
    }

    #[test]
    fn repository_interfaces_support_storage_agnostic_consumers() {
        fn save_and_load_community(
            repository: &impl CommunityRepository,
            config: &CommunityConfig,
        ) -> CommunityConfig {
            repository.put_community(config, 1).unwrap();
            repository.get_community(config.id).unwrap().unwrap()
        }

        fn assert_repository_set<T>()
        where
            T: CommunityRepository
                + AgentRepository
                + OperationRepository
                + AuditRepository
                + IdempotencyRepository,
        {
        }

        assert_repository_set::<SqliteStore>();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = community();
        assert_eq!(save_and_load_community(&store, &config), config);
    }
}
