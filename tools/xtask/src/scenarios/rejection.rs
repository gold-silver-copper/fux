//! Rejected local handshakes cannot alter tty settings or enter the alternate screen.
use crate::support::{
    local::{Root, until},
    process::{Guard, wait},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::Path,
    process::Stdio,
    time::Duration,
};

pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new("fpr-rs-", &["/bin/cat".into()])?;
    let path = root.path().join("service.sock");
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    for response in [
        json!({"hello":{"version":1}}),
        json!({"hello":{"version":999}}),
        json!({"error":{"message":"incompatible local protocol; save work before restart"}}),
    ] {
        let pty = crate::support::pty::open(0, 0)?;
        let before = nix::sys::termios::tcgetattr(&pty.slave)?;
        let child = root
            .command(binary)
            .args(["attach", "--socket"])
            .arg(&path)
            .stdin(Stdio::from(pty.slave.try_clone()?))
            .stdout(Stdio::from(pty.slave.try_clone()?))
            .stderr(Stdio::from(pty.slave.try_clone()?))
            .spawn()?;
        let mut child = Guard(child);
        let (mut peer, _) = until(Duration::from_secs(5), || match listener.accept() {
            Ok(connection) => Ok(Some(connection)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.into()),
        })
        .map_err(|error| {
            let _ = nix::fcntl::fcntl(
                &pty.master,
                nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
            );
            let mut output = [0; 65536];
            let count = nix::unistd::read(&pty.master, &mut output).unwrap_or(0);
            anyhow::anyhow!(
                "{error:#}; client output: {}",
                String::from_utf8_lossy(&output[..count])
            )
        })?;
        peer.set_nonblocking(false)?;
        peer.set_read_timeout(Some(Duration::from_secs(5)))?;
        peer.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut length = [0; 4];
        peer.read_exact(&mut length)?;
        let size = u32::from_be_bytes(length) as usize;
        ensure!(size <= 65536, "oversized client hello");
        let mut bytes = vec![0; size];
        peer.read_exact(&mut bytes)?;
        let hello: Value = serde_json::from_slice(&bytes)?;
        ensure!(hello["type"] == "hello", "wrong hello");
        let encoded = serde_json::to_vec(&response)?;
        peer.write_all(&u32::try_from(encoded.len())?.to_be_bytes())?;
        peer.write_all(&encoded)?;
        ensure!(
            !wait(&mut child.0, Duration::from_secs(5))?.success(),
            "rejection exited successfully"
        );
        ensure!(
            nix::sys::termios::tcgetattr(&pty.slave)? == before,
            "terminal attributes changed on rejection"
        );
        nix::fcntl::fcntl(
            &pty.master,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )?;
        let mut output = Vec::new();
        loop {
            let mut bytes = [0; 16384];
            match nix::unistd::read(&pty.master, &mut bytes) {
                Ok(0) | Err(nix::errno::Errno::EIO | nix::errno::Errno::EAGAIN) => break,
                Ok(count) => {
                    output.extend_from_slice(&bytes[..count]);
                    ensure!(output.len() <= 65536, "unbounded rejection output");
                }
                Err(error) => return Err(error.into()),
            }
        }
        let text = String::from_utf8_lossy(&output);
        ensure!(
            text.contains("incompatible"),
            "missing rejection diagnostic: {text}"
        );
        ensure!(
            !text.contains("\x1b[?1049h"),
            "alternate screen entered before negotiation"
        );
        ensure!(!text.contains("Passphrase"), "unexpected passphrase prompt");
    }
    fn no_keys(path: &Path) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                no_keys(&entry.path())?;
            } else {
                ensure!(
                    entry.path().extension() != Some(std::ffi::OsStr::new("key")),
                    "unexpected key file"
                );
            }
        }
        Ok(())
    }
    no_keys(root.path())?;
    println!("PASS: version/error rejection preserves terminal state and creates no keys");
    Ok(())
}
