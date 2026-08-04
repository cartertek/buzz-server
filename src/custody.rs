//! Owner-only filesystem custody for disposable per-agent Nostr identities.

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use nostr::Keys;

use crate::AgentId;

const SECRET_HEX_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustodiedIdentity {
    pub agent_id: AgentId,
    pub public_key: String,
}

pub trait AgentIdentityCustody: Send + Sync {
    fn provision(&self, agent_id: AgentId) -> Result<CustodiedIdentity, CustodyError>;
    fn load(&self, agent_id: AgentId) -> Result<Keys, CustodyError>;
    fn purge(&self, agent_id: AgentId) -> Result<(), CustodyError>;
}

#[derive(Clone, Debug)]
pub struct FilesystemAgentIdentityCustody {
    root: PathBuf,
    expected_owner_uid: u32,
}

impl FilesystemAgentIdentityCustody {
    pub fn new(root: impl Into<PathBuf>, expected_owner_uid: u32) -> Self {
        Self {
            root: root.into(),
            expected_owner_uid,
        }
    }

    fn prepare_root(&self) -> Result<(), CustodyError> {
        fs::create_dir_all(&self.root)?;
        let metadata = fs::symlink_metadata(&self.root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(CustodyError::UnsafeRoot);
        }
        if metadata.uid() != self.expected_owner_uid {
            return Err(CustodyError::WrongOwner);
        }
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn secret_path(&self, agent_id: AgentId) -> PathBuf {
        self.root.join(format!("{agent_id}.secret"))
    }

    fn read_keys(&self, path: &Path) -> Result<Keys, CustodyError> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != self.expected_owner_uid
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(CustodyError::UnsafeSecret);
        }
        let mut file = fs::File::open(path)?;
        let mut secret = String::new();
        Read::by_ref(&mut file)
            .take((SECRET_HEX_BYTES + 1) as u64)
            .read_to_string(&mut secret)?;
        if secret.len() != SECRET_HEX_BYTES || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CustodyError::InvalidSecret);
        }
        Keys::parse(&secret).map_err(|_| CustodyError::InvalidSecret)
    }
}

impl AgentIdentityCustody for FilesystemAgentIdentityCustody {
    fn provision(&self, agent_id: AgentId) -> Result<CustodiedIdentity, CustodyError> {
        self.prepare_root()?;
        let path = self.secret_path(agent_id);
        let keys = match self.read_keys(&path) {
            Ok(keys) => keys,
            Err(CustodyError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                let keys = Keys::generate();
                let temporary = self
                    .root
                    .join(format!(".{agent_id}.{}.tmp", uuid::Uuid::now_v7().simple()));
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(&temporary)?;
                file.write_all(keys.secret_key().to_secret_hex().as_bytes())?;
                file.sync_all()?;
                match fs::hard_link(&temporary, &path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        let _ = fs::remove_file(&temporary);
                        return Err(error.into());
                    }
                }
                fs::remove_file(&temporary)?;
                self.read_keys(&path)?
            }
            Err(error) => return Err(error),
        };
        Ok(CustodiedIdentity {
            agent_id,
            public_key: keys.public_key().to_hex(),
        })
    }

    fn load(&self, agent_id: AgentId) -> Result<Keys, CustodyError> {
        self.prepare_root()?;
        self.read_keys(&self.secret_path(agent_id))
    }

    fn purge(&self, agent_id: AgentId) -> Result<(), CustodyError> {
        self.prepare_root()?;
        match fs::remove_file(self.secret_path(agent_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CustodyError {
    #[error("custody I/O failed")]
    Io(#[from] std::io::Error),
    #[error("custody root is not a real directory")]
    UnsafeRoot,
    #[error("custody path is owned by an unexpected uid")]
    WrongOwner,
    #[error("custodied secret has unsafe ownership, type, or permissions")]
    UnsafeSecret,
    #[error("custodied secret is invalid")]
    InvalidSecret,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_is_stable_owner_only_and_purge_removes_identity() {
        let directory = tempfile::tempdir().unwrap();
        let owner = fs::metadata(directory.path()).unwrap().uid();
        let root = directory.path().join("identities");
        let custody = FilesystemAgentIdentityCustody::new(&root, owner);
        let agent_id = AgentId::new();

        let first = custody.provision(agent_id).unwrap();
        let second = custody.provision(agent_id).unwrap();
        assert_eq!(first, second);
        assert!(!format!("{first:?}")
            .contains(&custody.load(agent_id).unwrap().secret_key().to_secret_hex()));
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(custody.secret_path(agent_id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        custody.purge(agent_id).unwrap();
        assert!(
            matches!(custody.load(agent_id), Err(CustodyError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn rejects_secret_with_relaxed_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let owner = fs::metadata(directory.path()).unwrap().uid();
        let custody = FilesystemAgentIdentityCustody::new(directory.path(), owner);
        let agent_id = AgentId::new();
        custody.provision(agent_id).unwrap();
        fs::set_permissions(
            custody.secret_path(agent_id),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(matches!(
            custody.load(agent_id),
            Err(CustodyError::UnsafeSecret)
        ));
    }
}
