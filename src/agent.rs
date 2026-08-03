//! Desired agent configuration independent of a deployment backend.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{community::validate_nonempty, AgentId, CommunityConfigId, ValidationError};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredAgentState {
    #[default]
    Enabled,
    Disabled,
    Deleted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeSpec {
    pub runtime_id: String,
    pub executable: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

impl RuntimeSpec {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_nonempty("runtime.runtime_id", &self.runtime_id, 80)?;
        validate_nonempty("runtime.executable", &self.executable, 1024)?;
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
        validate_nonempty("system_prompt", &self.system_prompt, 65_536)?;
        self.runtime.validate()
    }
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
                runtime_id: "codex".to_owned(),
                executable: "/opt/buzz/bin/codex-acp".to_owned(),
                arguments: vec!["--model".to_owned(), "gpt-5".to_owned()],
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
}
