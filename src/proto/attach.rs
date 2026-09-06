//! Attachment protocol v6: bounded length-prefixed JSON frames between a viewer (or a koh gateway
//! carrying a viewer) and the session server. A partial frame may only be abandoned with its
//! connection; reading another frame after a cancelled partial read would lose synchronization.

use crate::ids::PaneId;
use crate::view::{FrameUpdate, PaneUpdate};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Write};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u32 = 6;
/// Bound on one delta or full frame beyond the cells it carries; the cell budget is the pane
/// area of the viewer's screen.
pub const MAX_UPDATE_CELLS: usize = crate::view::MAX_TOTAL_CELLS;
pub const MAX_CLIENT_FRAME: usize = 64 * 1024;
pub const MAX_INPUT_CHUNK: usize = 4096;
pub const MAX_SERVER_FRAME: usize = 16 << 20;
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_VIEWERS_PER_WORKSPACE: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ClientMessage {
    Hello {
        version: u32,
        rows: u16,
        columns: u16,
    },
    /// Byte-exact input for the viewer's focused pane.
    Input {
        bytes: Vec<u8>,
    },
    /// An SGR mouse report for the application under the pointer. `generation` names the frame
    /// the viewer hit-tested against; stale generations are ignored by the server.
    Mouse {
        event: MouseEvent,
        generation: u64,
    },
    Control {
        request: crate::proto::control::Request,
    },
    /// Private history read: a viewport of `pane` starting `offset` rows above the live screen.
    View {
        request: u64,
        pane: PaneId,
        offset: u32,
    },
    Resize {
        rows: u16,
        columns: u16,
    },
    Detach,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum ServerMessage {
    Hello {
        version: u32,
    },
    /// The registry of prefix and bindings, sent once after the hello.
    Bindings {
        bindings: crate::commands::ClientBindings,
    },
    /// A full frame or a delta; see docs/local-attachment-protocol.md.
    State {
        state: Box<FrameUpdate>,
    },
    Reply {
        reply: crate::proto::control::Reply,
    },
    View {
        reply: ViewReply,
    },
    Error {
        message: String,
    },
    Exited {
        code: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewReply {
    pub request: u64,
    pub pane: PaneId,
    /// `None` when the pane no longer exists; otherwise a full update of the viewport.
    #[serde(deserialize_with = "deserialize_view")]
    pub view: Option<Box<PaneUpdate>>,
    /// History rows retained above the live screen at the time of the read.
    pub history: u32,
}

fn deserialize_view<'de, D>(deserializer: D) -> Result<Option<Box<PaneUpdate>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let view = Option::<Box<PaneUpdate>>::deserialize(deserializer)?;
    if view
        .as_ref()
        .is_some_and(|view| !view.full || view.cells.len() > MAX_UPDATE_CELLS)
    {
        return Err(serde::de::Error::custom("invalid pane view"));
    }
    Ok(view)
}

/// An SGR (1006) mouse report: `code` carries button, modifier, motion and wheel bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MouseEvent {
    pub code: u16,
    /// One-based terminal column.
    pub column: u16,
    /// One-based terminal row.
    pub row: u16,
    pub release: bool,
}

impl MouseEvent {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let tail = bytes.strip_prefix(b"\x1b[<")?;
        let release = match tail.last()? {
            b'M' => false,
            b'm' => true,
            _ => return None,
        };
        let body = std::str::from_utf8(tail.get(..tail.len().checked_sub(1)?)?).ok()?;
        let mut fields = body.split(';');
        let code = fields.next()?.parse().ok()?;
        let column = fields.next()?.parse().ok()?;
        let row = fields.next()?.parse().ok()?;
        if fields.next().is_some() || column == 0 || row == 0 {
            return None;
        }
        Some(Self {
            code,
            column,
            row,
            release,
        })
    }
    #[must_use]
    pub const fn shift(self) -> bool {
        self.code & 4 != 0
    }
    #[must_use]
    pub const fn wheel(self) -> bool {
        self.code & 64 != 0
    }
    #[must_use]
    pub const fn motion(self) -> bool {
        self.code & 32 != 0
    }
    #[must_use]
    pub const fn button(self) -> u16 {
        self.code & 3
    }
    /// The same event re-encoded with pane-relative one-based coordinates (SGR form).
    #[must_use]
    pub fn sgr(self, column: u16, row: u16) -> Vec<u8> {
        format!(
            "\x1b[<{};{};{}{}",
            self.code,
            column,
            row,
            if self.release { 'm' } else { 'M' }
        )
        .into_bytes()
    }
}

/// Read one bounded frame. Idle time before its first byte is unlimited; once a frame begins the
/// sender must finish within [`FRAME_TIMEOUT`].
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
                "invalid attachment frame length",
            ));
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).await?;
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "attachment frame stalled"))?
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
                "attachment frame exceeds limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serializes a frame within `limit` bytes.
pub fn encode_frame<T: Serialize>(value: &T, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = BoundedBytes {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value).map_err(io::Error::other)?;
    let length = u32::try_from(output.bytes.len()).map_err(io::Error::other)?;
    let mut frame = length.to_be_bytes().to_vec();
    frame.append(&mut output.bytes);
    Ok(frame)
}

pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
    limit: usize,
) -> io::Result<()> {
    let frame = encode_frame(value, limit)?;
    tokio::time::timeout(FRAME_TIMEOUT, async {
        writer.write_all(&frame).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "attachment peer stopped reading"))?
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            received,
            ClientMessage::Input {
                bytes: vec![0, 27, 255]
            }
        );
        first.write_u32(u32::MAX).await?;
        assert!(
            read_frame::<_, ClientMessage>(&mut second, MAX_CLIENT_FRAME)
                .await
                .is_err()
        );
        assert!(encode_frame(&vec![42_u8; 1024], 16).is_err());
        Ok(())
    }

    #[test]
    fn frame_state_shape_is_stable_for_gateway_consumers() {
        // Gateway consumers read `/state/state/panes/*/cells[].text`; blank runs have no text.
        let mut frame = FrameUpdate::default();
        let mut parser = vt100::Parser::new(2, 3, 0);
        parser.process(b"hi");
        frame.panes.insert(
            PaneId(1),
            PaneUpdate::full_from_screen(parser.screen(), "", 0, None).unwrap_or_default(),
        );
        let value = serde_json::to_value(ServerMessage::State {
            state: Box::new(frame),
        })
        .unwrap_or_default();
        let text: String = value
            .pointer("/state/state/panes")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flat_map(|panes| panes.values())
            .flat_map(|pane| pane["cells"].as_array().into_iter().flatten())
            .filter_map(|cell| cell["text"].as_str())
            .collect();
        assert_eq!(text, "hi");
    }

    #[test]
    fn sgr_mouse_reports_parse_and_re_encode() {
        let event = MouseEvent::parse(b"\x1b[<36;12;2M").unwrap_or(MouseEvent {
            code: 0,
            column: 0,
            row: 0,
            release: true,
        });
        assert!(event.shift() && event.motion() && !event.wheel() && !event.release);
        assert_eq!(event.sgr(3, 4), b"\x1b[<36;3;4M");
        assert!(MouseEvent::parse(b"\x1b[<0;0;1M").is_none());
        assert!(MouseEvent::parse(b"\x1b[<0;1;1;9M").is_none());
        assert!(MouseEvent::parse(b"\x1b[<0;1;1").is_none());
    }
}
