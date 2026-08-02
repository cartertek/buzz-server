use buzz_sdk::nip_oa::{compute_auth_tag, parse_auth_tag, verify_auth_tag};
use nostr::{Keys, PublicKey};
use serde::Deserialize;

#[derive(Deserialize)]
struct NipOaVector {
    buzz_revision: String,
    owner_pubkey: String,
    agent_pubkey: String,
    conditions: String,
    signature: String,
}

fn vector() -> NipOaVector {
    serde_json::from_str(include_str!("fixtures/nip-oa-spec.json"))
        .expect("checked-in NIP-OA fixture must parse")
}

#[test]
fn pinned_buzz_sdk_verifies_the_buzz_spec_vector() {
    let fixture = vector();
    assert_eq!(
        fixture.buzz_revision,
        "7ff5fc31895efe6265a379d01637c8ee301872e5"
    );
    let agent = PublicKey::from_hex(&fixture.agent_pubkey).expect("fixture agent key");
    let auth_tag = serde_json::json!([
        "auth",
        fixture.owner_pubkey,
        fixture.conditions,
        fixture.signature,
    ])
    .to_string();

    let owner = verify_auth_tag(&auth_tag, &agent).expect("Buzz spec vector must verify");
    assert_eq!(
        owner.to_hex(),
        "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
    );
    let parsed = parse_auth_tag(&auth_tag).expect("verified tag must parse for harness use");
    assert_eq!(parsed.as_slice()[0], "auth");
}

#[test]
fn server_signing_path_is_the_same_shared_sdk_path_used_by_desktop() {
    let owner = Keys::parse("0000000000000000000000000000000000000000000000000000000000000001")
        .expect("fixed disposable owner key");
    let agent_keys =
        Keys::parse("0000000000000000000000000000000000000000000000000000000000000002")
            .expect("fixed disposable agent key");
    let agent = agent_keys.public_key();

    let auth_tag = compute_auth_tag(&owner, &agent, "")
        .expect("shared Buzz SDK must authorize a distinct disposable agent");
    let recovered =
        verify_auth_tag(&auth_tag, &agent).expect("shared Buzz SDK must verify what it creates");

    assert_eq!(recovered, owner.public_key());
    assert_eq!(
        parse_auth_tag(&auth_tag).expect("tag shape").as_slice()[2],
        ""
    );
}

#[test]
fn shared_sdk_rejects_noncanonical_conditions() {
    let owner = Keys::parse("0000000000000000000000000000000000000000000000000000000000000001")
        .expect("fixed disposable owner key");
    let agent = Keys::parse("0000000000000000000000000000000000000000000000000000000000000002")
        .expect("fixed disposable agent key")
        .public_key();

    assert!(compute_auth_tag(&owner, &agent, "kind=01").is_err());
}
