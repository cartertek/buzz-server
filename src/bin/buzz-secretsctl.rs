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
use nostr::nips::nip49::{EncryptedSecretKey, KeySecurity};
use nostr::{FromBech32, Keys, ToBech32};
use scrypt::{scrypt, Params as ScryptParams};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const VERSION: u8 = 1;
const PASSPHRASE_VERSION: u8 = 1;
const SCRYPT_LOG_N: u8 = 18;
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

#[derive(Debug, Serialize, Deserialize)]
struct PassphraseEnvelope {
    version: u8,
    kdf: String,
    log_n: u8,
    salt: String,
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
        Some("public-key") => public_key_command(&args[1..]),
        Some("public-id") => public_id_command(&args[1..]),
        Some("encrypt-passphrase") => encrypt_passphrase_command(&args[1..]),
        Some("decrypt-passphrase") => decrypt_passphrase_command(&args[1..]),
        Some("export-nip49") => export_nip49_command(&args[1..]),
        Some("import-nip49") => import_nip49_command(&args[1..]),
        Some("persist") => persist_command(&args[1..]),
        Some("materialize") => materialize_command(&args[1..]),
        Some("clear-local") => clear_local_command(&args[1..]),
        _ => Err(usage()),
    }
}

fn public_key_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let secret = Zeroizing::new(
        String::from_utf8(read_bounded(Path::new(input))?)
            .map_err(|_| "Nostr secret is not valid UTF-8")?,
    );
    let keys = Keys::parse(secret.trim()).map_err(|_| "input is not a valid Nostr secret")?;
    println!("{}", keys.public_key().to_hex());
    Ok(())
}

fn public_id_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let secret = Zeroizing::new(
        String::from_utf8(read_bounded(Path::new(input))?)
            .map_err(|_| "Nostr secret is not valid UTF-8")?,
    );
    let keys = Keys::parse(secret.trim()).map_err(|_| "input is not a valid Nostr secret")?;
    println!(
        "{}",
        keys.public_key().to_bech32().map_err(|e| e.to_string())?
    );
    Ok(())
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

fn passphrase(args: &[String]) -> Result<Zeroizing<String>, String> {
    let path = required(args, "--passphrase-file")?;
    let value =
        fs::read_to_string(path).map_err(|e| format!("cannot read passphrase file: {e}"))?;
    let trimmed = value.trim_end_matches(['\r', '\n']).to_owned();
    if trimmed.len() < 12 {
        return Err("backup passphrase must be at least 12 characters".into());
    }
    Ok(Zeroizing::new(trimmed))
}

fn derive_passphrase_key(
    password: &str,
    salt: &[u8],
    log_n: u8,
) -> Result<Zeroizing<Vec<u8>>, String> {
    if log_n > SCRYPT_LOG_N {
        return Err("unsupported backup KDF cost".into());
    }
    let params = ScryptParams::new(log_n, 8, 1, 32).map_err(|e| e.to_string())?;
    let mut key = Zeroizing::new(vec![0u8; 32]);
    scrypt(password.as_bytes(), salt, &params, &mut key).map_err(|e| e.to_string())?;
    Ok(key)
}

fn encrypt_passphrase_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let output = required(args, "--output")?;
    let password = passphrase(args)?;
    let plaintext = Zeroizing::new(read_bounded(Path::new(input))?);
    let mut salt = [0u8; 16];
    fill_random(&mut salt)?;
    let key = derive_passphrase_key(&password, &salt, SCRYPT_LOG_N)?;
    let nonce = random_nonce()?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| "invalid derived key")?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
        .map_err(|_| "encryption failed")?;
    let envelope = PassphraseEnvelope {
        version: PASSPHRASE_VERSION,
        kdf: "scrypt".into(),
        log_n: SCRYPT_LOG_N,
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        ciphertext: STANDARD.encode(ciphertext),
        plaintext_sha256: format!("{:x}", Sha256::digest(&plaintext[..])),
    };
    atomic_write(
        Path::new(output),
        &serde_json::to_vec_pretty(&envelope).map_err(|e| e.to_string())?,
        0o600,
    )
}

