use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct DeployEnvelope {
    op: String,
    request_id: String,
    agent: AgentPayload,
    provider_config: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AgentPayload {
    relay_url: String,
    private_key_nsec: String,
    auth_tag: Option<String>,
    respond_to: Option<String>,
    launch: LaunchBlock,
}

#[derive(Debug, Deserialize)]
struct LaunchBlock {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    policy_env: BTreeMap<String, String>,
    owner_pubkey: String,
}

#[test]
fn accepts_the_pinned_desktop_full_deploy_fixture() {
    let request: DeployEnvelope = serde_json::from_str(include_str!(
        "fixtures/provider-wire/deploy-full-launch.request.json",
    ))
    .expect("pinned Desktop provider fixture must parse");

    assert_eq!(request.op, "deploy");
    assert_eq!(request.request_id, "req-6");
    assert_eq!(request.agent.relay_url, "wss://relay.example");
    assert!(request.agent.private_key_nsec.starts_with("nsec1"));
    assert_eq!(request.agent.auth_tag.as_deref(), Some("tag-1"));
    assert_eq!(request.agent.respond_to.as_deref(), Some("allowlist"));
    assert_eq!(request.agent.launch.command, "goose");
    assert_eq!(request.agent.launch.args, ["acp"]);
    assert_eq!(request.agent.launch.env["GOOSE_MODEL"], "gpt-5");
    assert_eq!(request.agent.launch.policy_env["BUZZ_ACP_AGENTS"], "10");
    assert_eq!(request.agent.launch.owner_pubkey.len(), 64);
    assert_eq!(request.provider_config["namespace"], "buzz-agents-test");
}
