//! Trusted discovery for future external `buzz-backend-*` executables.

use std::{
    collections::BTreeSet,
    fs,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

const PREFIX: &str = "buzz-backend-";

pub trait ProviderTrustPolicy {
    /// Trust is an administrator decision over canonical path and immutable bytes.
    fn is_trusted(&self, provider_id: &str, canonical_path: &Path, sha256: &[u8; 32]) -> bool;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderCandidate {
    pub(crate) id: String,
    pub(crate) canonical_path: PathBuf,
    pub(crate) sha256: [u8; 32],
}

impl ProviderCandidate {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    #[must_use]
    pub fn sha256_hex(&self) -> String {
        hex_digest(&self.sha256)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("provider discovery failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("provider {0} has more than one trusted executable")]
    Duplicate(String),
}

/// Scans only administrator-supplied directories. Merely appearing on PATH is
/// not a trust decision and is intentionally insufficient.
pub fn discover_trusted(
    directories: &[PathBuf],
    trust: &dyn ProviderTrustPolicy,
) -> Result<Vec<ProviderCandidate>, DiscoveryError> {
    let mut candidates = Vec::new();
    let mut ids = BTreeSet::new();
    for directory in directories {
        let directory = match directory.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(id) = provider_id_from_filename(&name) else {
                continue;
            };
            if !valid_provider_id(id) || !is_executable(&entry.path()) {
                continue;
            }
            let canonical_path = entry.path().canonicalize()?;
            if !canonical_path.is_file() {
                continue;
            }
            let sha256 = sha256_reader(File::open(&canonical_path)?)?;
            if !trust.is_trusted(id, &canonical_path, &sha256) {
                continue;
            }
            if !ids.insert(id.to_owned()) {
                return Err(DiscoveryError::Duplicate(id.to_owned()));
            }
            candidates.push(ProviderCandidate {
                id: id.to_owned(),
                canonical_path,
                sha256,
            });
        }
    }
    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(candidates)
}

pub(crate) fn sha256_reader(mut reader: impl Read) -> io::Result<[u8; 32]> {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    fn compress(state: &mut [u32; 8], block: &[u8]) {
        let mut words = [0_u32; 64];
        for (index, chunk) in block.chunks_exact(4).take(16).enumerate() {
            words[index] = u32::from_be_bytes(chunk.try_into().expect("four-byte chunk"));
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut state = INITIAL;
    let mut pending = Vec::with_capacity(128);
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "provider is too large"))?;
        pending.extend_from_slice(&buffer[..count]);
        let complete = pending.len() / 64 * 64;
        for block in pending[..complete].chunks_exact(64) {
            compress(&mut state, block);
        }
        pending.drain(..complete);
    }
    let bit_length = total
        .checked_mul(8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "provider is too large"))?;
    pending.push(0x80);
    while pending.len() % 64 != 56 {
        pending.push(0);
    }
    pending.extend_from_slice(&bit_length.to_be_bytes());
    for block in pending.chunks_exact(64) {
        compress(&mut state, block);
    }
    let mut digest = [0_u8; 32];
    for (chunk, word) in digest.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    Ok(digest)
}

pub(crate) fn hex_digest(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 15) as usize] as char);
    }
    output
}

#[must_use]
pub fn valid_provider_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn provider_id_from_filename(name: &str) -> Option<&str> {
    let raw = name.strip_prefix(PREFIX)?;
    #[cfg(windows)]
    let raw = [".exe", ".bat", ".cmd"]
        .into_iter()
        .find_map(|suffix| raw.strip_suffix(suffix))
        .unwrap_or(raw);
    (!raw.is_empty()).then_some(raw)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    struct Exact(PathBuf);
    impl ProviderTrustPolicy for Exact {
        fn is_trusted(&self, _provider_id: &str, path: &Path, _sha256: &[u8; 32]) -> bool {
            path == self.0
        }
    }

    #[test]
    fn discovery_requires_valid_name_executable_bit_and_explicit_trust() {
        let directory = tempfile::tempdir().unwrap();
        let trusted = directory.path().join("buzz-backend-reference");
        fs::write(&trusted, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&trusted, fs::Permissions::from_mode(0o700)).unwrap();
        let untrusted = directory.path().join("buzz-backend-other");
        fs::write(&untrusted, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&untrusted, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(directory.path().join("buzz-backend-NotValid"), "x").unwrap();

        let found = discover_trusted(
            &[directory.path().to_path_buf()],
            &Exact(trusted.canonicalize().unwrap()),
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "reference");
        assert_eq!(found[0].canonical_path, trusted.canonicalize().unwrap());
        assert_eq!(found[0].sha256_hex().len(), 64);
    }

    #[test]
    fn sha256_matches_standard_vector() {
        assert_eq!(
            hex_digest(&sha256_reader(&b"abc"[..]).unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
