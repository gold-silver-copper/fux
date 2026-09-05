//! Shared manager client contract for CLI commands and integrated viewer pickers.
use crate::control::write_frame;
use anyhow::{Context, Result, bail};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ManagerRequest {
    Resolve { name: Option<String> },
    List,
    Kill { name: String },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ManagerReply {
    Attach {
        descriptor: crate::daemon::Descriptor,
    },
    Pick {
        names: Vec<String>,
    },
    Failed {
        message: String,
    },
}

pub fn manager_request(path: &Path, request: &ManagerRequest) -> Result<ManagerReply> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("connecting to manager socket {}", path.display()))?;
    set_rpc_deadlines(&stream).context("setting manager socket deadlines")?;
    crate::control::negotiate_client(&mut stream)
        .context("authenticating the manager socket and negotiating its control protocol")?;
    write_frame(&mut stream, request).context("sending manager request")?;
    let reply = read_json_frame(&mut stream).context("receiving manager reply")?;
    serde_json::from_slice(&reply).context("decoding manager reply")
}

fn set_rpc_deadlines(stream: &UnixStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(())
}

fn read_json_frame(stream: &mut UnixStream) -> Result<Vec<u8>> {
    use nix::poll::{PollFd, PollFlags, poll};
    use std::io::Read as _;
    use std::os::fd::AsFd as _;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = Vec::new();
    let mut byte = [0];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| anyhow::anyhow!("manager response timed out"))?;
        let mut descriptors = [PollFd::new(stream.as_fd(), PollFlags::POLLIN)];
        let timeout = u16::try_from(remaining.as_millis().max(1)).unwrap_or(u16::MAX);
        match poll(&mut descriptors, timeout) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
        if stream.read(&mut byte)? == 0 {
            bail!("manager closed before a complete response");
        }
        if byte[0] == b'\n' {
            return Ok(bytes);
        }
        anyhow::ensure!(
            bytes.len() < crate::control::MAX_FRAME_BYTES,
            "manager response exceeds frame limit"
        );
        bytes.push(byte[0]);
    }
}

pub fn workspace_names(path: &Path) -> Result<Vec<String>> {
    match manager_request(path, &ManagerRequest::List)? {
        ManagerReply::Pick { names } => {
            anyhow::ensure!(
                names.len() <= super::MAX_WORKSPACES,
                "too many workspaces in manager reply"
            );
            for name in &names {
                super::validate_workspace_name(name)?;
            }
            Ok(names)
        }
        ManagerReply::Failed { message } => bail!("{message}"),
        _ => bail!("manager did not return a workspace list"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_reply_reader_accepts_a_complete_frame() -> Result<()> {
        let (mut reader, mut writer) = UnixStream::pair()?;
        write_frame(
            &mut writer,
            &ManagerReply::Pick {
                names: vec!["one".into()],
            },
        )?;
        drop(writer);
        let bytes = read_json_frame(&mut reader)?;
        assert!(matches!(
            serde_json::from_slice::<ManagerReply>(&bytes),
            Ok(ManagerReply::Pick { .. })
        ));
        Ok(())
    }
}