fn decrypt_passphrase_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let output = required(args, "--output")?;
    let password = passphrase(args)?;
    let env: PassphraseEnvelope = serde_json::from_slice(&read_bounded(Path::new(input))?)
        .map_err(|e| format!("invalid passphrase envelope: {e}"))?;
    if env.version != PASSPHRASE_VERSION || env.kdf != "scrypt" {
        return Err("unsupported passphrase envelope".into());
    }
    let salt = STANDARD.decode(env.salt).map_err(|_| "invalid salt")?;
    let nonce = STANDARD.decode(env.nonce).map_err(|_| "invalid nonce")?;
    let ciphertext = STANDARD
        .decode(env.ciphertext)
        .map_err(|_| "invalid ciphertext")?;
    if nonce.len() != 12 {
        return Err("invalid nonce length".into());
    }
    let key = derive_passphrase_key(&password, &salt, env.log_n)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| "invalid derived key")?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| "wrong passphrase or damaged backup")?,
    );
    if format!("{:x}", Sha256::digest(&plaintext[..])) != env.plaintext_sha256 {
        return Err("plaintext digest mismatch".into());
    }
    atomic_write(Path::new(output), plaintext.as_ref(), 0o400)
}

fn export_nip49_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let output = required(args, "--output")?;
    let password = passphrase(args)?;
    let raw = Zeroizing::new(
        String::from_utf8(read_bounded(Path::new(input))?)
            .map_err(|_| "owner secret is not UTF-8")?,
    );
    let keys = Keys::parse(raw.trim()).map_err(|_| "owner secret is not a valid Nostr secret")?;
    let encrypted = EncryptedSecretKey::new(
        keys.secret_key(),
        &password,
        SCRYPT_LOG_N,
        KeySecurity::Unknown,
    )
    .map_err(|e| format!("encrypt NIP-49 backup: {e}"))?;
    let encoded = encrypted.to_bech32().map_err(|e| e.to_string())?;
    let recovered = EncryptedSecretKey::from_bech32(&encoded)
        .map_err(|e| e.to_string())?
        .decrypt(&password)
        .map_err(|e| e.to_string())?;
    if Keys::new(recovered).public_key() != keys.public_key() {
        return Err("NIP-49 verification failed".into());
    }
    atomic_write(Path::new(output), encoded.as_bytes(), 0o400)
}

fn import_nip49_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let output = required(args, "--output")?;
    let password = passphrase(args)?;
    let encoded = fs::read_to_string(input).map_err(|e| e.to_string())?;
    let encrypted = EncryptedSecretKey::from_bech32(encoded.trim())
        .map_err(|e| format!("invalid NIP-49 backup: {e}"))?;
    if encrypted.log_n() > SCRYPT_LOG_N {
        return Err("unsupported NIP-49 KDF cost".into());
    }
    let secret = encrypted
        .decrypt(&password)
        .map_err(|_| "wrong passphrase or damaged NIP-49 backup")?;
    let nsec = secret.to_bech32().map_err(|e| e.to_string())?;
    atomic_write(Path::new(output), nsec.as_bytes(), 0o400)
}

fn fingerprint_command(args: &[String]) -> Result<(), String> {
    let input = required(args, "--input")?;
    let bytes = Zeroizing::new(read_bounded(Path::new(input))?);
    println!("sha256:{:x}", Sha256::digest(&bytes[..]));
    Ok(())
}

const DEFAULT_KEYRING_SERVICE: &str = "buzz-server";
const DEFAULT_KEYRING_NAME: &str = "owner-identity";

