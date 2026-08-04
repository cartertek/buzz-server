//! Version-1 external provider negotiation and deployment boundary.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::CommandExt};

use serde::{Deserialize, Serialize};

use crate::provider_discovery::{hex_digest, sha256_reader, ProviderCandidate};

pub const PROVIDER_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderInfo {
    pub ok: bool,
    pub name: String,
    pub version: String,
    pub protocol_version: u32,
    pub description: String,
    pub config_schema: serde_json::Value,
}

impl ProviderInfo {
    fn validate(&self) -> Result<(), ProviderError> {
        if !self.ok
            || self.name.trim().is_empty()
            || self.version.trim().is_empty()
            || self.description.trim().is_empty()
            || !self.config_schema.is_object()
        {
            return Err(ProviderError::InvalidInfo);
        }
        if self.protocol_version != PROVIDER_PROTOCOL_VERSION {
            return Err(ProviderError::UnsupportedProtocol {
                actual: self.protocol_version,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderLifecycleAction {
    Deploy,
    Inspect,
    Logs,
    Enable,
    Disable,
    Delete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleSupport {
    Supported,
    Unsupported,
}

#[must_use]
pub const fn lifecycle_support(action: ProviderLifecycleAction) -> LifecycleSupport {
    match action {
        ProviderLifecycleAction::Deploy => LifecycleSupport::Supported,
        ProviderLifecycleAction::Inspect
        | ProviderLifecycleAction::Logs
        | ProviderLifecycleAction::Enable
        | ProviderLifecycleAction::Disable
        | ProviderLifecycleAction::Delete => LifecycleSupport::Unsupported,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("provider invocation timed out")]
    Timeout,
    #[error("provider output exceeded its byte limit")]
    OutputLimit,
    #[error("provider response was not valid JSON")]
    InvalidJson,
    #[error("provider info response is incomplete or invalid")]
    InvalidInfo,
    #[error("provider identity {actual} does not match trusted ID {expected}")]
    IdentityMismatch { expected: String, actual: String },
    #[error("provider bytes changed after trust approval")]
    TrustBinding,
    #[error("unsupported provider protocol version {actual}")]
    UnsupportedProtocol { actual: u32 },
    #[error("provider returned an error: {0}")]
    Provider(String),
    #[error("deploy response is missing agent_id")]
    MissingAgentId,
    #[error("secret-bearing payload construction failed before provider invocation")]
    Payload,
    #[error("provider config schema uses an unsupported construct: {0}")]
    UnsupportedSchema(String),
    #[error("provider config does not satisfy negotiated schema: {0}")]
    InvalidConfig(String),
}

#[derive(Clone, Debug)]
pub struct ProviderHostConfig {
    pub staging_directory: PathBuf,
    pub info_timeout: Duration,
    pub deploy_timeout: Duration,
    pub stdout_cap: usize,
    pub stderr_cap: usize,
}

pub struct ProviderHost {
    config: ProviderHostConfig,
}

impl ProviderHost {
    pub fn new(config: ProviderHostConfig) -> Result<Self, ProviderError> {
        if !config.staging_directory.is_absolute()
            || config.info_timeout.is_zero()
            || config.deploy_timeout.is_zero()
            || config.stdout_cap == 0
            || config.stderr_cap == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid provider host config",
            )
            .into());
        }
        fs::create_dir_all(&config.staging_directory)?;
        Ok(Self { config })
    }

    /// Stages trusted bytes and negotiates v1 without constructing or exposing
    /// a secret-bearing deploy request.
    pub fn negotiate(
        &self,
        candidate: &ProviderCandidate,
    ) -> Result<NegotiatedProvider, ProviderError> {
        let staged = StagedProvider::copy(
            &candidate.canonical_path,
            &self.config.staging_directory,
            &candidate.sha256,
        )?;
        let request = serde_json::json!({
            "op": "info",
            "request_id": uuid::Uuid::now_v7().to_string(),
        });
        let response = invoke(
            &staged.path,
            &request,
            self.config.info_timeout,
            self.config.stdout_cap,
            self.config.stderr_cap,
        )?;
        let info: ProviderInfo =
            serde_json::from_value(response).map_err(|_| ProviderError::InvalidInfo)?;
        info.validate()?;
        if info.name != candidate.id {
            return Err(ProviderError::IdentityMismatch {
                expected: candidate.id.clone(),
                actual: info.name,
            });
        }
        validate_supported_schema(&info.config_schema)?;
        Ok(NegotiatedProvider {
            id: candidate.id.clone(),
            info,
            staged_sha256: hex_digest(&staged.sha256),
            staged,
            deploy_timeout: self.config.deploy_timeout,
            stdout_cap: self.config.stdout_cap,
            stderr_cap: self.config.stderr_cap,
        })
    }
}

pub struct NegotiatedProvider {
    pub id: String,
    pub info: ProviderInfo,
    pub staged_sha256: String,
    staged: StagedProvider,
    deploy_timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
}

impl NegotiatedProvider {
    /// The closure is intentionally invoked only after trust and protocol
    /// negotiation have succeeded on the exact immutable staged bytes.
    pub fn deploy<F>(&self, build_payload: F) -> Result<String, ProviderError>
    where
        F: FnOnce() -> Result<(serde_json::Value, serde_json::Value), ProviderError>,
    {
        let (agent, provider_config) = build_payload().map_err(|_| ProviderError::Payload)?;
        validate_provider_config(&provider_config)?;
        validate_config_against_schema(&self.info.config_schema, &provider_config)?;
        let request = serde_json::json!({
            "op": "deploy",
            "request_id": uuid::Uuid::now_v7().to_string(),
            "agent": agent,
            "provider_config": provider_config,
        });
        let response = invoke(
            &self.staged.path,
            &request,
            self.deploy_timeout,
            self.stdout_cap,
            self.stderr_cap,
        )?;
        response
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or(ProviderError::MissingAgentId)
    }
}

fn validate_provider_config(config: &serde_json::Value) -> Result<(), ProviderError> {
    let object = config.as_object().ok_or(ProviderError::Payload)?;
    if object.len() > 20
        || serde_json::to_vec(config)
            .map_err(|_| ProviderError::Payload)?
            .len()
            > 65_536
    {
        return Err(ProviderError::Payload);
    }
    for (key, value) in object {
        if secret_shaped_key(key) || value.is_array() || value.is_object() {
            return Err(ProviderError::Payload);
        }
    }
    Ok(())
}

fn validate_supported_schema(schema: &serde_json::Value) -> Result<(), ProviderError> {
    let object = schema
        .as_object()
        .ok_or_else(|| ProviderError::UnsupportedSchema("root must be an object".into()))?;
    for key in object.keys() {
        if ![
            "type",
            "properties",
            "required",
            "additionalProperties",
            "title",
            "description",
        ]
        .contains(&key.as_str())
        {
            return Err(ProviderError::UnsupportedSchema(key.clone()));
        }
    }
    if object.get("type").is_some_and(|value| value != "object") {
        return Err(ProviderError::UnsupportedSchema("root type".into()));
    }
    if let Some(required) = object.get("required") {
        let values = required
            .as_array()
            .ok_or_else(|| ProviderError::UnsupportedSchema("required".into()))?;
        if values.iter().any(|value| !value.is_string()) {
            return Err(ProviderError::UnsupportedSchema("required".into()));
        }
    }
    if let Some(additional) = object.get("additionalProperties") {
        if !additional.is_boolean() {
            return Err(ProviderError::UnsupportedSchema(
                "additionalProperties".into(),
            ));
        }
    }
    let Some(properties) = object.get("properties") else {
        return Ok(());
    };
    let properties = properties
        .as_object()
        .ok_or_else(|| ProviderError::UnsupportedSchema("properties".into()))?;
    for property in properties.values() {
        let property = property
            .as_object()
            .ok_or_else(|| ProviderError::UnsupportedSchema("property".into()))?;
        for key in property.keys() {
            if ![
                "type",
                "title",
                "description",
                "default",
                "enum",
                "minimum",
                "maximum",
            ]
            .contains(&key.as_str())
            {
                return Err(ProviderError::UnsupportedSchema(key.clone()));
            }
        }
        if let Some(kind) = property.get("type").and_then(serde_json::Value::as_str) {
            if !["string", "number", "integer", "boolean", "null"].contains(&kind) {
                return Err(ProviderError::UnsupportedSchema(format!(
                    "property type {kind}"
                )));
            }
        } else {
            return Err(ProviderError::UnsupportedSchema("property type".into()));
        }
        if property.get("enum").is_some_and(|value| !value.is_array()) {
            return Err(ProviderError::UnsupportedSchema("enum".into()));
        }
        if property
            .get("minimum")
            .is_some_and(|value| !value.is_number())
            || property
                .get("maximum")
                .is_some_and(|value| !value.is_number())
        {
            return Err(ProviderError::UnsupportedSchema("numeric bound".into()));
        }
    }
    Ok(())
}

fn validate_config_against_schema(
    schema: &serde_json::Value,
    config: &serde_json::Value,
) -> Result<(), ProviderError> {
    validate_supported_schema(schema)?;
    let schema = schema.as_object().expect("validated schema object");
    let config = config
        .as_object()
        .ok_or_else(|| ProviderError::InvalidConfig("must be an object".into()))?;
    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        for key in required.iter().filter_map(serde_json::Value::as_str) {
            if !config.contains_key(key) {
                return Err(ProviderError::InvalidConfig(format!(
                    "missing required field {key}"
                )));
            }
        }
    }
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object);
    if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false)) {
        for key in config.keys() {
            if properties.is_none_or(|properties| !properties.contains_key(key)) {
                return Err(ProviderError::InvalidConfig(format!("unknown field {key}")));
            }
        }
    }
    for (key, value) in config {
        let Some(property) = properties
            .and_then(|properties| properties.get(key))
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        let valid_type = match property.get("type").and_then(serde_json::Value::as_str) {
            Some("string") => value.is_string(),
            Some("number") => value.is_number(),
            Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
            Some("boolean") => value.is_boolean(),
            Some("null") => value.is_null(),
            _ => false,
        };
        if !valid_type {
            return Err(ProviderError::InvalidConfig(format!(
                "field {key} has wrong type"
            )));
        }
        if let Some(allowed) = property.get("enum").and_then(serde_json::Value::as_array) {
            if !allowed.contains(value) {
                return Err(ProviderError::InvalidConfig(format!(
                    "field {key} is not an allowed value"
                )));
            }
        }
        if let Some(number) = value.as_f64() {
            if property
                .get("minimum")
                .and_then(serde_json::Value::as_f64)
                .is_some_and(|minimum| number < minimum)
                || property
                    .get("maximum")
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(|maximum| number > maximum)
            {
                return Err(ProviderError::InvalidConfig(format!(
                    "field {key} is outside bounds"
                )));
            }
        }
    }
    Ok(())
}

