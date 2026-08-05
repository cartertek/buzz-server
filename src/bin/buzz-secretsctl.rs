use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const VERSION: u8 = 1;
const MAX_SECRET_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    version: u8,
    kms_key_id: String,
    encrypted_data_key: String,
    nonce: String,
    ciphertext: String,
    plaintext_sha256: String,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("encrypt") => encrypt_command(&args[1..]),
        Some("decrypt") => decrypt_command(&args[1..]),
        Some("fingerprint") => fingerprint_command(&args[1..]),
        _ => Err(usage()),
    }
}

fn encrypt_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let output = required(args, "--output")?;
    let key_id = required(args, "--kms-key-id")?;
    let aws = optional(args, "--aws-command").unwrap_or("aws");
    let plaintext = Zeroizing::new(read_bounded(Path::new(input))?);
    let generated = run_aws(
        aws,
        &[
            "kms",
            "generate-data-key",
            "--key-id",
            key_id,
            "--key-spec",
            "AES_256",
            "--output",
            "json",
        ],
    )?;
    let value: serde_json::Value =
        serde_json::from_slice(&generated).map_err(|e| format!("invalid KMS response: {e}"))?;
    let data_key = Zeroizing::new(decode_field(&value, "Plaintext")?);
    if data_key.len() != 32 {
        return Err("KMS returned a data key with the wrong length".into());
    }
    let encrypted_data_key = value
        .get("CiphertextBlob")
        .and_then(|v| v.as_str())
        .ok_or("KMS response omitted CiphertextBlob")?;
    let nonce_bytes = random_nonce()?;
    let cipher = Aes256Gcm::new_from_slice(&data_key).map_err(|_| "invalid data key")?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_ref())
        .map_err(|_| "encryption failed")?;
    let envelope = Envelope {
        version: VERSION,
        kms_key_id: key_id.to_owned(),
        encrypted_data_key: encrypted_data_key.to_owned(),
        nonce: STANDARD.encode(nonce_bytes),
        ciphertext: STANDARD.encode(ciphertext),
        plaintext_sha256: format!("{:x}", Sha256::digest(&plaintext[..])),
    };
    atomic_write(
        Path::new(output),
        &serde_json::to_vec_pretty(&envelope).map_err(|e| e.to_string())?,
        0o600,
    )
}

fn decrypt_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let output = required(args, "--output")?;
    let aws = optional(args, "--aws-command").unwrap_or("aws");
    let envelope: Envelope = serde_json::from_slice(&read_bounded(Path::new(input))?)
        .map_err(|e| format!("invalid envelope: {e}"))?;
    if envelope.version != VERSION {
        return Err("unsupported envelope version".into());
    }
    let encrypted_key = STANDARD
        .decode(&envelope.encrypted_data_key)
        .map_err(|_| "invalid encrypted data key")?;
    let temporary =
        env::temp_dir().join(format!("buzz-kms-{}.blob", uuid::Uuid::now_v7().simple()));
    atomic_write(&temporary, &encrypted_key, 0o600)?;
    let result = run_aws(
        aws,
        &[
            "kms",
            "decrypt",
            "--ciphertext-blob",
            &format!("fileb://{}", temporary.display()),
            "--key-id",
            &envelope.kms_key_id,
            "--output",
            "json",
        ],
    );
    let _ = fs::remove_file(&temporary);
    let value: serde_json::Value =
        serde_json::from_slice(&result?).map_err(|e| format!("invalid KMS response: {e}"))?;
    let data_key = Zeroizing::new(decode_field(&value, "Plaintext")?);
    if data_key.len() != 32 {
        return Err("KMS returned a data key with the wrong length".into());
    }
    let nonce = STANDARD
        .decode(&envelope.nonce)
        .map_err(|_| "invalid nonce")?;
    if nonce.len() != 12 {
        return Err("invalid nonce length".into());
    }
    let ciphertext = STANDARD
        .decode(&envelope.ciphertext)
        .map_err(|_| "invalid ciphertext")?;
    let cipher = Aes256Gcm::new_from_slice(&data_key).map_err(|_| "invalid data key")?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| "decryption authentication failed")?,
    );
    let digest = format!("{:x}", Sha256::digest(&plaintext[..]));
    if digest != envelope.plaintext_sha256 {
        return Err("plaintext digest mismatch".into());
    }
    atomic_write(Path::new(output), plaintext.as_ref(), 0o400)
}

fn fingerprint_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let bytes = Zeroizing::new(read_bounded(Path::new(input))?);
    println!("sha256:{:x}", Sha256::digest(&bytes[..]));
    Ok(())
}

fn run_aws(command: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|e| format!("cannot execute KMS command: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "KMS command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn decode_field(value: &serde_json::Value, field: &str) -> Result<Vec<u8>, String> {
    STANDARD
        .decode(
            value
                .get(field)
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("KMS response omitted {field}"))?,
        )
        .map_err(|_| format!("KMS returned invalid base64 in {field}"))
}

fn random_nonce() -> Result<[u8; 12], String> {
    use std::io::Read;
    let mut nonce = [0u8; 12];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut nonce))
        .map_err(|e| e.to_string())?;
    Ok(nonce)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if metadata.len() > MAX_SECRET_BYTES as u64 {
        return Err("input exceeds maximum supported size".into());
    }
    fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let parent = path.parent().ok_or("output path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary: PathBuf = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("secret"),
        uuid::Uuid::now_v7().simple()
    ));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
    fs::rename(&temporary, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn required<'a>(args: &'a [String], name: &str) -> Result<&'a str, String> {
    optional(args, name).ok_or_else(|| format!("missing {name}\n{}", usage()))
}
fn optional<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}
fn usage() -> String {
    "usage: buzz-secretsctl encrypt --kms-key-id KEY --input FILE --output FILE [--aws-command PATH]\n       buzz-secretsctl decrypt --input FILE --output FILE [--aws-command PATH]\n       buzz-secretsctl fingerprint --input FILE".into()
}
