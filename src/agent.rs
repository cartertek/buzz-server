//! Desired agent lifecycle state plus human-authored file configuration.

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    community::validate_nonempty, launch::SecretRef, provider::secret_shaped_key, AgentId,
    CommunityConfigId, RuntimeId, ValidationError,
};

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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_environment: BTreeMap<String, SecretRef>,
}

impl RuntimeSpec {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_environment(&self.environment)
            .map_err(|error| ValidationError::new("runtime.environment", error.message))?;
        validate_secret_environment(&self.environment, &self.secret_environment, "runtime")?;
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_environment: BTreeMap<String, SecretRef>,
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
        validate_secret_environment(&self.environment, &self.secret_environment, "persona")?;
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_environment: BTreeMap<String, SecretRef>,
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
            if self.system_prompt_file.is_none() {
                validate_nonempty("system_prompt", prompt, 65_536)?;
            }
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
        validate_secret_environment(&self.environment, &self.secret_environment, "agent")?;
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
    if environment.keys().any(|key| secret_shaped_key(key)) {
        return Err(ValidationError::new(
            "environment",
            "contains a secret-shaped key; use secret_environment with a Server secret reference",
        ));
    }
    Ok(())
}

fn validate_secret_environment(
    environment: &BTreeMap<String, String>,
    secret_environment: &BTreeMap<String, SecretRef>,
    scope: &str,
) -> Result<(), ValidationError> {
    for (key, reference) in secret_environment {
        if !valid_environment_key(key) {
            return Err(ValidationError::new(
                "secret_environment",
                "contains an invalid environment variable name",
            ));
        }
        if environment.contains_key(key) {
            return Err(ValidationError::new(
                "secret_environment",
                format!("{scope} secret key {key} shadows a non-secret environment value"),
            ));
        }
        if reference.key.is_empty()
            || reference.key.len() > 256
            || reference.key.contains(char::is_whitespace)
            || reference.key.contains('\0')
        {
            return Err(ValidationError::new(
                "secret_environment",
                "contains an invalid secret reference",
            ));
        }
        if reference
            .version
            .as_ref()
            .is_some_and(|version| version.is_empty() || version.len() > 160)
        {
            return Err(ValidationError::new(
                "secret_environment",
                "secret reference version must contain 1 to 160 characters when present",
            ));
        }
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
                secret_environment: BTreeMap::new(),
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
    fn plain_environment_rejects_secret_shaped_keys_but_accepts_ordinary_keys() {
        let mut spec = valid_spec();
        spec.runtime
            .environment
            .insert("OPENAI_API_KEY".to_owned(), "inline-value".to_owned());
        let error = spec.validate().unwrap_err();
        assert_eq!(error.field, "runtime.environment");
        assert!(!error.to_string().contains("inline-value"));

        let mut persona = PersonaDefinition {
            id: "safe".into(),
            display_name: "Safe".into(),
            avatar_url: None,
            system_prompt: "Safe".into(),
            runtime: None,
            model: None,
            provider: None,
            name_pool: vec![],
            environment: BTreeMap::from([(String::from("LOG_LEVEL"), String::from("info"))]),
            secret_environment: BTreeMap::new(),
            respond_to: None,
            respond_to_allowlist: vec![],
            parallelism: None,
            is_builtin: false,
            is_active: true,
            shared: false,
        };
        persona.validate().unwrap();
        persona
            .environment
            .insert("SERVICE_PASSWORD".into(), "inline-value".into());
        assert_eq!(persona.validate().unwrap_err().field, "environment");
    }

    #[test]
    fn config_files_round_trip_secret_references_and_omit_legacy_field() {
        let secret = SecretRef {
            key: "agent/example/openai".into(),
            version: Some("v1".into()),
        };
        let agent = AgentConfigFile {
            id: AgentId::new(),
            display_name: "Builder".into(),
            persona_id: None,
            avatar_url: None,
            system_prompt: Some("Build safely.".into()),
            system_prompt_file: None,
            runtime: Some("codex-acp".parse().unwrap()),
            model: None,
            provider: None,
            environment: BTreeMap::from([(String::from("LOG_LEVEL"), String::from("info"))]),
            secret_environment: BTreeMap::from([(String::from("OPENAI_API_KEY"), secret)]),
            filesystem: FilesystemConfig::default(),
            auto_join_open_channels: AutoJoinOpenChannels::Disabled,
            agent_args: vec![],
            parallelism: DEFAULT_AGENT_PARALLELISM,
            respond_to: RespondToMode::OwnerOnly,
            respond_to_allowlist: vec![],
            idle_timeout_seconds: None,
            max_turn_duration_seconds: None,
        };
        let encoded = serde_json::to_string(&agent).unwrap();
        assert!(encoded.contains("agent/example/openai"));
        assert!(!encoded.contains("inline-value"));
        assert_eq!(
            serde_json::from_str::<AgentConfigFile>(&encoded).unwrap(),
            agent
        );

        let mut legacy = serde_json::to_value(&agent).unwrap();
        legacy.as_object_mut().unwrap().remove("secret_environment");
        let loaded = serde_json::from_value::<AgentConfigFile>(legacy).unwrap();
        assert!(loaded.secret_environment.is_empty());
    }

    #[test]
    fn secret_environment_round_trips_without_a_secret_value() {
        let mut spec = valid_spec();
        spec.runtime.secret_environment.insert(
            "OPENAI_API_KEY".into(),
            SecretRef {
                key: "agent/example/openai".into(),
                version: Some("v1".into()),
            },
        );
        let encoded = serde_json::to_string(&spec).unwrap();
        assert!(encoded.contains("agent/example/openai"));
        assert!(!encoded.contains("secret-value"));
        assert_eq!(serde_json::from_str::<AgentSpec>(&encoded).unwrap(), spec);
    }

    #[test]
    fn plain_and_secret_environment_keys_collide() {
        let mut spec = valid_spec();
        spec.runtime.secret_environment.insert(
            "CODEX_HOME".into(),
            SecretRef {
                key: "x".into(),
                version: None,
            },
        );
        assert_eq!(spec.validate().unwrap_err().field, "secret_environment");
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
