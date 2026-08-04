//! Version-pinned runtime catalog independent of process supervision backends.

use std::{collections::BTreeSet, fmt, path::Path, str::FromStr};

use serde::{Deserialize, Serialize};

const MAX_TIMEOUT_SECONDS: u32 = 300;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RuntimeId(String);

impl RuntimeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogError> {
        let value = value.into();
        if value.is_empty() || value.len() > 80 {
            return Err(CatalogError::InvalidRuntimeId(value));
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        }) {
            return Err(CatalogError::InvalidRuntimeId(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuntimeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for RuntimeId {
    type Err = CatalogError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for RuntimeId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeArtifact {
    /// An executable installed at `RuntimeCatalogEntry::command`.
    LocalExecutable {
        #[serde(skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
    Package {
        manager: String,
        name: String,
        version: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SecretReference {
    pub environment_key: String,
    pub secret_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreflightProbe {
    pub timeout_seconds: u32,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeCatalogEntry {
    pub id: RuntimeId,
    pub version: String,
    pub artifact: RuntimeArtifact,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight: Option<PreflightProbe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_secrets: Vec<SecretReference>,
}

impl RuntimeCatalogEntry {
    pub fn validate(&self) -> Result<(), CatalogError> {
        validate_text("version", &self.version, 120)?;
        if matches!(self.version.as_str(), "latest" | "stable" | "current")
            || self.version.contains('*')
            || self.version.contains('^')
            || self.version.contains('~')
        {
            return Err(CatalogError::UnpinnedVersion(self.version.clone()));
        }
        validate_absolute_command("command", &self.command)?;
        validate_arguments("arguments", &self.arguments)?;
        match &self.artifact {
            RuntimeArtifact::LocalExecutable { sha256 } => {
                if sha256.as_ref().is_some_and(|digest| !valid_sha256(digest)) {
                    return Err(CatalogError::InvalidField("artifact.sha256"));
                }
            }
            RuntimeArtifact::Package {
                manager,
                name,
                version,
            } => {
                validate_text("artifact.manager", manager, 80)?;
                validate_text("artifact.name", name, 240)?;
                validate_text("artifact.version", version, 120)?;
                if version != &self.version {
                    return Err(CatalogError::ArtifactVersionMismatch);
                }
            }
        }
        if let Some(probe) = &self.preflight {
            if probe.timeout_seconds == 0 || probe.timeout_seconds > MAX_TIMEOUT_SECONDS {
                return Err(CatalogError::InvalidTimeout(probe.timeout_seconds));
            }
            validate_absolute_command("preflight.command", &probe.command)?;
            validate_arguments("preflight.arguments", &probe.arguments)?;
        }
        let mut keys = BTreeSet::new();
        let mut names = BTreeSet::new();
        for secret in &self.required_secrets {
            if !valid_environment_key(&secret.environment_key) {
                return Err(CatalogError::InvalidField(
                    "required_secrets.environment_key",
                ));
            }
            validate_text("required_secrets.secret_name", &secret.secret_name, 240)?;
            if !keys.insert(&secret.environment_key) || !names.insert(&secret.secret_name) {
                return Err(CatalogError::DuplicateSecretReference);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeCatalog {
    pub runtimes: Vec<RuntimeCatalogEntry>,
}

impl RuntimeCatalog {
    pub fn validate(&self) -> Result<(), CatalogError> {
        let mut ids = BTreeSet::new();
        for runtime in &self.runtimes {
            runtime.validate()?;
            if !ids.insert(&runtime.id) {
                return Err(CatalogError::DuplicateRuntime(runtime.id.clone()));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: &RuntimeId) -> Option<&RuntimeCatalogEntry> {
        self.runtimes.iter().find(|runtime| &runtime.id == id)
    }

    pub fn require(&self, id: &RuntimeId) -> Result<&RuntimeCatalogEntry, CatalogError> {
        self.get(id)
            .ok_or_else(|| CatalogError::RuntimeNotFound(id.clone()))
    }

    /// Loads the initial Local catalog from deployment-supplied immutable
    /// entries. Versions and paths are intentionally configuration, since this
    /// crate cannot truthfully pin whatever Sprig and Codex packages an operator
    /// has installed.
    pub fn load_initial_local(
        sprig: RuntimeCatalogEntry,
        codex: RuntimeCatalogEntry,
    ) -> Result<Self, CatalogError> {
        let sprig_id = RuntimeId::parse("sprig-buzz-agent").expect("static runtime ID is valid");
        let codex_id = RuntimeId::parse("codex-acp").expect("static runtime ID is valid");
        if sprig.id != sprig_id {
            return Err(CatalogError::RequiredRuntimeId {
                expected: sprig_id,
                actual: sprig.id,
            });
        }
        if codex.id != codex_id {
            return Err(CatalogError::RequiredRuntimeId {
                expected: codex_id,
                actual: codex.id,
            });
        }
        let catalog = Self {
            runtimes: vec![sprig, codex],
        };
        catalog.validate()?;
        Ok(catalog)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CatalogError {
    #[error("invalid runtime identifier: {0}")]
    InvalidRuntimeId(String),
    #[error("{0} is invalid")]
    InvalidField(&'static str),
    #[error("runtime version must be immutable and exact: {0}")]
    UnpinnedVersion(String),
    #[error("package artifact version must equal the runtime version")]
    ArtifactVersionMismatch,
    #[error("preflight timeout must be between 1 and {MAX_TIMEOUT_SECONDS} seconds, got {0}")]
    InvalidTimeout(u32),
    #[error("duplicate runtime identifier: {0}")]
    DuplicateRuntime(RuntimeId),
    #[error("duplicate required secret reference")]
    DuplicateSecretReference,
    #[error("runtime not found: {0}")]
    RuntimeNotFound(RuntimeId),
    #[error("required runtime ID is {expected}, got {actual}")]
    RequiredRuntimeId {
        expected: RuntimeId,
        actual: RuntimeId,
    },
}

fn validate_text(field: &'static str, value: &str, max: usize) -> Result<(), CatalogError> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(CatalogError::InvalidField(field));
    }
    Ok(())
}

fn validate_command(field: &'static str, value: &str) -> Result<(), CatalogError> {
    validate_text(field, value, 1024)
}

fn validate_absolute_command(field: &'static str, value: &str) -> Result<(), CatalogError> {
    validate_command(field, value)?;
    if !Path::new(value).is_absolute() {
        return Err(CatalogError::InvalidField(field));
    }
    Ok(())
}

fn validate_arguments(field: &'static str, values: &[String]) -> Result<(), CatalogError> {
    if values.len() > 128
        || values
            .iter()
            .any(|value| value.len() > 4096 || value.contains('\0'))
    {
        return Err(CatalogError::InvalidField(field));
    }
    Ok(())
}

fn valid_environment_key(key: &str) -> bool {
    let mut characters = key.chars();
    matches!(characters.next(), Some('A'..='Z' | '_'))
        && characters.all(|character| matches!(character, 'A'..='Z' | '0'..='9' | '_'))
}

pub(crate) fn valid_sha256(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> RuntimeCatalogEntry {
        RuntimeCatalogEntry {
            id: "codex-acp".parse().unwrap(),
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
                command: "/opt/buzz/bin/codex-acp".into(),
                arguments: vec!["--version".into()],
            }),
            required_secrets: vec![SecretReference {
                environment_key: "OPENAI_API_KEY".into(),
                secret_name: "openai-api-key".into(),
            }],
        }
    }

    #[test]
    fn catalog_validates_serializes_and_looks_up() {
        let catalog = RuntimeCatalog {
            runtimes: vec![entry()],
        };
        catalog.validate().unwrap();
        let json = serde_json::to_string(&catalog).unwrap();
        let decoded: RuntimeCatalog = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, catalog);
        assert_eq!(
            decoded
                .require(&"codex-acp".parse().unwrap())
                .unwrap()
                .version,
            "1.2.3"
        );
    }

    #[test]
    fn runtime_ids_are_canonical_and_validated_during_deserialization() {
        assert!(matches!(
            RuntimeId::parse("Codex ACP"),
            Err(CatalogError::InvalidRuntimeId(_))
        ));
        assert!(serde_json::from_str::<RuntimeId>("\"../codex\"").is_err());
    }

    #[test]
    fn mutable_versions_are_rejected() {
        for version in ["latest", "^1.2.3", "1.*"] {
            let mut value = entry();
            value.version = version.into();
            assert!(matches!(
                value.validate(),
                Err(CatalogError::UnpinnedVersion(_))
            ));
        }
    }

    #[test]
    fn duplicate_runtime_and_secret_references_are_rejected() {
        let item = entry();
        let catalog = RuntimeCatalog {
            runtimes: vec![item.clone(), item.clone()],
        };
        assert!(matches!(
            catalog.validate(),
            Err(CatalogError::DuplicateRuntime(_))
        ));

        let mut item = item;
        item.required_secrets.push(item.required_secrets[0].clone());
        assert_eq!(item.validate(), Err(CatalogError::DuplicateSecretReference));
    }

    #[test]
    fn preflight_and_package_pin_are_checked() {
        let mut item = entry();
        item.preflight.as_mut().unwrap().timeout_seconds = 0;
        assert_eq!(item.validate(), Err(CatalogError::InvalidTimeout(0)));
        item.preflight.as_mut().unwrap().timeout_seconds = 10;
        if let RuntimeArtifact::Package { version, .. } = &mut item.artifact {
            *version = "1.2.4".into();
        }
        assert_eq!(item.validate(), Err(CatalogError::ArtifactVersionMismatch));
    }

    #[test]
    fn local_executable_artifacts_are_supported() {
        let mut item = entry();
        item.artifact = RuntimeArtifact::LocalExecutable { sha256: None };
        item.validate().unwrap();
    }

    #[test]
    fn initial_local_catalog_requires_explicit_pinned_sprig_and_codex_entries() {
        let codex = entry();
        let mut sprig = entry();
        sprig.id = "sprig-buzz-agent".parse().unwrap();
        sprig.artifact = RuntimeArtifact::LocalExecutable {
            sha256: Some("a".repeat(64)),
        };
        assert!(RuntimeCatalog::load_initial_local(sprig.clone(), codex.clone()).is_ok());

        let valid_sprig = sprig.clone();
        let mut wrong_sprig = sprig;
        wrong_sprig.id = "buzz-agent".parse().unwrap();
        assert!(matches!(
            RuntimeCatalog::load_initial_local(wrong_sprig, codex.clone()),
            Err(CatalogError::RequiredRuntimeId { .. })
        ));
        let mut wrong_codex = codex;
        wrong_codex.id = "codex".parse().unwrap();
        assert!(matches!(
            RuntimeCatalog::load_initial_local(valid_sprig, wrong_codex),
            Err(CatalogError::RequiredRuntimeId { .. })
        ));
    }

    #[test]
    fn local_commands_and_preflight_commands_must_be_absolute() {
        let mut item = entry();
        item.command = "codex-acp".into();
        assert_eq!(item.validate(), Err(CatalogError::InvalidField("command")));
        item.command = "/opt/buzz/bin/codex-acp".into();
        item.preflight.as_mut().unwrap().command = "codex-acp".into();
        assert_eq!(
            item.validate(),
            Err(CatalogError::InvalidField("preflight.command"))
        );
    }
}
