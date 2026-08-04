#![cfg(unix)]

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use buzz_server::{
    provider::{ProviderError, ProviderHost, ProviderHostConfig, ProviderLifecycleAction},
    provider_discovery::{discover_trusted, ProviderTrustPolicy},
    provider_reconcile::{
        ProviderDeploymentCoordinator, ProviderDeploymentReceipt,
        ProviderDeploymentReceiptRepository,
    },
};

struct ExactPath(PathBuf);

impl ProviderTrustPolicy for ExactPath {
    fn is_trusted(&self, _id: &str, path: &Path, _sha256: &[u8; 32]) -> bool {
        path == self.0
    }
}

#[derive(Default)]
struct MemoryReceipts(Mutex<Option<ProviderDeploymentReceipt>>);

impl ProviderDeploymentReceiptRepository for MemoryReceipts {
    type Error = ();

    fn get(&self, _request_id: &str) -> Result<Option<ProviderDeploymentReceipt>, Self::Error> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn put_if_absent(
        &self,
        receipt: &ProviderDeploymentReceipt,
    ) -> Result<ProviderDeploymentReceipt, Self::Error> {
        let mut slot = self.0.lock().unwrap();
        Ok(slot.get_or_insert_with(|| receipt.clone()).clone())
    }
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn host(root: &Path) -> ProviderHost {
    ProviderHost::new(ProviderHostConfig {
        staging_directory: root.join("stage"),
        info_timeout: Duration::from_secs(10),
        deploy_timeout: Duration::from_secs(10),
        stdout_cap: 64 * 1024,
        stderr_cap: 4096,
        environment: BTreeMap::from([("KUBECONFIG".into(), "/nonexistent/fixture".into())]),
    })
    .unwrap()
}

fn discover_one(path: &Path) -> buzz_server::provider_discovery::ProviderCandidate {
    let canonical = path.canonicalize().unwrap();
    let mut found = discover_trusted(
        &[path.parent().unwrap().to_path_buf()],
        &ExactPath(canonical),
    )
    .unwrap();
    assert_eq!(found.len(), 1);
    found.pop().unwrap()
}

fn fixture(name: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/provider-wire")
        .join(name);
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn vendored_upstream_fixture_corpus_is_complete() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-wire");
    let actual: BTreeSet<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    let expected = BTreeSet::from([
        "README.md".into(),
        "deploy-full-launch.request.json".into(),
        "deploy-no-owner.request.json".into(),
        "deploy-no-owner.response.json".into(),
        "deploy-relay-mesh-padded.request.json".into(),
        "deploy-relay-mesh-padded.response.json".into(),
        "deploy-relay-mesh.request.json".into(),
        "deploy-relay-mesh.response.json".into(),
        "deploy-tag-image.request.json".into(),
        "deploy-tag-image.response.json".into(),
        "info.request.json".into(),
    ]);
    assert_eq!(actual, expected);
}

#[test]
fn fake_provider_passes_discovery_negotiation_deploy_and_lifecycle_contract() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("fake.log");
    let path = directory.path().join("buzz-backend-fake");
    executable(
        &path,
        &format!(
            r#"#!/bin/sh
request=$(cat)
printf 'ambient_private_key=%s\n' "${{BUZZ_PRIVATE_KEY-unset}}" >> '{}'
printf '%s\n' "$request" >> '{}'
case "$request" in
  *'"op":"info"'*) printf '%s\n' '{{"ok":true,"name":"fake","version":"1.0.0","protocol_version":1,"description":"fixture fake provider","config_schema":{{"type":"object","properties":{{}},"additionalProperties":false}},"capabilities":{{"lifecycle_protocol_version":1,"lifecycle_actions":["inspect"]}}}}' ;;
  *) printf '%s\n' '{{"ok":true,"agent_id":"fake-agent"}}' ;;
esac
"#,
            log.display(),
            log.display()
        ),
    );

    std::env::set_var("BUZZ_PRIVATE_KEY", "ambient-must-not-leak");
    let negotiated = host(directory.path()).negotiate(&discover_one(&path));
    std::env::remove_var("BUZZ_PRIVATE_KEY");
    let provider = negotiated.unwrap();
    let after_info = fs::read_to_string(&log).unwrap();
    assert!(after_info.contains("ambient_private_key=unset"));
    assert!(after_info.contains("\"op\":\"info\""));
    assert!(!after_info.contains("private_key_nsec"));
    let descriptor = serde_json::to_string(&provider.descriptor()).unwrap();
    assert!(descriptor.contains("config_schema"));
    assert!(!descriptor.contains("KUBECONFIG"));
    assert!(!descriptor.contains(path.to_string_lossy().as_ref()));

    provider
        .lifecycle(ProviderLifecycleAction::Inspect)
        .unwrap();

    assert!(matches!(
        provider.lifecycle(ProviderLifecycleAction::Delete),
        Err(ProviderError::UnsupportedLifecycle(
            ProviderLifecycleAction::Delete
        ))
    ));
    assert_eq!(fs::read_to_string(&log).unwrap(), after_info);

    let secret = "nsec1-secret-only-built-after-negotiation";
    let agent_id = provider
        .deploy(|| {
            Ok((
                serde_json::json!({"private_key_nsec": secret}),
                serde_json::json!({}),
            ))
        })
        .unwrap();
    assert_eq!(agent_id, "fake-agent");
    assert!(fs::read_to_string(log).unwrap().contains(secret));
}

