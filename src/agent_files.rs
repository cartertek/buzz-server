//! Individual human-editable agent/persona files for Buzz Server.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    AgentConfigFile, AgentId, AgentSpec, AutoJoinOpenChannels, CommunityConfigId,
    DesiredAgentState, FilesystemConfig, PersonaDefinition, ResolvedAgentConfig, RespondToMode,
    RuntimeId, RuntimeSpec, ValidationError, DEFAULT_AGENT_PARALLELISM,
};

#[derive(Clone, Debug)]
pub struct AgentFileStore {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentFileError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid agent configuration: {0}")]
    Validation(#[from] ValidationError),
    #[error("persona {0} not found")]
    PersonaNotFound(String),
    #[error("persona {persona_id} is still referenced by agents: {agents}")]
    PersonaReferenced { persona_id: String, agents: String },
    #[error("linked persona {0} has no runtime and the agent has no runtime override")]
    RuntimeRequired(String),
    #[error("standalone agent requires a runtime")]
    StandaloneRuntimeRequired,
    #[error("agent file id {found} does not match expected id {expected}")]
    AgentIdMismatch { expected: AgentId, found: AgentId },
    #[error("cannot read system prompt file {path}: {message}")]
    SystemPromptFile { path: String, message: String },
}

impl AgentFileStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, AgentFileError> {
        let store = Self { root: root.into() };
        fs::create_dir_all(store.agents_dir())?;
        fs::create_dir_all(store.personas_dir())?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }

    pub fn personas_dir(&self) -> PathBuf {
        self.root.join("personas")
    }

    pub fn agent_path(&self, id: AgentId) -> PathBuf {
        self.agents_dir().join(format!("{id}.json"))
    }

