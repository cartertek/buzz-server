//! Desired agent lifecycle state plus human-authored file configuration.

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::{community::validate_nonempty, AgentId, CommunityConfigId, RuntimeId, ValidationError};

pub const DEFAULT_AGENT_PARALLELISM: u32 = 10;

const fn default_agent_parallelism() -> u32 {
    DEFAULT_AGENT_PARALLELISM
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredAgentState {
    #[default]
    Enabled,
    Disabled,
    Deleted,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RespondToMode {
    #[default]
    OwnerOnly,
    Allowlist,
    Anyone,
}

impl RespondToMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OwnerOnly => "owner-only",
            Self::Allowlist => "allowlist",
            Self::Anyone => "anyone",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeSpec {
    pub runtime_id: RuntimeId,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

impl RuntimeSpec {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self
            .environment
            .keys()
            .any(|key| !valid_environment_key(key))
        {
            return Err(ValidationError::new(
                "runtime.environment",
                "contains an invalid environment variable name",
            ));
        }
        Ok(())
    }
}

/// The lifecycle cache persisted in SQLite. Human-authored configuration is
/// authoritative in the corresponding agent/persona files.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentSpec {
    pub id: AgentId,
    pub community_config_id: CommunityConfigId,
    pub display_name: String,
    pub system_prompt: String,
    pub runtime: RuntimeSpec,
    #[serde(default)]
    pub desired_state: DesiredAgentState,
}

impl AgentSpec {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_nonempty("display_name", &self.display_name, 120)?;
        if !self.system_prompt.is_empty() {
            validate_nonempty("system_prompt", &self.system_prompt, 65_536)?;
        }
        self.runtime.validate()
    }
}

/// Desktop-compatible keyless agent definition/persona stored as an individual
/// human-editable file on Buzz Server.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersonaDefinition {
    pub id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub system_prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name_pool: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub respond_to: Option<RespondToMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub respond_to_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,
    #[serde(default)]
    pub is_builtin: bool,
    #[serde(default = "default_true")]
    pub is_active: bool,
    #[serde(default)]
    pub shared: bool,
}

impl PersonaDefinition {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_identifier("persona.id", &self.id)?;
        validate_nonempty("persona.display_name", &self.display_name, 120)?;
        if self.system_prompt.chars().count() > 65_536 || self.system_prompt.contains('\0') {
            return Err(ValidationError::new(
                "persona.system_prompt",
                "must be at most 65536 NUL-free characters",
            ));
        }
        validate_environment(&self.environment)?;
        validate_behavior(
            self.parallelism,
            self.respond_to,
            &self.respond_to_allowlist,
        )
    }
}

/// Human-authored managed-agent instance. Desktop semantics are preserved:
/// linked definitions own prompt/model/provider and definition env is layered
/// below instance env; runtime is inherited unless explicitly overridden;
/// avatar/access/parallelism are mint-time instance values.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfigFile {
    pub id: AgentId,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Absolute path to a UTF-8 prompt file selected by the administrator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// Host filesystem identity for the local runtime. When omitted, Buzz
    /// Server provisions a dedicated Unix account for this agent.
    #[serde(default, skip_serializing_if = "FilesystemConfig::is_default")]
    pub filesystem: FilesystemConfig,
    /// Keep this materialized agent identity joined to open channels in its community.
    /// Membership is reconciled by Buzz Server; ACP subscription remains a separate setting.
    #[serde(default, skip_serializing_if = "AutoJoinOpenChannels::is_disabled")]
    pub auto_join_open_channels: AutoJoinOpenChannels,
    /// Per-instance ACP runtime arguments. Empty means use the selected runtime
    /// catalog defaults, matching Desktop's normalized-agent-args behavior.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_args: Vec<String>,
    #[serde(default = "default_agent_parallelism")]
    pub parallelism: u32,
    #[serde(default)]
    pub respond_to: RespondToMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub respond_to_allowlist: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turn_duration_seconds: Option<u64>,
}

/// Policy for automatically joining open channels.
///
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoJoinOpenChannels {
    #[default]
    Disabled,
    All,
    New,
}

impl AutoJoinOpenChannels {
    pub const fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }
}

