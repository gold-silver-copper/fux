//! Length-prefixed public attachment protocol, with bounded fixture frames.
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

pub fn connect(path: &Path) -> Result<UnixStream> {
    let peer = super::local::connect(path, Instant::now() + Duration::from_secs(5))?;
    peer.set_read_timeout(Some(Duration::from_secs(5)))?;
    peer.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(peer)
}
pub fn send(peer: &mut UnixStream, value: &Value) -> Result<()> {
    send_bounded(peer, value, 65536)
}
/// Server frames include a complete screen and have the receive-side bound.
pub fn send_server(peer: &mut UnixStream, value: &Value) -> Result<()> {
    send_bounded(peer, value, 16 * 1024 * 1024)
}
fn send_bounded(peer: &mut UnixStream, value: &Value, maximum: usize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= maximum, "attachment fixture input limit");
    peer.write_all(&u32::try_from(bytes.len())?.to_be_bytes())?;
    peer.write_all(&bytes)?;
    Ok(())
}
pub fn receive(peer: &mut UnixStream) -> Result<Value> {
    let mut prefix = [0; 4];
    peer.read_exact(&mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    ensure!(
        length <= 16 * 1024 * 1024,
        "attachment fixture output limit"
    );
    let mut bytes = vec![0; length];
    peer.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn text(message: &Value) -> Result<String> {
    let Some(panes) = message
        .pointer("/state/state/panes")
        .and_then(Value::as_object)
    else {
        return Ok(String::new());
    };
    let mut text = String::new();
    for pane in panes.values() {
        // A metadata-only delta has no carried cells.
        let Some(cells) = pane.get("cells") else {
            continue;
        };
        for cell in cells
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing pane cells"))?
        {
            // Main omits the default empty text in compact wire cells.
            if let Some(value) = cell.get("text") {
                text.push_str(
                    value
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid cell text"))?,
                );
            }
        }
    }
    Ok(text)
}