    pub fn persona_path(&self, id: &str) -> Result<PathBuf, AgentFileError> {
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ValidationError::new(
                "persona_id",
                "must contain only letters, numbers, '-' or '_'",
            )
            .into());
        }
        Ok(self.personas_dir().join(format!("{id}.json")))
    }

    pub fn load_agent(&self, id: AgentId) -> Result<AgentConfigFile, AgentFileError> {
        let value: AgentConfigFile = serde_json::from_slice(&fs::read(self.agent_path(id))?)?;
        if value.id != id {
            return Err(AgentFileError::AgentIdMismatch {
                expected: id,
                found: value.id,
            });
        }
        value.validate()?;
        Ok(value)
    }

    pub fn load_persona(&self, id: &str) -> Result<PersonaDefinition, AgentFileError> {
        let path = self.persona_path(id)?;
        if !path.exists() {
            return Err(AgentFileError::PersonaNotFound(id.to_owned()));
        }
        let value: PersonaDefinition = serde_json::from_slice(&fs::read(path)?)?;
        value.validate()?;
        if value.id != id {
            return Err(
                ValidationError::new("persona.id", "must match the persona filename").into(),
            );
        }
        Ok(value)
    }

    pub fn list_personas(&self) -> Result<Vec<PersonaDefinition>, AgentFileError> {
        let mut values = Vec::new();
        for entry in fs::read_dir(self.personas_dir())? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let value: PersonaDefinition = serde_json::from_slice(&fs::read(&path)?)?;
            value.validate()?;
            values.push(value);
        }
        values.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(values)
    }

    pub fn ensure_persona_removable(&self, id: &str) -> Result<PersonaDefinition, AgentFileError> {
        let persona = self.load_persona(id)?;
        let mut linked = Vec::new();
        for entry in fs::read_dir(self.agents_dir())? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let value: AgentConfigFile = serde_json::from_slice(&fs::read(path)?)?;
            if value.persona_id.as_deref() == Some(id) {
                linked.push(value.id.to_string());
            }
        }
        if !linked.is_empty() {
            linked.sort();
            return Err(AgentFileError::PersonaReferenced {
                persona_id: id.to_owned(),
                agents: linked.join(", "),
            });
        }
        Ok(persona)
    }

    pub fn remove_persona(&self, id: &str) -> Result<PersonaDefinition, AgentFileError> {
        let persona = self.ensure_persona_removable(id)?;
        fs::remove_file(self.persona_path(id)?)?;
        Ok(persona)
    }

    pub fn write_agent(&self, value: &AgentConfigFile) -> Result<(), AgentFileError> {
        value.validate()?;
        atomic_pretty_json(&self.agent_path(value.id), value)
    }

    pub fn write_persona(&self, value: &PersonaDefinition) -> Result<(), AgentFileError> {
        value.validate()?;
        atomic_pretty_json(&self.persona_path(&value.id)?, value)
    }

    pub fn remove_agent(&self, id: AgentId) -> Result<(), AgentFileError> {
        match fs::remove_file(self.agent_path(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn ensure_agent_file(&self, spec: &AgentSpec) -> Result<(), AgentFileError> {
        if self.agent_path(spec.id).exists() {
            return Ok(());
        }
        self.write_agent(&AgentConfigFile {
            id: spec.id,
            display_name: spec.display_name.clone(),
            persona_id: None,
            avatar_url: None,
            system_prompt: (!spec.system_prompt.is_empty()).then(|| spec.system_prompt.clone()),
            system_prompt_file: None,
            runtime: Some(spec.runtime.runtime_id.clone()),
            model: None,
            provider: None,
            environment: spec.runtime.environment.clone(),
            filesystem: Default::default(),
            auto_join_open_channels: AutoJoinOpenChannels::Disabled,
            agent_args: vec![],
            parallelism: DEFAULT_AGENT_PARALLELISM,
            respond_to: RespondToMode::OwnerOnly,
            respond_to_allowlist: vec![],
            idle_timeout_seconds: None,
            max_turn_duration_seconds: None,
        })
    }

    pub fn resolve(
        &self,
        file: &AgentConfigFile,
        community_config_id: CommunityConfigId,
        desired_state: DesiredAgentState,
    ) -> Result<ResolvedAgentConfig, AgentFileError> {
        file.validate()?;
        let (runtime_id, system_prompt, model, provider, environment) =
            if let Some(persona_id) = file.persona_id.as_deref() {
                let persona = self.load_persona(persona_id)?;
                let runtime = file
                    .runtime
                    .clone()
                    .or(persona.runtime.clone())
                    .ok_or_else(|| AgentFileError::RuntimeRequired(persona_id.to_owned()))?;
                let mut env = persona.environment;
                env.extend(file.environment.clone());
                (
                    runtime,
                    persona.system_prompt.trim().to_owned(),
                    nonblank(persona.model),
                    nonblank(persona.provider),
                    env,
                )
            } else {
                (
                    file.runtime
                        .clone()
                        .ok_or(AgentFileError::StandaloneRuntimeRequired)?,
                    resolve_system_prompt(file)?,
                    nonblank(file.model.clone()),
                    nonblank(file.provider.clone()),
                    file.environment.clone(),
                )
            };
        let spec = AgentSpec {
            id: file.id,
            community_config_id,
            display_name: file.display_name.clone(),
            system_prompt,
            runtime: RuntimeSpec {
                runtime_id,
                environment,
            },
            desired_state,
        };
        spec.validate()?;
        Ok(ResolvedAgentConfig {
            spec,
            persona_id: file.persona_id.clone(),
            avatar_url: file.avatar_url.clone(),
            model,
            provider,
            filesystem: file.filesystem.clone(),
            agent_args: file.agent_args.clone(),
            parallelism: file.parallelism,
            respond_to: file.respond_to,
            respond_to_allowlist: file
                .respond_to_allowlist
                .iter()
                .map(|v| v.to_ascii_lowercase())
                .collect(),
            idle_timeout_seconds: file.idle_timeout_seconds.filter(|v| *v > 0),
            max_turn_duration_seconds: file.max_turn_duration_seconds.filter(|v| *v > 0),
        })
    }

    pub fn build_create_file(
        &self,
        id: AgentId,
        display_name: String,
        persona_id: Option<String>,
        system_prompt: Option<String>,
        system_prompt_file: Option<String>,
        runtime: Option<RuntimeId>,
        filesystem_user: Option<String>,
    ) -> Result<AgentConfigFile, AgentFileError> {
        let (avatar_url, parallelism, respond_to, respond_to_allowlist, prompt_snapshot) =
            match persona_id.as_deref() {
                Some(pid) => {
                    let p = self.load_persona(pid)?;
                    (
                        p.avatar_url,
                        p.parallelism.unwrap_or(DEFAULT_AGENT_PARALLELISM),
                        p.respond_to.unwrap_or_default(),
                        p.respond_to_allowlist,
                        Some(p.system_prompt),
                    )
                }
                None => (
                    None,
                    DEFAULT_AGENT_PARALLELISM,
                    RespondToMode::OwnerOnly,
                    vec![],
                    system_prompt,
                ),
            };
        let file = AgentConfigFile {
            id,
            display_name,
            persona_id,
            avatar_url,
            system_prompt: prompt_snapshot,
            system_prompt_file,
            runtime,
            model: None,
            provider: None,
            environment: BTreeMap::new(),
            filesystem: FilesystemConfig {
                user: filesystem_user,
            },
            auto_join_open_channels: AutoJoinOpenChannels::Disabled,
            agent_args: vec![],
            parallelism,
            respond_to,
            respond_to_allowlist,
            idle_timeout_seconds: None,
            max_turn_duration_seconds: None,
        };
        file.validate()?;
        Ok(file)
    }
}

fn resolve_system_prompt(file: &AgentConfigFile) -> Result<String, AgentFileError> {
    if let Some(prompt) = file
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Ok(prompt.to_owned());
    }
    let Some(path) = file.system_prompt_file.as_deref() else {
        return Ok(String::new());
    };
    let resolved =
        PathBuf::from(path)
            .canonicalize()
            .map_err(|error| AgentFileError::SystemPromptFile {
                path: path.to_owned(),
                message: format!("cannot resolve file: {error}"),
            })?;
    let metadata = fs::metadata(&resolved).map_err(|error| AgentFileError::SystemPromptFile {
        path: path.to_owned(),
        message: format!("cannot inspect file: {error}"),
    })?;
    if !metadata.is_file() {
        return Err(AgentFileError::SystemPromptFile {
            path: path.to_owned(),
            message: "path is not a regular file".into(),
        });
    }
    let bytes = fs::read(&resolved).map_err(|error| AgentFileError::SystemPromptFile {
        path: path.to_owned(),
        message: format!("cannot read file: {error}"),
    })?;
    String::from_utf8(bytes)
        .map(|prompt| prompt.trim().to_owned())
        .map_err(|error| AgentFileError::SystemPromptFile {
            path: path.to_owned(),
            message: format!("file is not valid UTF-8: {error}"),
        })
}

fn nonblank(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn atomic_pretty_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), AgentFileError> {
    let payload = serde_json::to_vec_pretty(value)?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(&payload)?;
    tmp.write_all(b"\n")?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| AgentFileError::Io(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona() -> PersonaDefinition {
        PersonaDefinition {
            id: "reviewer".into(),
            display_name: "Reviewer".into(),
            avatar_url: Some("https://example.test/reviewer.png".into()),
            system_prompt: "Review carefully.".into(),
            runtime: Some("codex-acp".parse().unwrap()),
            model: Some("persona-model".into()),
            provider: Some("persona-provider".into()),
            name_pool: vec![],
            environment: BTreeMap::from([
                ("PERSONA_ONLY".into(), "one".into()),
                ("SHARED".into(), "persona".into()),
            ]),
            respond_to: Some(RespondToMode::Anyone),
            respond_to_allowlist: vec![],
            parallelism: Some(3),
            is_builtin: false,
            is_active: true,
            shared: false,
        }
    }

    #[test]
    fn linked_persona_matches_desktop_effective_field_rules() {
        let directory = tempfile::tempdir().unwrap();
        let store = AgentFileStore::new(directory.path()).unwrap();
        store.write_persona(&persona()).unwrap();
        let id = AgentId::new();
        let file = AgentConfigFile {
            id,
            display_name: "Review bot".into(),
            persona_id: Some("reviewer".into()),
            avatar_url: Some("snapshot-avatar".into()),
            system_prompt: Some("stale snapshot".into()),
            system_prompt_file: None,
            runtime: Some("claude-acp".parse().unwrap()),
            model: Some("stale-model".into()),
            provider: Some("stale-provider".into()),
            environment: BTreeMap::from([
                ("AGENT_ONLY".into(), "two".into()),
                ("SHARED".into(), "agent".into()),
            ]),
            filesystem: FilesystemConfig::default(),
            auto_join_open_channels: AutoJoinOpenChannels::All,
            agent_args: vec!["--stdio".into()],
            parallelism: 7,
            respond_to: RespondToMode::OwnerOnly,
            respond_to_allowlist: vec![],
            idle_timeout_seconds: Some(60),
            max_turn_duration_seconds: Some(600),
        };
        let community = CommunityConfigId::new();
        let resolved = store
            .resolve(&file, community, DesiredAgentState::Enabled)
            .unwrap();
        assert_eq!(resolved.spec.system_prompt, "Review carefully.");
        assert_eq!(resolved.spec.runtime.runtime_id.to_string(), "claude-acp");
        assert_eq!(resolved.model.as_deref(), Some("persona-model"));
        assert_eq!(resolved.provider.as_deref(), Some("persona-provider"));
        assert_eq!(resolved.spec.runtime.environment["PERSONA_ONLY"], "one");
        assert_eq!(resolved.spec.runtime.environment["AGENT_ONLY"], "two");
        assert_eq!(resolved.spec.runtime.environment["SHARED"], "agent");
        assert_eq!(resolved.avatar_url.as_deref(), Some("snapshot-avatar"));
        assert_eq!(resolved.agent_args, vec!["--stdio"]);
        assert_eq!(resolved.parallelism, 7);
        assert_eq!(resolved.respond_to, RespondToMode::OwnerOnly);
    }

    #[test]
    fn system_prompt_file_resolves_and_inline_prompt_wins() {
        let directory = tempfile::tempdir().unwrap();
        let store = AgentFileStore::new(directory.path()).unwrap();
        let prompt_path = directory.path().join("prompt.md");
        std::fs::write(&prompt_path, "from file\n").unwrap();
        let mut file = store
            .build_create_file(
                AgentId::new(),
                "Builder".into(),
                None,
                None,
                None,
                Some("codex-acp".parse().unwrap()),
                None,
            )
            .unwrap();
        file.system_prompt_file = Some(prompt_path.display().to_string());
        assert_eq!(
            store
                .resolve(&file, CommunityConfigId::new(), DesiredAgentState::Enabled)
                .unwrap()
                .spec
                .system_prompt,
            "from file"
        );
        std::fs::write(&prompt_path, "changed").unwrap();
        assert_eq!(
            store
                .resolve(&file, CommunityConfigId::new(), DesiredAgentState::Enabled)
                .unwrap()
                .spec
                .system_prompt,
            "changed"
        );
        file.system_prompt = Some("inline".into());
        assert_eq!(
            store
                .resolve(&file, CommunityConfigId::new(), DesiredAgentState::Enabled)
                .unwrap()
                .spec
                .system_prompt,
            "inline"
        );

        let created = store
            .build_create_file(
                AgentId::new(),
                "Inline wins".into(),
                None,
                Some("inline at create".into()),
                Some(prompt_path.display().to_string()),
                Some("codex-acp".parse().unwrap()),
                None,
            )
            .unwrap();
        assert_eq!(created.system_prompt.as_deref(), Some("inline at create"));
    }

    #[test]
    fn system_prompt_file_failures_are_actionable() {
        let directory = tempfile::tempdir().unwrap();
        let store = AgentFileStore::new(directory.path()).unwrap();
        let mut file = store
            .build_create_file(
                AgentId::new(),
                "Builder".into(),
                None,
                None,
                None,
                Some("codex-acp".parse().unwrap()),
                None,
            )
            .unwrap();
        let missing = directory.path().join("missing.md");
        file.system_prompt_file = Some(missing.display().to_string());
        let error = store
            .resolve(&file, CommunityConfigId::new(), DesiredAgentState::Enabled)
            .unwrap_err();
        assert!(error.to_string().contains("cannot resolve file"));
        let invalid = directory.path().join("invalid.md");
        std::fs::write(&invalid, [0xff, 0xfe]).unwrap();
        file.system_prompt_file = Some(invalid.display().to_string());
        let error = store
            .resolve(&file, CommunityConfigId::new(), DesiredAgentState::Enabled)
            .unwrap_err();
        assert!(error.to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn create_from_persona_mints_desktop_behavioral_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let store = AgentFileStore::new(directory.path()).unwrap();
        store.write_persona(&persona()).unwrap();
        let id = AgentId::new();
        let file = store
            .build_create_file(
                id,
                "Reviewer one".into(),
                Some("reviewer".into()),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(file.system_prompt.as_deref(), Some("Review carefully."));
        assert_eq!(
            file.avatar_url.as_deref(),
            Some("https://example.test/reviewer.png")
        );
        assert_eq!(file.parallelism, 3);
        assert_eq!(file.respond_to, RespondToMode::Anyone);
        assert!(
            file.runtime.is_none(),
            "runtime stays inherited rather than pinned"
        );
    }

    #[test]
    fn individual_files_round_trip_for_human_editing() {
        let directory = tempfile::tempdir().unwrap();
        let store = AgentFileStore::new(directory.path()).unwrap();
        let id = AgentId::new();
        let prompt_path = directory.path().join("prompt.md");
        let file = store
            .build_create_file(
                id,
                "Builder".into(),
                None,
                Some("Build safely.".into()),
                Some(prompt_path.display().to_string()),
                Some("codex-acp".parse().unwrap()),
                None,
            )
            .unwrap();
        store.write_agent(&file).unwrap();
        assert_eq!(store.load_agent(id).unwrap(), file);
        let json = std::fs::read_to_string(store.agent_path(id)).unwrap();
        assert!(json.contains(&format!(
            "\"system_prompt_file\": \"{}\"",
            prompt_path.display()
        )));
        assert!(
            json.contains("\n  \"display_name\""),
            "files are pretty-printed"
        );
    }

    #[test]
    fn filesystem_user_round_trips_and_resolves() {
        let directory = tempfile::tempdir().unwrap();
        let store = AgentFileStore::new(directory.path()).unwrap();
        let id = AgentId::new();
        let file = store
            .build_create_file(
                id,
                "Builder".into(),
                None,
                Some("Build safely.".into()),
                None,
                Some("codex-acp".parse().unwrap()),
                Some("ec2-user".into()),
            )
            .unwrap();
        store.write_agent(&file).unwrap();
        let resolved = store
            .resolve(
                &store.load_agent(id).unwrap(),
                CommunityConfigId::new(),
                DesiredAgentState::Enabled,
            )
            .unwrap();
        assert_eq!(resolved.filesystem.user.as_deref(), Some("ec2-user"));
    }

    #[test]
    fn file_prompt_supports_precedence_reload_and_absolute_paths() {
        let directory = tempfile::tempdir().unwrap();
        let store = AgentFileStore::new(directory.path()).unwrap();
        let prompt = directory.path().join("prompt.md");
        fs::write(&prompt, "from file\n").unwrap();
        let id = AgentId::new();
        let mut file = store
            .build_create_file(
                id,
                "Builder".into(),
                None,
                Some("".into()),
                Some(prompt.display().to_string()),
                Some("codex-acp".parse().unwrap()),
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .resolve(&file, CommunityConfigId::new(), DesiredAgentState::Enabled)
                .unwrap()
                .spec
                .system_prompt,
            "from file"
        );
        fs::write(&prompt, "reloaded").unwrap();
        assert_eq!(
            store
                .resolve(&file, CommunityConfigId::new(), DesiredAgentState::Enabled)
                .unwrap()
                .spec
                .system_prompt,
            "reloaded"
        );
        file.system_prompt = Some("inline wins".into());
        assert_eq!(
            store
                .resolve(&file, CommunityConfigId::new(), DesiredAgentState::Enabled)
                .unwrap()
                .spec
                .system_prompt,
            "inline wins"
        );

        let relative = AgentConfigFile {
            system_prompt_file: Some("prompt.md".into()),
            ..file
        };
        assert!(relative
            .validate()
            .unwrap_err()
            .to_string()
            .contains("absolute"));
    }
}