fn secret_shaped_key(key: &str) -> bool {
    let mut words = Vec::new();
    let mut current = String::new();
    let characters: Vec<char> = key.chars().collect();
    for (index, character) in characters.iter().copied().enumerate() {
        if matches!(character, '_' | '-' | '.') {
            if !current.is_empty() {
                words.push(current.to_ascii_lowercase());
                current.clear();
            }
            continue;
        }
        if character.is_uppercase() {
            let previous_lower = current.chars().last().is_some_and(char::is_lowercase);
            let acronym_end = current.chars().last().is_some_and(char::is_uppercase)
                && characters
                    .get(index + 1)
                    .is_some_and(|next| next.is_lowercase());
            if previous_lower || acronym_end {
                words.push(current.to_ascii_lowercase());
                current.clear();
            }
        }
        current.push(character);
    }
    if !current.is_empty() {
        words.push(current.to_ascii_lowercase());
    }
    ["secret", "password", "token", "credential", "key"]
        .iter()
        .any(|forbidden| words.iter().any(|word| word == forbidden))
}

struct StagedProvider {
    directory: PathBuf,
    path: PathBuf,
    _guard: File,
    sha256: [u8; 32],
}

impl StagedProvider {
    fn copy(source: &Path, root: &Path, trusted_sha256: &[u8; 32]) -> Result<Self, ProviderError> {
        let directory = root.join(format!("provider-{}", uuid::Uuid::now_v7().simple()));
        fs::create_dir(&directory)?;
        #[cfg(unix)]
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let path = directory.join("provider");
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500))?;
        drop(output);
        let sha256 = sha256_reader(File::open(&path)?)?;
        if &sha256 != trusted_sha256 {
            return Err(ProviderError::TrustBinding);
        }
        let guard = File::open(&path)?;
        Ok(Self {
            directory,
            path,
            _guard: guard,
            sha256,
        })
    }
}

