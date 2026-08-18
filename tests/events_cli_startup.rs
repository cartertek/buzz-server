use std::{process::Command, thread, time::Duration};

#[test]
fn subscribe_initializes_tls_before_connecting() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_buzz-events"))
        .args(["subscribe", "--filter", "{}"])
        .env("BUZZ_RELAY_URL", "wss://127.0.0.1:1")
        .env(
            "BUZZ_PRIVATE_KEY",
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .spawn()
        .expect("spawn buzz-events");

    thread::sleep(Duration::from_millis(250));
    if child.try_wait().expect("poll buzz-events").is_none() {
        child.kill().expect("stop buzz-events");
    }
    let output = child
        .wait_with_output()
        .expect("collect buzz-events output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Could not automatically determine the process-level CryptoProvider")
            && !stderr.contains("panicked at"),
        "subscribe panicked before initializing Rustls: {stderr}"
    );
}
