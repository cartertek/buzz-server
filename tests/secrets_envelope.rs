use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn kms_envelope_round_trip_and_tamper_rejection() {
    let directory = tempfile::tempdir().unwrap();
    let aws = directory.path().join("aws");
    let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7u8; 32]);
    fs::write(
        &aws,
        format!(
            r#"#!/bin/sh
set -eu
case "$2" in
generate-data-key) printf '{{"Plaintext":"{key}","CiphertextBlob":"ZW5jcnlwdGVkLWtleQ=="}}' ;;
decrypt) printf '{{"Plaintext":"{key}"}}' ;;
*) exit 2 ;;
esac
"#
        ),
    )
    .unwrap();
    fs::set_permissions(&aws, fs::Permissions::from_mode(0o755)).unwrap();
    let input = directory.path().join("owner");
    let envelope = directory.path().join("owner.json");
    let output = directory.path().join("restored");
    fs::write(
        &input,
        b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    let binary = env!("CARGO_BIN_EXE_buzz-secretsctl");
    assert!(Command::new(binary)
        .args([
            "encrypt",
            "--kms-key-id",
            "test-key",
            "--input",
            input.to_str().unwrap(),
            "--output",
            envelope.to_str().unwrap(),
            "--aws-command",
            aws.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    assert!(Command::new(binary)
        .args([
            "decrypt",
            "--input",
            envelope.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--aws-command",
            aws.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    assert_eq!(fs::read(&input).unwrap(), fs::read(&output).unwrap());
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&envelope).unwrap()).unwrap();
    value["ciphertext"] = serde_json::Value::String("AAAA".into());
    fs::write(&envelope, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(!Command::new(binary)
        .args([
            "decrypt",
            "--input",
            envelope.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--aws-command",
            aws.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
}
