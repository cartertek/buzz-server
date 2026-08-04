//! Owner-only Unix-domain transport for the constrained signer protocol.

use std::{
    io,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::watch,
    task::JoinSet,
};

use crate::signer::{DisposableSigner, MAX_SIGNER_FRAME_BYTES};

const MAX_CONCURRENT_CONNECTIONS: usize = 32;

pub struct SignerIpcServer {
    socket_path: PathBuf,
    signer: Arc<DisposableSigner>,
}

impl SignerIpcServer {
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>, signer: Arc<DisposableSigner>) -> Self {
        Self {
            socket_path: socket_path.into(),
            signer,
        }
    }

    /// Serves exactly one request and response per accepted connection.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<(), SignerIpcError> {
        prepare_socket_path(&self.socket_path)?;
        let listener = UnixListener::bind(&self.socket_path)?;
        let guard = SocketGuard(self.socket_path.clone());
        std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600))?;
        let mut connections = JoinSet::new();

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                accepted = listener.accept(), if connections.len() < MAX_CONCURRENT_CONNECTIONS => {
                    let (stream, _) = accepted?;
                    let signer = Arc::clone(&self.signer);
                    connections.spawn(async move {
                        let _ = serve_connection(stream, &signer).await;
                    });
                }
                completed = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = completed {
                        return Err(SignerIpcError::Task(error.to_string()));
                    }
                }
            }
        }

        connections.abort_all();
        while connections.join_next().await.is_some() {}
        drop(listener);
        drop(guard);
        Ok(())
    }
}

async fn serve_connection(
    mut stream: UnixStream,
    signer: &DisposableSigner,
) -> Result<(), io::Error> {
    let frame = read_bounded_frame(&mut stream).await?;
    let response = signer.handle_frame(&frame);
    stream.write_all(&response).await?;
    stream.shutdown().await
}

/// Reads the four-byte length before allocating payload storage. Oversize and
/// truncated frames are returned to `handle_frame`, which produces the stable
/// protocol error response without parsing or allocating the claimed payload.
async fn read_bounded_frame(stream: &mut UnixStream) -> Result<Vec<u8>, io::Error> {
    let mut header = [0_u8; 4];
    let mut header_read = 0;
    while header_read < header.len() {
        let read = stream.read(&mut header[header_read..]).await?;
        if read == 0 {
            return Ok(header[..header_read].to_vec());
        }
        header_read += read;
    }

    let payload_length = u32::from_be_bytes(header) as usize;
    if payload_length > MAX_SIGNER_FRAME_BYTES {
        return Ok(header.to_vec());
    }
    let mut frame = Vec::with_capacity(4 + payload_length);
    frame.extend_from_slice(&header);
    frame.resize(4 + payload_length, 0);
    let mut offset = 4;
    while offset < frame.len() {
        let read = stream.read(&mut frame[offset..]).await?;
        if read == 0 {
            frame.truncate(offset);
            break;
        }
        offset += read;
    }
    Ok(frame)
}

fn prepare_socket_path(path: &Path) -> Result<(), SignerIpcError> {
    let parent = path.parent().ok_or(SignerIpcError::MissingParent)?;
    let metadata = std::fs::metadata(parent)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(SignerIpcError::InsecureParentPermissions);
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => return Err(SignerIpcError::PathOccupied),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.0).is_ok_and(|metadata| metadata.file_type().is_socket())
        {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SignerIpcError {
    #[error("signer socket path must have a parent directory")]
    MissingParent,
    #[error("signer socket parent directory must be owner-only")]
    InsecureParentPermissions,
    #[error("signer socket path is occupied by a non-socket entry")]
    PathOccupied,
    #[error("signer connection task failed: {0}")]
    Task(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        signer::{
            decode_signer_response, encode_authorize_request, AuthorizeAgentRequest,
            SignerErrorCode, SignerIpcResponse,
        },
        CommunityConfigId,
    };
    use tempfile::TempDir;
    use url::Url;

    fn fixture() -> (
        TempDir,
        PathBuf,
        Arc<DisposableSigner>,
        AuthorizeAgentRequest,
    ) {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("signer.sock");
        let community = CommunityConfigId::new();
        let relay = Url::parse("wss://relay.example.test/").unwrap();
        let (signer, agent) = DisposableSigner::generate(community, relay.clone(), "kind=9");
        let request = AuthorizeAgentRequest {
            action: "authorize_agent".into(),
            community_config_id: community,
            relay_url: relay,
            agent_pubkey: agent.public_key_hex(),
            conditions: "kind=9".into(),
        };
        (directory, path, Arc::new(signer), request)
    }

    async fn exchange(path: &Path, frame: &[u8]) -> Vec<u8> {
        let mut stream = UnixStream::connect(path).await.unwrap();
        stream.write_all(frame).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        response
    }

    async fn wait_for_socket(path: &Path) {
        for _ in 0..100 {
            if path.exists() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("socket was not created");
    }

    #[tokio::test]
    async fn valid_authorization_and_owner_only_permissions() {
        let (_directory, path, signer, request) = fixture();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = SignerIpcServer::new(&path, signer);
        let task = tokio::spawn(async move { server.run(shutdown_rx).await });
        wait_for_socket(&path).await;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let response = exchange(&path, &encode_authorize_request(&request)).await;
        assert!(matches!(
            decode_signer_response(&response).unwrap(),
            SignerIpcResponse::Authorized(_)
        ));
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn malformed_and_oversize_frames_are_rejected_without_secret_leakage() {
        let (_directory, path, signer, _request) = fixture();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = SignerIpcServer::new(&path, signer);
        let task = tokio::spawn(async move { server.run(shutdown_rx).await });
        wait_for_socket(&path).await;

        let malformed = exchange(&path, &[0, 0, 0, 4, b'{']).await;
        let oversize = exchange(&path, &((MAX_SIGNER_FRAME_BYTES as u32 + 1).to_be_bytes())).await;
        for (frame, expected) in [
            (malformed, SignerErrorCode::MalformedFrame),
            (oversize, SignerErrorCode::FrameTooLarge),
        ] {
            let response = decode_signer_response(&frame).unwrap();
            assert!(
                matches!(response, SignerIpcResponse::Error(ref error) if error.code == expected)
            );
            let encoded = String::from_utf8(frame).unwrap_or_default();
            assert!(!encoded.contains("nsec"));
            assert!(!encoded.contains("secret"));
        }
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shutdown_aborts_idle_connections_and_cleans_socket() {
        let (_directory, path, signer, _request) = fixture();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = SignerIpcServer::new(&path, signer);
        let task = tokio::spawn(async move { server.run(shutdown_rx).await });
        wait_for_socket(&path).await;
        let _idle = UnixStream::connect(&path).await.unwrap();
        shutdown_tx.send(true).unwrap();
        task.await.unwrap().unwrap();
        assert!(!path.exists());
    }
}
