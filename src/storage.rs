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

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_initial.sql")),
    (2, include_str!("../migrations/0002_lifecycle.sql")),
    (3, include_str!("../migrations/0003_retention.sql")),
    (4, include_str!("../migrations/0004_relay_publications.sql")),
    (5, include_str!("../migrations/0005_auto_join.sql")),
];

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
    pub correlation_id: String,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayProjectionKind {
    ManagedAgent,
    Persona,
}

impl RelayProjectionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManagedAgent => "managed_agent",
            Self::Persona => "persona",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayPublicationAction {
    SyncManagedAgent,
    SyncPersona,
    TombstoneManagedAgent,
    ArchiveIdentity,
    TombstonePersona,
}

impl RelayPublicationAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SyncManagedAgent => "sync_managed_agent",
            Self::SyncPersona => "sync_persona",
            Self::TombstoneManagedAgent => "tombstone_managed_agent",
            Self::ArchiveIdentity => "archive_identity",
            Self::TombstonePersona => "tombstone_persona",
        }
    }

    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "sync_managed_agent" => Ok(Self::SyncManagedAgent),
            "sync_persona" => Ok(Self::SyncPersona),
            "tombstone_managed_agent" => Ok(Self::TombstoneManagedAgent),
            "archive_identity" => Ok(Self::ArchiveIdentity),
            "tombstone_persona" => Ok(Self::TombstonePersona),
            other => Err(StorageError::InvalidData(format!(
                "unknown relay publication action {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayProjectionScope {
    pub community_config_id: CommunityConfigId,
    pub relay_url: String,
    pub owner_pubkey: String,
    pub d_tag: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayPublication {
    pub id: String,
    pub action: RelayPublicationAction,
    pub community_config_id: Option<CommunityConfigId>,
    pub relay_url: String,
    pub owner_pubkey: String,
    pub subject_id: String,
    pub d_tag: String,
    pub attempts: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyRecord {
    pub scope: String,
    pub key: String,
    pub request_hash: String,
    pub operation_id: OperationId,
    pub created_at: i64,
}

pub enum AgentCommandMutation<'a> {
    Create(&'a AgentSpec),
    Update {
        id: AgentId,
        changes: &'a crate::api::UpdateAgentInput,
    },
    SetState {
        id: AgentId,
        desired_state: crate::DesiredAgentState,
        retention_deadline: Option<i64>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentRetention {
    pub deleted_at: i64,
    pub purge_after: i64,
}

/// Persistence boundary for community configuration state.
pub trait CommunityRepository {
    fn put_community(&self, config: &CommunityConfig, now: i64) -> Result<(), StorageError>;
    fn get_community(&self, id: CommunityConfigId)
        -> Result<Option<CommunityConfig>, StorageError>;
    fn list_communities(&self) -> Result<Vec<CommunityConfig>, StorageError>;
    fn delete_community(&self, id: CommunityConfigId) -> Result<(), StorageError>;
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

    /// Returns the durable boundary for a new-channels-only policy, creating it
    /// atomically the first time that policy is observed.
    pub fn ensure_auto_join_enabled_at(
        &self,
        agent_id: AgentId,
        now: i64,
    ) -> Result<i64, StorageError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT OR IGNORE INTO auto_join_state(agent_id, enabled_at) VALUES(?1, ?2)",
            params![agent_id.to_string(), now],
        )?;
        connection
            .query_row(
                "SELECT enabled_at FROM auto_join_state WHERE agent_id=?1",
                [agent_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StorageError::from)
    }

    pub fn clear_auto_join_enabled_at(&self, agent_id: AgentId) -> Result<(), StorageError> {
        self.connection()?.execute(
            "DELETE FROM auto_join_state WHERE agent_id=?1",
            [agent_id.to_string()],
        )?;
        Ok(())
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

    pub fn list_communities(&self) -> Result<Vec<CommunityConfig>, StorageError> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT document FROM community_configs ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            let document = row?;
            serde_json::from_str(&document).map_err(StorageError::from)
        })
        .collect()
    }

    pub fn delete_community(&self, id: CommunityConfigId) -> Result<(), StorageError> {
        let changed = self
            .connection()?
            .execute(
                "DELETE FROM community_configs WHERE id=?1",
                [id.to_string()],
            )
            .map_err(map_constraint)?;
        if changed == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub fn delete_community_with_deleted_agents(
        &self,
        id: CommunityConfigId,
        now: i64,
    ) -> Result<Vec<AgentId>, StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = transaction
            .query_row(
                "SELECT 1 FROM community_configs WHERE id=?1",
                [id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(StorageError::NotFound);
        }

        let agents = {
            let mut statement = transaction.prepare(
                "SELECT id, document FROM agent_specs WHERE community_config_id=?1 ORDER BY id",
            )?;
            let rows = statement.query_map([id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut deleted_agents = Vec::with_capacity(agents.len());
        for (agent_id, document) in agents {
            let agent_id: AgentId = parse_id(&agent_id)?;
            let agent: AgentSpec = serde_json::from_str(&document)?;
            if agent.desired_state != crate::DesiredAgentState::Deleted {
                return Err(StorageError::Conflict(format!(
                    "community {id} cannot be deleted while it still has active agents; delete the remaining agents first"
                )));
            }
            let completed_delete: Option<String> = transaction
                .query_row(
                    "SELECT status FROM operations WHERE agent_id=?1 AND kind IN ('delete_agent','purge_agent') ORDER BY created_at DESC, id DESC LIMIT 1",
                    [agent_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            if completed_delete.as_deref() != Some("succeeded") {
                return Err(StorageError::Conflict(format!(
                    "deleted agent {agent_id} has not completed shutdown; retry its delete first"
                )));
            }
            deleted_agents.push(agent_id);
        }

        for agent_id in &deleted_agents {
            if let Some((relay_url, owner_pubkey, d_tag)) = transaction
                .query_row(
                    "SELECT relay_url, owner_pubkey, d_tag FROM relay_projection_state WHERE community_config_id=?1 AND projection_kind='managed_agent' AND subject_id=?2",
                    params![id.to_string(), agent_id.to_string()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
                )
                .optional()?
            {
                for action in [
                    RelayPublicationAction::TombstoneManagedAgent,
                    RelayPublicationAction::ArchiveIdentity,
                ] {
                    transaction.execute(
                        "INSERT INTO relay_publication_outbox(id, action, community_config_id, relay_url, owner_pubkey, subject_id, d_tag, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) ON CONFLICT(action, relay_url, owner_pubkey, subject_id, d_tag) DO NOTHING",
                        params![uuid::Uuid::now_v7().to_string(), action.as_str(), id.to_string(), &relay_url, &owner_pubkey, agent_id.to_string(), &d_tag, now],
                    )?;
                }
            }
            let purge_operation = OperationId::new();
            transaction.execute(
                "INSERT INTO operations(id, kind, status, agent_id, error_code, created_at, updated_at, correlation_id) VALUES(?1, 'purge_agent', 'succeeded', ?2, NULL, ?3, ?3, ?4)",
                params![
                    purge_operation.to_string(),
                    agent_id.to_string(),
                    now,
                    format!("community-delete:{id}")
                ],
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO purged_agent_tombstones(agent_id, purged_at, purge_operation_id) VALUES(?1, ?2, ?3)",
                params![agent_id.to_string(), now, purge_operation.to_string()],
            )?;
            transaction.execute(
                "DELETE FROM agent_specs WHERE id=?1",
                [agent_id.to_string()],
            )?;
        }

        {
            let mut statement = transaction.prepare(
                "SELECT subject_id, relay_url, owner_pubkey, d_tag FROM relay_projection_state WHERE community_config_id=?1 AND projection_kind='persona'",
            )?;
            let rows = statement.query_map([id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (persona_id, relay_url, owner_pubkey, d_tag) = row?;
                transaction.execute(
                    "INSERT INTO relay_publication_outbox(id, action, community_config_id, relay_url, owner_pubkey, subject_id, d_tag, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) ON CONFLICT(action, relay_url, owner_pubkey, subject_id, d_tag) DO NOTHING",
                    params![uuid::Uuid::now_v7().to_string(), RelayPublicationAction::TombstonePersona.as_str(), id.to_string(), relay_url, owner_pubkey, persona_id, d_tag, now],
                )?;
            }
        }

        transaction.execute(
            "DELETE FROM community_configs WHERE id=?1",
            [id.to_string()],
        )?;
        transaction.commit()?;
        Ok(deleted_agents)
    }

    pub fn list_purged_agent_ids(&self) -> Result<Vec<AgentId>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT agent_id FROM purged_agent_tombstones ORDER BY purged_at, agent_id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|value| parse_id(&value))
            .collect()
    }

    pub fn record_relay_projection(
        &self,
        kind: RelayProjectionKind,
        subject_id: &str,
        scope: &RelayProjectionScope,
        now: i64,
    ) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT INTO relay_projection_state(community_config_id, projection_kind, subject_id, relay_url, owner_pubkey, d_tag, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(community_config_id, projection_kind, subject_id) DO UPDATE SET relay_url=excluded.relay_url, owner_pubkey=excluded.owner_pubkey, d_tag=excluded.d_tag, updated_at=excluded.updated_at",
            params![scope.community_config_id.to_string(), kind.as_str(), subject_id, scope.relay_url, scope.owner_pubkey, scope.d_tag, now],
        )?;
        Ok(())
    }

    pub fn relay_projection_scopes(
        &self,
        kind: RelayProjectionKind,
        subject_id: &str,
    ) -> Result<Vec<RelayProjectionScope>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT community_config_id, relay_url, owner_pubkey, d_tag FROM relay_projection_state WHERE projection_kind=?1 AND subject_id=?2 ORDER BY community_config_id",
        )?;
        let rows = statement.query_map(params![kind.as_str(), subject_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (community, relay_url, owner_pubkey, d_tag) = row?;
            Ok(RelayProjectionScope {
                community_config_id: parse_id(&community)?,
                relay_url,
                owner_pubkey,
                d_tag,
            })
        })
        .collect()
    }

    pub fn enqueue_relay_publication(
        &self,
        action: RelayPublicationAction,
        scope: &RelayProjectionScope,
        subject_id: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT INTO relay_publication_outbox(id, action, community_config_id, relay_url, owner_pubkey, subject_id, d_tag, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) ON CONFLICT(action, relay_url, owner_pubkey, subject_id, d_tag) DO NOTHING",
            params![uuid::Uuid::now_v7().to_string(), action.as_str(), scope.community_config_id.to_string(), scope.relay_url, scope.owner_pubkey, subject_id, scope.d_tag, now],
        )?;
        Ok(())
    }

    pub fn pending_relay_publications(&self) -> Result<Vec<RelayPublication>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, action, community_config_id, relay_url, owner_pubkey, subject_id, d_tag, attempts FROM relay_publication_outbox ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?;
        rows.map(|row| {
            let (id, action, community, relay_url, owner_pubkey, subject_id, d_tag, attempts) =
                row?;
            Ok(RelayPublication {
                id,
                action: RelayPublicationAction::parse(&action)?,
                community_config_id: community.as_deref().map(parse_id).transpose()?,
                relay_url,
                owner_pubkey,
                subject_id,
                d_tag,
                attempts,
            })
        })
        .collect()
    }

    pub fn complete_relay_publication(&self, id: &str) -> Result<(), StorageError> {
        self.connection()?
            .execute("DELETE FROM relay_publication_outbox WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn fail_relay_publication(
        &self,
        id: &str,
        error: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        self.connection()?.execute(
            "UPDATE relay_publication_outbox SET attempts=attempts+1, last_error=?2, updated_at=?3 WHERE id=?1",
            params![id, error, now],
        )?;
        Ok(())
    }

    pub fn has_pending_relay_publications_for_owner(
        &self,
        owner_pubkey: &str,
    ) -> Result<bool, StorageError> {
        self.connection()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM relay_publication_outbox WHERE owner_pubkey=?1)",
                [owner_pubkey],
                |row| row.get(0),
            )
            .map_err(StorageError::from)
    }

    pub fn remove_relay_projection(
        &self,
        community_config_id: CommunityConfigId,
        kind: RelayProjectionKind,
        subject_id: &str,
    ) -> Result<(), StorageError> {
        self.connection()?.execute(
            "DELETE FROM relay_projection_state WHERE community_config_id=?1 AND projection_kind=?2 AND subject_id=?3",
            params![community_config_id.to_string(), kind.as_str(), subject_id],
        )?;
        Ok(())
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

    pub fn agent_retention(&self, id: AgentId) -> Result<Option<AgentRetention>, StorageError> {
        self.connection()?
            .query_row(
                "SELECT deleted_at, purge_after FROM agent_retention WHERE agent_id=?1",
                [id.to_string()],
                |row| {
                    Ok(AgentRetention {
                        deleted_at: row.get(0)?,
                        purge_after: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn expired_retained_agents(&self, now: i64) -> Result<Vec<AgentId>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT agent_id FROM agent_retention WHERE purge_after <= ?1 ORDER BY purge_after, agent_id",
        )?;
        let rows = statement.query_map([now], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|value| parse_id(&value))
            .collect()
    }

    pub fn is_agent_purged(&self, id: AgentId) -> Result<bool, StorageError> {
        self.connection()?
            .query_row(
                "SELECT 1 FROM purged_agent_tombstones WHERE agent_id=?1",
                [id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(StorageError::from)
    }

    pub fn list_agents(
        &self,
        community_config_id: Option<CommunityConfigId>,
    ) -> Result<Vec<AgentSpec>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT document FROM agent_specs WHERE (?1 IS NULL OR community_config_id=?1) ORDER BY id",
        )?;
        let community = community_config_id.map(|id| id.to_string());
        let rows = statement.query_map([community], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            let document = row?;
            serde_json::from_str(&document).map_err(StorageError::from)
        })
        .collect()
    }

    pub fn purge_agent(&self, id: AgentId) -> Result<(), StorageError> {
        let changed = self
            .connection()?
            .execute("DELETE FROM agent_specs WHERE id=?1", [id.to_string()])?;
        if changed == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    pub fn put_draft(
        &self,
        draft: &crate::api::DraftResource,
        now: i64,
    ) -> Result<(), StorageError> {
        let document = serde_json::to_string(draft)?;
        self.connection()?.execute(
            "INSERT INTO agent_drafts(id, owner_key, document, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?4) ON CONFLICT(id) DO UPDATE SET owner_key=excluded.owner_key, document=excluded.document, updated_at=excluded.updated_at",
            params![draft.id, serde_json::to_string(&draft.owner)?, document, now],
        )?;
        Ok(())
    }

    pub fn get_draft(&self, id: &str) -> Result<Option<crate::api::DraftResource>, StorageError> {
        let document: Option<String> = self
            .connection()?
            .query_row(
                "SELECT document FROM agent_drafts WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        document
            .map(|value| serde_json::from_str(&value).map_err(StorageError::from))
            .transpose()
    }

    pub fn delete_draft(&self, id: &str) -> Result<(), StorageError> {
        self.connection()?
            .execute("DELETE FROM agent_drafts WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn append_redacted_log(
        &self,
        agent_id: AgentId,
        entry: &crate::api::RedactedLogEntry,
    ) -> Result<(), StorageError> {
        if contains_secret_marker(&entry.redacted_message) {
            return Err(StorageError::InvalidData(
                "log entry contains a credential-like marker".into(),
            ));
        }
        self.connection()?.execute(
            "INSERT OR IGNORE INTO agent_logs(agent_id, cursor, occurred_at, stream, redacted_message) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![agent_id.to_string(), entry.cursor, entry.occurred_at, entry.stream, entry.redacted_message],
        ).map_err(map_constraint)?;
        Ok(())
    }

    pub fn agent_logs(
        &self,
        agent_id: AgentId,
        after_cursor: Option<&str>,
        limit: u16,
    ) -> Result<Vec<crate::api::RedactedLogEntry>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT cursor, occurred_at, stream, redacted_message FROM agent_logs WHERE agent_id=?1 AND (?2 IS NULL OR sequence > COALESCE((SELECT sequence FROM agent_logs WHERE agent_id=?1 AND cursor=?2), 0)) ORDER BY sequence LIMIT ?3")?;
        let rows =
            statement.query_map(params![agent_id.to_string(), after_cursor, limit], |row| {
                Ok(crate::api::RedactedLogEntry {
                    cursor: row.get(0)?,
                    occurred_at: row.get(1)?,
                    stream: row.get(2)?,
                    redacted_message: row.get(3)?,
                })
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn claim_nip98_replay(
        &self,
        event_id: &str,
        expires_at: u64,
        now: u64,
    ) -> Result<bool, StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM nip98_replay WHERE expires_at < ?1", [now])?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO nip98_replay(event_id, expires_at) VALUES(?1, ?2)",
            params![event_id, expires_at],
        )?;
        transaction.commit()?;
        Ok(inserted == 1)
    }

    pub fn create_operation(&self, operation: &DurableOperation) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT INTO operations(id, kind, status, agent_id, error_code, created_at, updated_at, correlation_id) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![operation.id.to_string(), json_name(operation.kind)?, json_name(operation.status)?, operation.agent_id.map(|id| id.to_string()), operation.error_code.map(json_name).transpose()?, operation.created_at, operation.updated_at, operation.correlation_id],
        ).map_err(map_constraint)?;
        Ok(())
    }

    pub fn delete_operation(&self, id: OperationId) -> Result<(), StorageError> {
        self.connection()?
            .execute("DELETE FROM operations WHERE id=?1", [id.to_string()])?;
        Ok(())
    }

    pub fn get_operation(&self, id: OperationId) -> Result<Option<DurableOperation>, StorageError> {
        self.connection()?.query_row(
            "SELECT id, kind, status, agent_id, error_code, created_at, updated_at, correlation_id FROM operations WHERE id=?1",
            [id.to_string()], decode_operation,
        ).optional().map_err(StorageError::from)
    }

    pub fn nonterminal_operations(&self) -> Result<Vec<DurableOperation>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, kind, status, agent_id, error_code, created_at, updated_at, correlation_id FROM operations WHERE status IN ('pending', 'running') ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([], decode_operation)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
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
                    .map_err(invalid)?,
                operation_id: operation_id
                    .map(|value| parse_id(&value))
                    .transpose()
                    .map_err(invalid)?,
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

    /// Atomically commits agent intent, its operation, scoped idempotency, and audit record.
    pub fn apply_agent_command(
        &self,
        operation: &DurableOperation,
        idempotency: &IdempotencyRecord,
        mutation: AgentCommandMutation<'_>,
        promoted_draft_id: Option<&str>,
        audit: NewAuditRecord<'_>,
    ) -> Result<(DurableOperation, bool), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT request_hash, operation_id FROM idempotency_keys WHERE scope=?1 AND key=?2",
                params![idempotency.scope, idempotency.key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((hash, operation_id)) = existing {
            if hash != idempotency.request_hash {
                return Err(StorageError::Conflict(
                    "idempotency key was reused with a different request".into(),
                ));
            }
            let existing = transaction.query_row(
                "SELECT id, kind, status, agent_id, error_code, created_at, updated_at, correlation_id FROM operations WHERE id=?1",
                [operation_id],
                decode_operation,
            )?;
            return Ok((existing, true));
        }
        match mutation {
            AgentCommandMutation::Create(agent) => {
                if operation.kind != OperationKind::CreateAgent
                    || operation.agent_id != Some(agent.id)
                {
                    return Err(StorageError::InvalidData(
                        "create mutation does not match operation".into(),
                    ));
                }
                agent
                    .validate()
                    .map_err(|error| StorageError::InvalidData(error.to_string()))?;
                transaction.execute(
                    "INSERT INTO agent_specs(id, community_config_id, document, updated_at) VALUES(?1, ?2, ?3, ?4)",
                    params![agent.id.to_string(), agent.community_config_id.to_string(), serde_json::to_string(agent)?, operation.created_at],
                ).map_err(map_constraint)?;
            }
            AgentCommandMutation::Update { id, changes } => {
                if operation.kind != OperationKind::UpdateAgent || operation.agent_id != Some(id) {
                    return Err(StorageError::InvalidData(
                        "update mutation does not match operation".into(),
                    ));
                }
                let mut agent = load_agent_transaction(&transaction, id)?;
                if let Some(value) = &changes.display_name {
                    agent.display_name.clone_from(value);
                }
                if let Some(value) = &changes.system_prompt {
                    agent.system_prompt.clone_from(value);
                }
                if let Some(value) = &changes.runtime_id {
                    agent.runtime.runtime_id.clone_from(value);
                }
                agent
                    .validate()
                    .map_err(|error| StorageError::InvalidData(error.to_string()))?;
                update_agent_transaction(&transaction, &agent, operation.created_at)?;
            }
            AgentCommandMutation::SetState {
                id,
                desired_state,
                retention_deadline,
            } => {
                let expected = match operation.kind {
                    OperationKind::EnableAgent => crate::DesiredAgentState::Enabled,
                    OperationKind::DisableAgent => crate::DesiredAgentState::Disabled,
                    OperationKind::DeleteAgent | OperationKind::PurgeAgent => {
                        crate::DesiredAgentState::Deleted
                    }
                    _ => {
                        return Err(StorageError::InvalidData(
                            "state mutation does not match operation".into(),
                        ))
                    }
                };
                if operation.agent_id != Some(id) || desired_state != expected {
                    return Err(StorageError::InvalidData(
                        "state mutation does not match operation".into(),
                    ));
                }
                let mut agent = load_agent_transaction(&transaction, id)?;
                agent.desired_state = desired_state;
                update_agent_transaction(&transaction, &agent, operation.created_at)?;
                if operation.kind == OperationKind::DeleteAgent {
                    let purge_after = retention_deadline.ok_or_else(|| {
                        StorageError::InvalidData(
                            "recoverable delete requires a retention deadline".into(),
                        )
                    })?;
                    transaction.execute(
                        "INSERT INTO agent_retention(agent_id, deleted_at, purge_after) VALUES(?1, ?2, ?3) ON CONFLICT(agent_id) DO UPDATE SET deleted_at=excluded.deleted_at, purge_after=excluded.purge_after",
                        params![id.to_string(), operation.created_at, purge_after],
                    )?;
                } else if operation.kind == OperationKind::EnableAgent {
                    transaction.execute(
                        "DELETE FROM agent_retention WHERE agent_id=?1",
                        [id.to_string()],
                    )?;
                }
            }
        }
        transaction.execute(
            "INSERT INTO operations(id, kind, status, agent_id, error_code, created_at, updated_at, correlation_id) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![operation.id.to_string(), json_name(operation.kind)?, json_name(operation.status)?, operation.agent_id.map(|id| id.to_string()), operation.error_code.map(json_name).transpose()?, operation.created_at, operation.updated_at, operation.correlation_id],
        ).map_err(map_constraint)?;
        transaction.execute(
            "INSERT INTO idempotency_keys(scope, key, request_hash, operation_id, created_at) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![idempotency.scope, idempotency.key, idempotency.request_hash, operation.id.to_string(), idempotency.created_at],
        ).map_err(map_constraint)?;
        if let Some(draft_id) = promoted_draft_id {
            let changed = transaction.execute(
                "UPDATE agent_drafts SET promoted_operation_id=?2, updated_at=?3 WHERE id=?1 AND promoted_operation_id IS NULL",
                params![draft_id, operation.id.to_string(), operation.created_at],
            )?;
            if changed != 1 {
                return Err(StorageError::Conflict(
                    "draft does not exist or was already promoted".into(),
                ));
            }
        }
        insert_audit(&transaction, audit)?;
        transaction.commit()?;
        Ok((operation.clone(), false))
    }

    pub fn apply_draft_command(
        &self,
        draft: &crate::api::DraftResource,
        principal_scope: &str,
        key: &str,
        request_hash: &str,
        now: i64,
        audit: NewAuditRecord<'_>,
    ) -> Result<(crate::api::DraftResource, bool), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT request_hash, draft_id FROM draft_idempotency WHERE principal_scope=?1 AND key=?2",
                params![principal_scope, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((hash, draft_id)) = existing {
            if hash != request_hash {
                return Err(StorageError::Conflict(
                    "idempotency key was reused with a different request".into(),
                ));
            }
            let document: String = transaction.query_row(
                "SELECT document FROM agent_drafts WHERE id=?1",
                [draft_id],
                |row| row.get(0),
            )?;
            return Ok((serde_json::from_str(&document)?, true));
        }
        transaction.execute(
            "INSERT INTO agent_drafts(id, owner_key, document, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?4)",
            params![draft.id, serde_json::to_string(&draft.owner)?, serde_json::to_string(draft)?, now],
        )?;
        transaction.execute(
            "INSERT INTO draft_idempotency(principal_scope, key, request_hash, draft_id, created_at) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![principal_scope, key, request_hash, draft.id, now],
        )?;
        insert_audit(&transaction, audit)?;
        transaction.commit()?;
        Ok((draft.clone(), false))
    }

    /// Atomically records successful purge completion, its audit, the tombstone, and deletion.
    pub fn complete_purge(
        &self,
        agent_id: AgentId,
        operation_id: OperationId,
        now: i64,
        audit: NewAuditRecord<'_>,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let operation: Option<String> = transaction
            .query_row(
                "SELECT status, updated_at FROM operations WHERE id=?1 AND agent_id=?2 AND kind='purge_agent'",
                params![operation_id.to_string(), agent_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let status = operation.ok_or(StorageError::NotFound)?;
        let status: OperationStatus = parse_json_name(&status)?;
        if !status.can_transition_to(OperationStatus::Succeeded) {
            return Err(StorageError::Conflict(format!(
                "invalid purge transition from {status:?} to Succeeded"
            )));
        }
        let document: Option<String> = transaction
            .query_row(
                "SELECT document FROM agent_specs WHERE id=?1",
                [agent_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let document = document.ok_or(StorageError::NotFound)?;
        let agent: AgentSpec = serde_json::from_str(&document)?;
        if agent.desired_state != crate::DesiredAgentState::Deleted {
            return Err(StorageError::Conflict(
                "purge target is not recoverably deleted".into(),
            ));
        }
        transaction.execute(
            "UPDATE operations SET status='succeeded', error_code=NULL, updated_at=?2 WHERE id=?1",
            params![operation_id.to_string(), now],
        )?;
        insert_audit(&transaction, audit)?;
        transaction.execute(
            "INSERT OR IGNORE INTO purged_agent_tombstones(agent_id, purged_at, purge_operation_id) VALUES(?1, ?2, ?3)",
            params![agent_id.to_string(), now, operation_id.to_string()],
        )?;
        transaction.execute(
            "DELETE FROM agent_specs WHERE id=?1",
            [agent_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
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

    fn list_communities(&self) -> Result<Vec<CommunityConfig>, StorageError> {
        SqliteStore::list_communities(self)
    }

    fn delete_community(&self, id: CommunityConfigId) -> Result<(), StorageError> {
        SqliteStore::delete_community(self, id)
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

fn load_agent_transaction(
    transaction: &rusqlite::Transaction<'_>,
    id: AgentId,
) -> Result<AgentSpec, StorageError> {
    let document: Option<String> = transaction
        .query_row(
            "SELECT document FROM agent_specs WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    document
        .ok_or(StorageError::NotFound)
        .and_then(|value| serde_json::from_str(&value).map_err(StorageError::from))
}

fn update_agent_transaction(
    transaction: &rusqlite::Transaction<'_>,
    agent: &AgentSpec,
    now: i64,
) -> Result<(), StorageError> {
    let changed = transaction.execute(
        "UPDATE agent_specs SET community_config_id=?2, document=?3, updated_at=?4 WHERE id=?1",
        params![
            agent.id.to_string(),
            agent.community_config_id.to_string(),
            serde_json::to_string(agent)?,
            now
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::NotFound);
    }
    Ok(())
}

fn insert_audit(
    transaction: &rusqlite::Transaction<'_>,
    record: NewAuditRecord<'_>,
) -> Result<(), StorageError> {
    if record.redacted_detail.is_some_and(contains_secret_marker) {
        return Err(StorageError::InvalidData(
            "audit detail contains a credential-like marker".into(),
        ));
    }
    transaction.execute(
        "INSERT INTO audit_records(occurred_at, actor_principal, authentication_method, community_config_id, operation_id, correlation_id, idempotency_key, action, subject_type, subject_id, outcome, detail) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![record.occurred_at, record.actor_principal, record.authentication_method, record.community_config_id.map(|id| id.to_string()), record.operation_id.map(|id| id.to_string()), record.correlation_id, record.idempotency_key, record.action, record.subject_type, record.subject_id, record.outcome, record.redacted_detail],
    )?;
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
        id: parse_id(&id).map_err(invalid)?,
        kind: parse_json_name(&kind).map_err(invalid)?,
        status: parse_json_name(&status).map_err(invalid)?,
        agent_id: agent_id
            .map(|v| parse_id(&v))
            .transpose()
            .map_err(invalid)?,
        error_code: error_code
            .map(|v| parse_json_name(&v))
            .transpose()
            .map_err(invalid)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        correlation_id: row.get(7)?,
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
    fn community_delete_requires_completed_agent_deletes_and_purges_retained_agents() {
        let store = SqliteStore::open_in_memory().unwrap();
        let config = community();
        store.put_community(&config, 1).unwrap();
        let mut spec = agent(config.id);
        store.put_agent(&spec, 1).unwrap();

        assert!(matches!(
            store.delete_community_with_deleted_agents(config.id, 10),
            Err(StorageError::Conflict(message)) if message.contains("active agents")
        ));

        spec.desired_state = DesiredAgentState::Deleted;
        store.put_agent(&spec, 2).unwrap();
        assert!(matches!(
            store.delete_community_with_deleted_agents(config.id, 10),
            Err(StorageError::Conflict(message)) if message.contains("has not completed shutdown")
        ));

        let operation = DurableOperation {
            id: OperationId::new(),
            kind: OperationKind::DeleteAgent,
            status: OperationStatus::Succeeded,
            agent_id: Some(spec.id),
            error_code: None,
            created_at: 3,
            updated_at: 3,
            correlation_id: "delete-complete".into(),
        };
        store.create_operation(&operation).unwrap();
        let removed = store
            .delete_community_with_deleted_agents(config.id, 10)
            .unwrap();
        assert_eq!(removed, vec![spec.id]);
        assert!(store.get_community(config.id).unwrap().is_none());
        assert!(store.get_agent(spec.id).unwrap().is_none());
        assert!(store.is_agent_purged(spec.id).unwrap());
        assert_eq!(store.list_purged_agent_ids().unwrap(), vec![spec.id]);
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
            assert!(store.claim_nip98_replay("event-1", 100, 10).unwrap());
            assert_eq!(store.ensure_auto_join_enabled_at(spec.id, 12).unwrap(), 12);
            assert_eq!(store.ensure_auto_join_enabled_at(spec.id, 99).unwrap(), 12);
        }
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.get_community(config.id).unwrap(), Some(config));
        assert_eq!(store.get_agent(spec.id).unwrap(), Some(spec.clone()));
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
        assert_eq!(count, MIGRATIONS.len() as i64);
        drop(connection);
        assert!(!store.claim_nip98_replay("event-1", 100, 11).unwrap());
        assert_eq!(store.ensure_auto_join_enabled_at(spec.id, 99).unwrap(), 12);
        store.clear_auto_join_enabled_at(spec.id).unwrap();
        assert_eq!(store.ensure_auto_join_enabled_at(spec.id, 99).unwrap(), 99);
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
            correlation_id: "correlation-1".into(),
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

    #[test]
    fn atomic_agent_command_replays_without_duplicate_intent_operation_or_audit() {
        let store = SqliteStore::open_in_memory().unwrap();
        let config = community();
        store.put_community(&config, 1).unwrap();
        let spec = agent(config.id);
        let operation = DurableOperation {
            id: OperationId::new(),
            kind: OperationKind::CreateAgent,
            status: OperationStatus::Pending,
            agent_id: Some(spec.id),
            error_code: None,
            created_at: 2,
            updated_at: 2,
            correlation_id: "correlation-atomic".into(),
        };
        let idempotency = IdempotencyRecord {
            scope: "unix_uid:1000:create_agent".into(),
            key: "request-atomic".into(),
            request_hash: "sha256:request".into(),
            operation_id: operation.id,
            created_at: 2,
        };
        let subject = spec.id.to_string();
        let audit = || NewAuditRecord {
            occurred_at: 2,
            actor_principal: "unix_uid:1000",
            authentication_method: "unix_peer",
            community_config_id: Some(config.id),
            operation_id: Some(operation.id),
            correlation_id: "correlation-atomic",
            idempotency_key: Some("request-atomic"),
            action: "agent.create",
            subject_type: "agent",
            subject_id: Some(&subject),
            outcome: "accepted",
            redacted_detail: None,
        };
        let (created, replayed) = store
            .apply_agent_command(
                &operation,
                &idempotency,
                AgentCommandMutation::Create(&spec),
                None,
                audit(),
            )
            .unwrap();
        assert_eq!(created.correlation_id, "correlation-atomic");
        assert!(!replayed);
        let (_, replayed) = store
            .apply_agent_command(
                &operation,
                &idempotency,
                AgentCommandMutation::Create(&spec),
                None,
                audit(),
            )
            .unwrap();
        assert!(replayed);
        assert_eq!(store.audit_for_subject("agent", &subject).unwrap().len(), 1);
        let connection = store.connection().unwrap();
        let operations: i64 = connection
            .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(operations, 1);
    }

    #[test]
    fn atomic_commands_reject_duplicate_create_and_missing_update() {
        let store = SqliteStore::open_in_memory().unwrap();
        let config = community();
        store.put_community(&config, 1).unwrap();
        let spec = agent(config.id);
        store.put_agent(&spec, 1).unwrap();
        let operation = DurableOperation {
            id: OperationId::new(),
            kind: OperationKind::CreateAgent,
            status: OperationStatus::Pending,
            agent_id: Some(spec.id),
            error_code: None,
            created_at: 2,
            updated_at: 2,
            correlation_id: "duplicate".into(),
        };
        let idempotency = IdempotencyRecord {
            scope: "principal:create".into(),
            key: "duplicate".into(),
            request_hash: "hash".into(),
            operation_id: operation.id,
            created_at: 2,
        };
        let subject = spec.id.to_string();
        let audit = NewAuditRecord {
            occurred_at: 2,
            actor_principal: "principal",
            authentication_method: "unix_peer",
            community_config_id: Some(config.id),
            operation_id: Some(operation.id),
            correlation_id: "duplicate",
            idempotency_key: Some("duplicate"),
            action: "agent.create",
            subject_type: "agent",
            subject_id: Some(&subject),
            outcome: "accepted",
            redacted_detail: None,
        };
        assert!(matches!(
            store.apply_agent_command(
                &operation,
                &idempotency,
                AgentCommandMutation::Create(&spec),
                None,
                audit.clone()
            ),
            Err(StorageError::Conflict(_))
        ));

        let missing = AgentId::new();
        let operation = DurableOperation {
            id: OperationId::new(),
            kind: OperationKind::UpdateAgent,
            agent_id: Some(missing),
            correlation_id: "missing".into(),
            ..operation
        };
        let changes = crate::api::UpdateAgentInput {
            display_name: Some("Changed".into()),
            ..Default::default()
        };
        assert!(matches!(
            store.apply_agent_command(
                &operation,
                &IdempotencyRecord {
                    scope: "principal:update".into(),
                    key: "missing".into(),
                    operation_id: operation.id,
                    ..idempotency
                },
                AgentCommandMutation::Update {
                    id: missing,
                    changes: &changes
                },
                None,
                NewAuditRecord {
                    operation_id: Some(operation.id),
                    subject_id: None,
                    action: "agent.update",
                    ..audit
                }
            ),
            Err(StorageError::NotFound)
        ));
    }

    #[test]
    fn relay_publication_outbox_tracks_projection_and_retry_state() {
        let store = SqliteStore::open_in_memory().unwrap();
        let community_id = CommunityConfigId::new();
        let subject_id = AgentId::new().to_string();
        store
            .record_relay_projection(
                RelayProjectionKind::ManagedAgent,
                &subject_id,
                &RelayProjectionScope {
                    community_config_id: community_id,
                    relay_url: "wss://relay.example.com/".into(),
                    owner_pubkey: "owner-pubkey".into(),
                    d_tag: "agent-pubkey".into(),
                },
                10,
            )
            .unwrap();
        let scopes = store
            .relay_projection_scopes(RelayProjectionKind::ManagedAgent, &subject_id)
            .unwrap();
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].d_tag, "agent-pubkey");
        store
            .enqueue_relay_publication(
                RelayPublicationAction::TombstoneManagedAgent,
                &scopes[0],
                &subject_id,
                11,
            )
            .unwrap();
        let pending = store.pending_relay_publications().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].attempts, 0);
        assert!(store
            .has_pending_relay_publications_for_owner("owner-pubkey")
            .unwrap());
        store
            .fail_relay_publication(&pending[0].id, "offline", 12)
            .unwrap();
        assert_eq!(store.pending_relay_publications().unwrap()[0].attempts, 1);
        store.complete_relay_publication(&pending[0].id).unwrap();
        assert!(!store
            .has_pending_relay_publications_for_owner("owner-pubkey")
            .unwrap());
        store
            .remove_relay_projection(community_id, RelayProjectionKind::ManagedAgent, &subject_id)
            .unwrap();
        assert!(store
            .relay_projection_scopes(RelayProjectionKind::ManagedAgent, &subject_id)
            .unwrap()
            .is_empty());
    }
}