impl AgentConfigFile {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_nonempty("display_name", &self.display_name, 120)?;
        if let Some(persona_id) = self.persona_id.as_deref() {
            validate_identifier("persona_id", persona_id)?;
        }
        if let Some(prompt) = self.system_prompt.as_deref() {
            if prompt.chars().count() > 65_536 || prompt.contains('\0') {
                return Err(ValidationError::new(
                    "system_prompt",
                    "must be at most 65536 NUL-free characters",
                ));
            }
        }
        if let Some(path) = self.system_prompt_file.as_deref() {
            validate_nonempty("system_prompt_file", path, 4_096)?;
            if !Path::new(path).is_absolute() {
                return Err(ValidationError::new(
                    "system_prompt_file",
                    "must be an absolute path",
                ));
            }
        }
        if self.persona_id.is_some() && self.system_prompt_file.is_some() {
            return Err(ValidationError::new(
                "system_prompt_file",
                "cannot be used with persona_id",
            ));
        }
        validate_environment(&self.environment)?;
        self.filesystem.validate()?;
        validate_agent_args(&self.agent_args)?;
        validate_behavior(
            Some(self.parallelism),
            Some(self.respond_to),
            &self.respond_to_allowlist,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedAgentConfig {
    pub spec: AgentSpec,
    pub persona_id: Option<String>,
    pub avatar_url: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub filesystem: FilesystemConfig,
    pub agent_args: Vec<String>,
    pub parallelism: u32,
    pub respond_to: RespondToMode,
    pub respond_to_allowlist: Vec<String>,
    pub idle_timeout_seconds: Option<u64>,
    pub max_turn_duration_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

impl FilesystemConfig {
    pub fn is_default(&self) -> bool {
        self.user.is_none()
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(user) = self.user.as_deref() {
            if user.is_empty()
                || user.len() > 32
                || user.starts_with('-')
                || !user
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(ValidationError::new(
                    "filesystem.user",
                    "must be a 1-32 character Unix account name",
                ));
            }
        }
        Ok(())
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ValidationError> {
    validate_nonempty(field, value, 120)?;
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ValidationError::new(
            field,
            "must contain only letters, numbers, '-' or '_'",
        ));
    }
    Ok(())
}

fn validate_environment(environment: &BTreeMap<String, String>) -> Result<(), ValidationError> {
    if environment.keys().any(|key| !valid_environment_key(key)) {
        return Err(ValidationError::new(
            "environment",
            "contains an invalid environment variable name",
        ));
    }
    Ok(())
}

fn validate_agent_args(args: &[String]) -> Result<(), ValidationError> {
    if args.len() > 256
        || args
            .iter()
            .any(|arg| arg.is_empty() || arg.contains('\0') || arg.contains(','))
    {
        return Err(ValidationError::new(
            "agent_args",
            "must contain at most 256 non-empty, comma-free, NUL-free arguments",
        ));
    }
    Ok(())
}

fn validate_behavior(
    parallelism: Option<u32>,
    respond_to: Option<RespondToMode>,
    allowlist: &[String],
) -> Result<(), ValidationError> {
    if let Some(value) = parallelism {
        if !(1..=32).contains(&value) {
            return Err(ValidationError::new(
                "parallelism",
                "must be between 1 and 32",
            ));
        }
    }
    if respond_to == Some(RespondToMode::Allowlist) && allowlist.is_empty() {
        return Err(ValidationError::new(
            "respond_to_allowlist",
            "must contain at least one pubkey in allowlist mode",
        ));
    }
    if allowlist
        .iter()
        .any(|key| key.len() != 64 || !key.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(ValidationError::new(
            "respond_to_allowlist",
            "entries must be 64-character hex pubkeys",
        ));
    }
    Ok(())
}

fn valid_environment_key(key: &str) -> bool {
    let mut characters = key.chars();
    matches!(characters.next(), Some('A'..='Z' | '_'))
        && characters.all(|character| matches!(character, 'A'..='Z' | '0'..='9' | '_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_spec() -> AgentSpec {
        AgentSpec {
            id: AgentId::new(),
            community_config_id: CommunityConfigId::new(),
            display_name: "Build agent".to_owned(),
            system_prompt: "Build and verify the requested change.".to_owned(),
            runtime: RuntimeSpec {
                runtime_id: "codex-acp".parse().unwrap(),
                environment: BTreeMap::from([("CODEX_HOME".to_owned(), "/state".to_owned())]),
            },
            desired_state: DesiredAgentState::Enabled,
        }
    }

    #[test]
    fn agent_spec_validates_and_round_trips() {
        let spec = valid_spec();
        spec.validate().unwrap();
        let encoded = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<AgentSpec>(&encoded).unwrap(), spec);
    }

    #[test]
    fn environment_keys_are_restricted() {
        let mut spec = valid_spec();
        spec.runtime
            .environment
            .insert("invalid-key".to_owned(), "secret".to_owned());
        assert_eq!(spec.validate().unwrap_err().field, "runtime.environment");
    }

    #[test]
    fn auto_join_modes_reject_booleans_and_accept_named_modes() {
        assert!(serde_json::from_str::<AutoJoinOpenChannels>("true").is_err());
        assert!(serde_json::from_str::<AutoJoinOpenChannels>("false").is_err());
        assert_eq!(
            serde_json::from_str::<AutoJoinOpenChannels>(r#""new""#).unwrap(),
            AutoJoinOpenChannels::New
        );
        assert_eq!(
            serde_json::to_string(&AutoJoinOpenChannels::All).unwrap(),
            r#""all""#
        );
    }
}