fn persist_command(args: &[String]) -> Result<(), String> {
    let input = Path::new(required(args, "--input")?);
    let key_file = Path::new(required(args, "--key-file")?);
    let marker = Path::new(required(args, "--marker")?);
    let service = optional(args, "--service").unwrap_or(DEFAULT_KEYRING_SERVICE);
    let name = optional(args, "--name").unwrap_or(DEFAULT_KEYRING_NAME);
    let secret = Zeroizing::new(read_bounded(input)?);
    let secret_text = std::str::from_utf8(&secret)
        .map_err(|_| "owner secret is not UTF-8")?
        .trim();
    nostr::Keys::parse(secret_text).map_err(|_| "owner secret is not a valid Nostr secret")?;

    if env::var_os("BUZZ_DISABLE_SYSTEM_KEYRING").is_none() {
        if let Ok(entry) = keyring::Entry::new(service, name) {
            if entry.set_password(secret_text).is_ok() {
                match entry.get_password() {
                    Ok(stored) if stored == secret_text => {
                        atomic_write(marker, b"system-keyring\n", 0o600)?;
                        let _ = fs::remove_file(key_file);
                        println!("system-keyring");
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }

    atomic_write(key_file, secret_text.as_bytes(), 0o400)?;
    let readback = read_bounded(key_file)?;
    if readback != secret_text.as_bytes() {
        return Err("owner key-file read-back verification failed".into());
    }
    let _ = fs::remove_file(marker);
    println!("key-file");
    Ok(())
}

fn materialize_command(args: &[String]) -> Result<(), String> {
    let output = Path::new(required(args, "--output")?);
    let key_file = Path::new(required(args, "--key-file")?);
    let marker = Path::new(required(args, "--marker")?);
    let service = optional(args, "--service").unwrap_or(DEFAULT_KEYRING_SERVICE);
    let name = optional(args, "--name").unwrap_or(DEFAULT_KEYRING_NAME);

    if marker.exists() {
        if env::var_os("BUZZ_DISABLE_SYSTEM_KEYRING").is_none() {
            if let Ok(entry) = keyring::Entry::new(service, name) {
                if let Ok(secret) = entry.get_password() {
                    nostr::Keys::parse(secret.trim())
                        .map_err(|_| "system keyring contains an invalid owner secret")?;
                    atomic_write(output, secret.trim().as_bytes(), 0o400)?;
                    return Ok(());
                }
            }
        }
        return Err("identity is recorded in the system keyring, but the keyring is unavailable or the entry is missing".into());
    }

    if key_file.exists() {
        let secret = Zeroizing::new(read_bounded(key_file)?);
        let text = std::str::from_utf8(&secret)
            .map_err(|_| "owner key file is not UTF-8")?
            .trim();
        nostr::Keys::parse(text).map_err(|_| "owner key file contains an invalid secret")?;
        atomic_write(output, text.as_bytes(), 0o400)?;
        return Ok(());
    }

    Err("no persisted identity was found".into())
}

fn clear_local_command(args: &[String]) -> Result<(), String> {
    let key_file = Path::new(required(args, "--key-file")?);
    let marker = Path::new(required(args, "--marker")?);
    let service = optional(args, "--service").unwrap_or(DEFAULT_KEYRING_SERVICE);
    let name = optional(args, "--name").unwrap_or(DEFAULT_KEYRING_NAME);
    if let Ok(entry) = keyring::Entry::new(service, name) {
        let _ = entry.delete_credential();
    }
    let _ = fs::remove_file(key_file);
    let _ = fs::remove_file(marker);
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

fn fill_random(bytes: &mut [u8]) -> Result<(), String> {
    use std::io::Read;
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(bytes))
        .map_err(|e| e.to_string())
}
fn random_nonce() -> Result<[u8; 12], String> {
    let mut nonce = [0u8; 12];
    fill_random(&mut nonce)?;
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
    "usage: buzz-secretsctl encrypt --kms-key-id KEY --input FILE --output FILE [--aws-command PATH]\n       buzz-secretsctl decrypt --input FILE --output FILE [--aws-command PATH]\n       buzz-secretsctl fingerprint --input FILE\n       buzz-secretsctl public-key --input FILE\n       buzz-secretsctl public-id --input FILE\n       buzz-secretsctl encrypt-passphrase --input FILE --output FILE --passphrase-file FILE\n       buzz-secretsctl decrypt-passphrase --input FILE --output FILE --passphrase-file FILE\n       buzz-secretsctl export-nip49 --input FILE --output FILE --passphrase-file FILE\n       buzz-secretsctl import-nip49 --input FILE --output FILE --passphrase-file FILE\n       buzz-secretsctl persist --input FILE --key-file FILE --marker FILE [--service NAME] [--name NAME]\n       buzz-secretsctl materialize --output FILE --key-file FILE --marker FILE [--service NAME] [--name NAME]\n       buzz-secretsctl clear-local --key-file FILE --marker FILE [--service NAME] [--name NAME]".into()
}
