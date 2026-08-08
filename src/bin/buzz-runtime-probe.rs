use std::{
    env,
    io::{Read as _, Seek as _, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MIN_CODEX_ACP_VERSION: (u64, u64, u64) = (1, 1, 7);
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

fn main() {
    let mut args = env::args_os().skip(1);
    let Some(command) = args.next() else {
        usage();
    };
    let Some(binary) = args.next() else {
        usage();
    };
    if args.next().is_some() || command != "codex-acp-version" {
        usage();
    }

    let Some(version) = probe_codex_acp_version(Path::new(&binary)) else {
        std::process::exit(1);
    };
    if version < MIN_CODEX_ACP_VERSION {
        std::process::exit(1);
    }
    println!("{}.{}.{}", version.0, version.1, version.2);
}

fn usage() -> ! {
    eprintln!("usage: buzz-runtime-probe codex-acp-version PATH");
    std::process::exit(64);
}

/// Mirrors Buzz Desktop's codex-acp availability probe from
/// desktop/src-tauri/src/managed_agents/discovery.rs at the pinned Buzz revision.
///
/// The child is bounded by a five-second deadline. Stdout is redirected to a
/// regular temporary file so a forked descendant cannot hold a pipe open and
/// make the post-exit read block indefinitely. Version parsing is deliberately
/// strict: exactly three numeric dot-separated components are accepted.
fn probe_codex_acp_version(binary_path: &Path) -> Option<(u64, u64, u64)> {
    let mut tmp = tempfile::tempfile().ok()?;

    let mut child = Command::new(binary_path)
        .arg("--version")
        .stdout(tmp.try_clone().ok()?)
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };

    if !exit_status.success() {
        return None;
    }

    tmp.seek(SeekFrom::Start(0)).ok()?;
    let mut buf = Vec::with_capacity(128);
    let _ = (&mut tmp as &mut dyn std::io::Read)
        .take(4096)
        .read_to_end(&mut buf);

    let stdout = String::from_utf8_lossy(&buf);
    let version_str = stdout.split_whitespace().last()?;
    let mut components = version_str.split('.');
    let major = components.next()?.parse::<u64>().ok()?;
    let minor = components.next()?.parse::<u64>().ok()?;
    let patch = components.next()?.parse::<u64>().ok()?;
    if components.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn script(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex-acp");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    #[test]
    fn parses_full_semver_output() {
        let (_dir, path) = script("echo '@agentclientprotocol/codex-acp 1.1.7'");
        assert_eq!(probe_codex_acp_version(&path), Some((1, 1, 7)));
    }

    #[test]
    fn rejects_prerelease_and_partial_versions() {
        let (_dir, prerelease) = script("echo '@agentclientprotocol/codex-acp 1.1.7-rc1'");
        assert_eq!(probe_codex_acp_version(&prerelease), None);
        let (_dir, partial) = script("echo '@agentclientprotocol/codex-acp 1.1'");
        assert_eq!(probe_codex_acp_version(&partial), None);
    }

    #[test]
    fn rejects_nonzero_exit() {
        let (_dir, path) = script("echo '@agentclientprotocol/codex-acp 1.1.7'; exit 1");
        assert_eq!(probe_codex_acp_version(&path), None);
    }

    #[test]
    fn times_out_hung_direct_child() {
        let (_dir, path) = script("sleep 20");
        let started = Instant::now();
        assert_eq!(probe_codex_acp_version(&path), None);
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn descendant_holding_stdout_does_not_block_probe() {
        let (_dir, path) =
            script("echo '@agentclientprotocol/codex-acp 1.1.7'\nsleep 60 &\nexit 0");
        let started = Instant::now();
        assert_eq!(probe_codex_acp_version(&path), Some((1, 1, 7)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