impl Drop for StagedProvider {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn invoke(
    binary: &Path,
    request: &serde_json::Value,
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<serde_json::Value, ProviderError> {
    let request_bytes = serde_json::to_vec(request).map_err(|_| ProviderError::InvalidJson)?;
    let secrets = collect_strings(request);
    let mut command = Command::new(binary);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = spawn_provider(&mut command)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&request_bytes)?;
        stdin.write_all(b"\n")?;
    }
    let stdout = spawn_bounded_reader(child.stdout.take(), stdout_cap);
    let stderr = spawn_bounded_reader(child.stderr.take(), stderr_cap);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            terminate_provider(&mut child);
            return Err(ProviderError::Timeout);
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| ProviderError::Timeout)??;
    let stderr = stderr
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| ProviderError::Timeout)??;
    if !status.success() {
        return Err(ProviderError::Provider(redact(
            &String::from_utf8_lossy(&stderr),
            &secrets,
        )));
    }
    let response: serde_json::Value =
        serde_json::from_slice(&stdout).map_err(|_| ProviderError::InvalidJson)?;
    if response.get("ok") == Some(&serde_json::Value::Bool(false)) {
        let message = response
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown provider error");
        return Err(ProviderError::Provider(redact(message, &secrets)));
    }
    Ok(response)
}

