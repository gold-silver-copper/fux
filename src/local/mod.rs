//! Local attachment protocol. Frames are bounded JSON preceded by a big-endian u32 length.
//! A reader must run to completion or its connection must be discarded: cancelling a partial
//! frame and then reading another frame would lose framing synchronization.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Write};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u32 = 2;
pub const MAX_CLIENT_FRAME: usize = 64 * 1024;
pub const MAX_SERVER_FRAME: usize = crate::state::RECV_DECODE_LIMIT;
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ClientMessage {
    Hello {
        version: u32,
        rows: u16,
        columns: u16,
    },
    Input {
        bytes: Vec<u8>,
    },
    PaneInput {
        bytes: Vec<u8>,
    },
    Mouse {
        event: crate::host::MouseEvent,
    },
    Binding {
        key: u8,
    },
    Control {
        request: crate::control::Request,
    },
    CopyView {
        request: u64,
        pane: u32,
        offset: u32,
    },
    Resize {
        rows: u16,
        columns: u16,
    },
    Detach,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum ServerMessage {
    Hello {
        version: u32,
    },
    State {
        state: Box<crate::state::WorkspaceState>,
    },
    Error {
        message: String,
    },
    Reply {
        reply: crate::control::Reply,
    },
    CopyView {
        reply: CopyViewReply,
    },
    Exited {
        code: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyViewReply {
    pub request: u64,
    pub pane: u32,
    #[serde(deserialize_with = "deserialize_copy_view")]
    pub view: Option<Box<crate::state::PaneView>>,
}

fn deserialize_copy_view<'de, D>(
    deserializer: D,
) -> Result<Option<Box<crate::state::PaneView>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let view = Option::<Box<crate::state::PaneView>>::deserialize(deserializer)?;
    if view.as_ref().is_some_and(|view| !view.valid()) {
        return Err(serde::de::Error::custom("invalid copy viewport"));
    }
    Ok(view)
}

impl From<Vec<u8>> for ClientMessage {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Input { bytes }
    }
}

/// Read one bounded frame. Idle time before its first byte is intentionally unlimited;
/// once a frame begins, the sender must finish within the deadline.
pub async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
    limit: usize,
) -> io::Result<T> {
    let first = reader.read_u8().await?;
    tokio::time::timeout(FRAME_TIMEOUT, async {
        let mut rest = [0; 3];
        reader.read_exact(&mut rest).await?;
        let [second, third, fourth] = rest;
        let length = u32::from_be_bytes([first, second, third, fourth]) as usize;
        if length == 0 || length > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid local frame length",
            ));
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).await?;
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "local frame stalled"))?
}

struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local frame exceeds limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
    limit: usize,
) -> io::Result<()> {
    let mut output = BoundedBytes {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value).map_err(io::Error::other)?;
    let length = u32::try_from(output.bytes.len()).map_err(io::Error::other)?;
    tokio::time::timeout(FRAME_TIMEOUT, async {
        writer.write_u32(length).await?;
        writer.write_all(&output.bytes).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "local client stopped reading"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "construct a valid populated workspace fixture"
    )]
    async fn populated_workspace_round_trips_without_losing_numeric_map_keys() -> io::Result<()> {
        use crate::state::{LayoutTree, PaneId, PaneView, Tab, TabId, WorkspaceState};
        let mut state = WorkspaceState::default();
        let parser = vt100::Parser::new(2, 2, 0);
        let pane = PaneView::from_vt100(parser.screen(), String::new(), Default::default(), 0);
        state
            .insert_pane(PaneId(1), pane.expect("valid screen"))
            .expect("valid pane");
        state
            .replace_tabs(
                vec![Tab {
                    id: TabId(1),
                    name: "main".into(),
                    layout: LayoutTree::new(PaneId(1)),
                    focused: PaneId(1),
                    zoomed: None,
                }],
                Some(TabId(1)),
            )
            .expect("valid tab");
        let (mut writer, mut reader) = tokio::io::duplex(65536);
        write_frame(
            &mut writer,
            &ServerMessage::State {
                state: Box::new(state.clone()),
            },
            MAX_SERVER_FRAME,
        )
        .await?;
        let received: ServerMessage = read_frame(&mut reader, MAX_SERVER_FRAME).await?;
        assert!(matches!(received, ServerMessage::State { state: received } if *received == state));
        Ok(())
    }

    #[tokio::test]
    async fn round_trip_and_oversized_header_rejection() -> io::Result<()> {
        let (mut first, mut second) = tokio::io::duplex(1024);
        write_frame(
            &mut first,
            &ClientMessage::Input {
                bytes: vec![0, 27, 255],
            },
            MAX_CLIENT_FRAME,
        )
        .await?;
        let received: ClientMessage = read_frame(&mut second, MAX_CLIENT_FRAME).await?;
        assert!(matches!(received, ClientMessage::Input { bytes } if bytes == vec![0,27,255]));
        first.write_u32(u32::MAX).await?;
        assert!(
            read_frame::<_, ClientMessage>(&mut second, MAX_CLIENT_FRAME)
                .await
                .is_err()
        );
        Ok(())
    }
    #[tokio::test]
    async fn serialization_cap_is_enforced_before_writing() -> io::Result<()> {
        let (mut first, _second) = tokio::io::duplex(1024);
        assert!(
            write_frame(&mut first, &vec![42_u8; 1024], 16)
                .await
                .is_err()
        );
        Ok(())
    }
}

pub mod server;

pub mod client;
