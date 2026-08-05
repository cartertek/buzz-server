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
