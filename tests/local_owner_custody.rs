use std::{fs, process::Command};

#[test]
fn restricted_file_fallback_round_trips_and_fails_closed_after_loss() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let key_file = dir.path().join("owner-secret");
    let marker = dir.path().join("owner-secret.keyring");
    let output = dir.path().join("runtime-secret");
    let keys = nostr::Keys::generate();
    let nsec = nostr::ToBech32::to_bech32(keys.secret_key()).unwrap();
    fs::write(&source, &nsec).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_buzz-secretsctl"))
        .env("BUZZ_DISABLE_SYSTEM_KEYRING", "1")
        .args([
            "persist",
            "--input",
            source.to_str().unwrap(),
            "--key-file",
            key_file.to_str().unwrap(),
            "--marker",
            marker.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!marker.exists());

    let status = Command::new(env!("CARGO_BIN_EXE_buzz-secretsctl"))
        .env("BUZZ_DISABLE_SYSTEM_KEYRING", "1")
        .args([
            "materialize",
            "--output",
            output.to_str().unwrap(),
            "--key-file",
            key_file.to_str().unwrap(),
            "--marker",
            marker.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read_to_string(&output).unwrap(), nsec);
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&output).unwrap().permissions())
            & 0o777,
        0o400
    );

    fs::remove_file(&key_file).unwrap();
    fs::write(&marker, "system-keyring\n").unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_buzz-secretsctl"))
        .env("BUZZ_DISABLE_SYSTEM_KEYRING", "1")
        .args([
            "materialize",
            "--output",
            output.to_str().unwrap(),
            "--key-file",
            key_file.to_str().unwrap(),
            "--marker",
            marker.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(!status.success());
}

#[test]
fn passphrase_backup_and_nip49_round_trip_and_reject_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let passphrase = dir.path().join("passphrase");
    let source = dir.path().join("source");
    let envelope = dir.path().join("backup.json");
    let restored = dir.path().join("restored");
    let ncryptsec = dir.path().join("identity.ncryptsec");
    let recovered = dir.path().join("recovered");
    fs::write(&passphrase, "correct horse battery staple\n").unwrap();
    fs::write(&source, b"portable server state").unwrap();

    let bin = env!("CARGO_BIN_EXE_buzz-secretsctl");
    assert!(Command::new(bin)
        .args([
            "encrypt-passphrase",
            "--input",
            source.to_str().unwrap(),
            "--output",
            envelope.to_str().unwrap(),
            "--passphrase-file",
            passphrase.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    assert!(Command::new(bin)
        .args([
            "decrypt-passphrase",
            "--input",
            envelope.to_str().unwrap(),
            "--output",
            restored.to_str().unwrap(),
            "--passphrase-file",
            passphrase.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    assert_eq!(fs::read(&restored).unwrap(), b"portable server state");

    let keys = nostr::Keys::generate();
    let nsec = nostr::ToBech32::to_bech32(keys.secret_key()).unwrap();
    fs::write(&source, &nsec).unwrap();
    assert!(Command::new(bin)
        .args([
            "export-nip49",
            "--input",
            source.to_str().unwrap(),
            "--output",
            ncryptsec.to_str().unwrap(),
            "--passphrase-file",
            passphrase.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    assert!(fs::read_to_string(&ncryptsec)
        .unwrap()
        .starts_with("ncryptsec1"));
    assert!(Command::new(bin)
        .args([
            "import-nip49",
            "--input",
            ncryptsec.to_str().unwrap(),
            "--output",
            recovered.to_str().unwrap(),
            "--passphrase-file",
            passphrase.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    assert_eq!(fs::read_to_string(&recovered).unwrap(), nsec);

    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&envelope).unwrap()).unwrap();
    value["ciphertext"] = serde_json::Value::String("AAAA".into());
    fs::write(&envelope, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(!Command::new(bin)
        .args([
            "decrypt-passphrase",
            "--input",
            envelope.to_str().unwrap(),
            "--output",
            restored.to_str().unwrap(),
            "--passphrase-file",
            passphrase.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
}
