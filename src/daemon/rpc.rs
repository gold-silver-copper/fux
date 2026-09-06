//! Manager socket contract: list, resolve and kill workspaces. Uses the control preface and
//! newline-delimited JSON, one request per connection.

use crate::proto::control::write_frame;
use anyhow::{Context, Result, bail};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ManagerRequest {
    /// Attach to `name`, creating it when missing. `None` applies the documented default rule.
    Resolve {
        name: Option<String>,
    },
    List,
    Kill {
        name: String,
    },
    /// The server's identity, version, runtime directory and limits.
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ManagerReply {
    Attach {
        descriptor: super::Descriptor,
    },
    Names {
        names: Vec<String>,
    },
    Info {
        info: Box<crate::proto::control::ServerInfo>,
    },
    Failed {
        message: String,
    },
}

pub const MANAGER_DEADLINE: Duration = Duration::from_secs(15);

pub fn manager_request(path: &Path, request: &ManagerRequest) -> Result<ManagerReply> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("connecting to manager socket {}", path.display()))?;
    stream.set_read_timeout(Some(MANAGER_DEADLINE))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    crate::proto::socket::negotiate_client(&mut stream)
        .context("authenticating the manager socket and negotiating its control protocol")?;
    write_frame(&mut stream, request).context("sending manager request")?;
    let reply =
        read_json_frame(&mut stream, MANAGER_DEADLINE).context("receiving manager reply")?;
    serde_json::from_slice(&reply).context("decoding manager reply")
}

pub fn read_json_frame(stream: &mut UnixStream, deadline: Duration) -> Result<Vec<u8>> {
    use nix::poll::{PollFd, PollFlags, poll};
    use std::io::Read as _;
    use std::os::fd::AsFd as _;
    let deadline = Instant::now() + deadline;
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
            bytes.len() < crate::proto::control::MAX_FRAME_BYTES,
            "manager response exceeds frame limit"
        );
        bytes.push(byte[0]);
    }
}

pub fn workspace_names(path: &Path) -> Result<Vec<String>> {
    match manager_request(path, &ManagerRequest::List)? {
        ManagerReply::Names { names } => {
            anyhow::ensure!(
                names.len() <= crate::config::MAX_WORKSPACES,
                "too many workspaces in manager reply"
            );
            for name in &names {
                crate::ids::validate_workspace_name(name)?;
            }
            Ok(names)
        }
        ManagerReply::Failed { message } => bail!("{message}"),
        ManagerReply::Attach { .. } | ManagerReply::Info { .. } => {
            bail!("manager did not return a workspace list")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_reader_accepts_a_complete_frame() -> Result<()> {
        let (mut reader, mut writer) = UnixStream::pair()?;
        write_frame(
            &mut writer,
            &ManagerReply::Names {
                names: vec!["one".into()],
            },
        )?;
        drop(writer);
        let bytes = read_json_frame(&mut reader, Duration::from_secs(1))?;
        assert!(matches!(
            serde_json::from_slice::<ManagerReply>(&bytes),
            Ok(ManagerReply::Names { .. })
        ));
        Ok(())
    }
}