#[test]
fn durable_provider_coordinator_replays_without_redeploy_or_secret_rebuild() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("deploy.log");
    let path = directory.path().join("buzz-backend-fake");
    executable(
        &path,
        &format!(
            r#"#!/bin/sh
request=$(cat)
case "$request" in
  *'"op":"info"'*) printf '%s\n' '{{"ok":true,"name":"fake","version":"1.0.0","protocol_version":1,"description":"fixture fake provider","config_schema":{{}}}}' ;;
  *) printf '%s\n' "$request" >> '{}'; printf '%s\n' '{{"ok":true,"agent_id":"fake-stable-agent"}}' ;;
esac
"#,
            log.display()
        ),
    );
    let provider = host(directory.path())
        .negotiate(&discover_one(&path))
        .unwrap();
    let receipts = MemoryReceipts::default();
    let coordinator = ProviderDeploymentCoordinator::new(&receipts);
    let builds = Cell::new(0);

    for _ in 0..2 {
        let receipt = coordinator
            .deploy_once("op-stable-1", &provider, || {
                builds.set(builds.get() + 1);
                Ok((
                    serde_json::json!({"private_key_nsec": "nsec1-only-after-info"}),
                    serde_json::json!({}),
                ))
            })
            .unwrap();
        assert_eq!(receipt.external_agent_id, "fake-stable-agent");
    }
    assert_eq!(builds.get(), 1);
    let log = fs::read_to_string(log).unwrap();
    assert_eq!(log.lines().count(), 1);
    assert!(log.contains("\"request_id\":\"op-stable-1\""));
}

#[test]
fn kubernetes_reference_contract_accepts_every_upstream_request_fixture() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("buzz-backend-kubernetes");
    executable(
        &path,
        r#"#!/bin/sh
request=$(cat)
case "$request" in
  *'"op":"info"'*) printf '%s\n' '{"ok":true,"name":"kubernetes","version":"1.0.0","protocol_version":1,"description":"Runs agents as pods in a Kubernetes cluster","config_schema":{"type":"object","properties":{"context":{"type":"string"},"namespace":{"type":"string","default":"buzz-agents-fixture"},"image":{"type":"string","default":"ghcr.io/block/buzz-sprig@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"inactivity_seconds":{"type":"number","default":3600}},"required":["namespace","image"]}}' ;;
  *'relay-mesh'*) printf '%s\n' '{"ok":false,"error":"deploy refused: this agent is configured for shared compute (relay-mesh), which runs on the relay rather than in a pod. Switch the agent to a local runtime before deploying it to Kubernetes."}' ;;
  *'buzz-sprig:latest'*) printf '%s\n' '{"ok":false,"error":"provider_config.image \"ghcr.io/block/buzz-sprig:latest\" is not digest-pinned: a tag is a mutable pointer, and this object runs with the agent'\''s private key. Use name@sha256:<64 hex chars>"}' ;;
  *'"auth_tag"'*|*'"owner_pubkey"'*) printf '%s\n' '{"ok":true,"agent_id":"buzz-agent-fixture"}' ;;
  *) printf '%s\n' '{"ok":false,"error":"deploy refused: neither auth_tag nor launch.owner_pubkey resolved — without an owner the agent cannot honor !shutdown"}' ;;
esac
"#,
    );

    let provider = host(directory.path())
        .negotiate(&discover_one(&path))
        .unwrap();
    assert_eq!(provider.id, "kubernetes");
    assert_eq!(provider.info.protocol_version, 1);

    for case in [
        "deploy-relay-mesh.request.json",
        "deploy-relay-mesh-padded.request.json",
        "deploy-tag-image.request.json",
        "deploy-no-owner.request.json",
    ] {
        let request = fixture(case);
        let expected_name = case.replace(".request.json", ".response.json");
        let expected = fixture(&expected_name);
        let error = provider
            .deploy(|| Ok((request["agent"].clone(), request["provider_config"].clone())))
            .unwrap_err();
        let ProviderError::Provider(actual) = error else {
            panic!("{case}: unexpected error: {error}");
        };
        assert_eq!(actual, expected["error"].as_str().unwrap(), "{case}");
    }

    let request = fixture("deploy-full-launch.request.json");
    let agent_id = provider
        .deploy(|| Ok((request["agent"].clone(), request["provider_config"].clone())))
        .unwrap();
    assert_eq!(agent_id, "buzz-agent-fixture");
}

#[test]
fn secret_shaped_explicit_provider_environment_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let result = ProviderHost::new(ProviderHostConfig {
        staging_directory: directory.path().join("stage"),
        info_timeout: Duration::from_secs(1),
        deploy_timeout: Duration::from_secs(1),
        stdout_cap: 1024,
        stderr_cap: 1024,
        environment: BTreeMap::from([("API_TOKEN".into(), "must-not-pass".into())]),
    });
    assert!(matches!(
        result,
        Err(ProviderError::SecretEnvironment(key)) if key == "API_TOKEN"
    ));
}