fn spawn_provider(command: &mut Command) -> io::Result<std::process::Child> {
    const MAX_EXECUTABLE_BUSY_RETRIES: usize = 5;
    for attempt in 0..=MAX_EXECUTABLE_BUSY_RETRIES {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && attempt < MAX_EXECUTABLE_BUSY_RETRIES =>
            {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded provider spawn loop always returns")
}

fn spawn_bounded_reader<R>(
    reader: Option<R>,
    cap: usize,
) -> mpsc::Receiver<Result<Vec<u8>, ProviderError>>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = match reader {
            Some(reader) => {
                let mut bytes = Vec::new();
                match reader.take((cap + 1) as u64).read_to_end(&mut bytes) {
                    Ok(_) if bytes.len() <= cap => Ok(bytes),
                    Ok(_) => Err(ProviderError::OutputLimit),
                    Err(error) => Err(error.into()),
                }
            }
            None => Ok(Vec::new()),
        };
        let _ = sender.send(result);
    });
    receiver
}

fn terminate_provider(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn collect_strings(value: &serde_json::Value) -> Vec<String> {
    let mut values = Vec::new();
    fn visit(value: &serde_json::Value, values: &mut Vec<String>) {
        match value {
            serde_json::Value::String(value) if value.len() >= 4 => values.push(value.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|value| visit(value, values)),
            serde_json::Value::Object(object) => {
                object.values().for_each(|value| visit(value, values))
            }
            _ => {}
        }
    }
    visit(value, &mut values);
    values
}

fn redact(input: &str, secrets: &[String]) -> String {
    secrets.iter().fold(input.to_owned(), |text, secret| {
        text.replace(secret, "[REDACTED]")
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::{cell::Cell, os::unix::fs::PermissionsExt};

    use super::*;

    fn candidate(directory: &Path, protocol: u32) -> ProviderCandidate {
        let path = directory.join("buzz-backend-reference");
        let script = format!(
            r#"#!/bin/sh
request=$(cat)
case "$request" in
  *'"op":"info"'*) printf '%s\n' '{{"ok":true,"name":"reference","version":"1.0.0","protocol_version":{protocol},"description":"test provider","config_schema":{{}}}}' ;;
  *) printf '%s\n' '{{"ok":true,"agent_id":"reference-agent-1"}}' ;;
esac
"#
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let sha256 = sha256_reader(File::open(&path).unwrap()).unwrap();
        ProviderCandidate {
            id: "reference".into(),
            canonical_path: path.canonicalize().unwrap(),
            sha256,
        }
    }

    fn host(directory: &Path) -> ProviderHost {
        ProviderHost::new(ProviderHostConfig {
            staging_directory: directory.join("stage"),
            // Leave enough margin for process startup when the full test suite is
            // concurrently compiling native TLS dependencies on a small runner.
            info_timeout: Duration::from_secs(10),
            deploy_timeout: Duration::from_secs(10),
            stdout_cap: 64 * 1024,
            stderr_cap: 4096,
        })
        .unwrap()
    }

    #[test]
    fn negotiates_before_building_secret_payload_and_accepts_desktop_fixture() {
        let directory = tempfile::tempdir().unwrap();
        let host = host(directory.path());
        let provider = host.negotiate(&candidate(directory.path(), 1)).unwrap();
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/provider-wire/deploy-full-launch.request.json"
        ))
        .unwrap();
        let built = Cell::new(false);
        let agent_id = provider
            .deploy(|| {
                built.set(true);
                Ok((fixture["agent"].clone(), fixture["provider_config"].clone()))
            })
            .unwrap();
        assert!(built.get());
        assert_eq!(agent_id, "reference-agent-1");
        assert_eq!(provider.staged_sha256.len(), 64);
    }

    #[test]
    fn incompatible_info_fails_before_any_payload_can_be_built() {
        let directory = tempfile::tempdir().unwrap();
        let error = host(directory.path())
            .negotiate(&candidate(directory.path(), 2))
            .err()
            .unwrap();
        assert!(matches!(
            error,
            ProviderError::UnsupportedProtocol { actual: 2 }
        ));
    }

    #[test]
    fn negotiated_name_must_match_trusted_provider_id() {
        let directory = tempfile::tempdir().unwrap();
        let mut provider = candidate(directory.path(), 1);
        provider.id = "different".into();
        assert!(matches!(
            host(directory.path()).negotiate(&provider),
            Err(ProviderError::IdentityMismatch { .. })
        ));
    }

    #[test]
    fn unsupported_lifecycle_is_explicit() {
        assert_eq!(
            lifecycle_support(ProviderLifecycleAction::Deploy),
            LifecycleSupport::Supported
        );
        for action in [
            ProviderLifecycleAction::Inspect,
            ProviderLifecycleAction::Logs,
            ProviderLifecycleAction::Enable,
            ProviderLifecycleAction::Disable,
            ProviderLifecycleAction::Delete,
        ] {
            assert_eq!(lifecycle_support(action), LifecycleSupport::Unsupported);
        }
    }

    #[test]
    fn provider_config_rejects_secret_shaped_keys_in_all_supported_styles() {
        for key in ["api_key", "clientSecret", "access-token", "credential"] {
            assert!(matches!(
                validate_provider_config(&serde_json::json!({ key: "value" })),
                Err(ProviderError::Payload)
            ));
        }
        validate_provider_config(&serde_json::json!({ "keyboard": "qwerty" })).unwrap();
    }

    #[test]
    fn negotiated_schema_rejects_missing_wrong_and_unsupported_config() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "namespace": {"type": "string"},
                "replicas": {"type": "integer", "minimum": 1}
            },
            "required": ["namespace"],
            "additionalProperties": false
        });
        validate_supported_schema(&schema).unwrap();
        assert!(matches!(
            validate_config_against_schema(&schema, &serde_json::json!({})),
            Err(ProviderError::InvalidConfig(_))
        ));
        assert!(matches!(
            validate_config_against_schema(&schema, &serde_json::json!({"namespace": 4})),
            Err(ProviderError::InvalidConfig(_))
        ));
        assert!(matches!(
            validate_supported_schema(&serde_json::json!({"oneOf": []})),
            Err(ProviderError::UnsupportedSchema(_))
        ));
    }

    #[test]
    fn source_replacement_after_trust_is_refused_before_execution() {
        let directory = tempfile::tempdir().unwrap();
        let provider = candidate(directory.path(), 1);
        fs::write(&provider.canonical_path, "#!/bin/sh\necho compromised\n").unwrap();
        fs::set_permissions(&provider.canonical_path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            host(directory.path()).negotiate(&provider),
            Err(ProviderError::TrustBinding)
        ));
    }
}
