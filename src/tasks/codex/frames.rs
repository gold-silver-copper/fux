//! Bounded JSON-lines framing for the installed app-server stdio protocol.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

pub const MAX_FRAME: usize = 1024 * 1024;
const MAX_BATCH: usize = 64;

#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}
impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        ensure!(
            self.pending.len().saturating_add(bytes.len()) <= MAX_FRAME,
            "native frame buffer limit exceeded"
        );
        self.pending.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut consumed = 0;
        for (index, byte) in self.pending.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            ensure!(
                frames.len() < MAX_BATCH,
                "native frame batch limit exceeded"
            );
            let frame: Value = serde_json::from_slice(
                self.pending
                    .get(consumed..index)
                    .context("native frame bounds")?,
            )?;
            ensure!(frame.is_object(), "native protocol frame must be an object");
            frames.push(frame);
            consumed = index + 1;
        }
        self.pending.drain(..consumed);
        Ok(frames)
    }

    pub fn finish(&self) -> Result<()> {
        ensure!(
            self.pending.is_empty(),
            "native stream ended inside a partial frame"
        );
        Ok(())
    }
}

pub fn encode(value: &Value) -> Result<Vec<u8>> {
    ensure!(value.is_object(), "native request must be an object");
    let mut bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() < MAX_FRAME,
        "native request exceeds frame limit"
    );
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn arbitrary_fragmentation_preserves_literal_text_and_coalesced_frames() -> Result<()> {
        let values = [
            json!({"text":"literal\n\t$(never execute)\\é"}),
            json!({"id":2,"result":{}}),
        ];
        let bytes = values
            .iter()
            .map(encode)
            .collect::<Result<Vec<_>>>()?
            .concat();
        for width in [1, 2, 7, bytes.len()] {
            let mut decoder = Decoder::default();
            let mut decoded = Vec::new();
            for chunk in bytes.chunks(width) {
                decoded.extend(decoder.push(chunk)?);
            }
            decoder.finish()?;
            assert_eq!(decoded, values);
        }
        Ok(())
    }

    #[test]
    fn truncated_malformed_and_flooded_frames_fail_with_finite_buffers() -> Result<()> {
        let mut decoder = Decoder::default();
        assert!(decoder.push(b"{\"id\":")?.is_empty());
        assert!(decoder.finish().is_err());
        assert!(Decoder::default().push(b"not-json\n").is_err());
        assert!(Decoder::default().push(b"[]\n").is_err());
        assert!(Decoder::default().push(&vec![b'x'; MAX_FRAME + 1]).is_err());
        assert!(
            Decoder::default()
                .push("{}\n".repeat(MAX_BATCH + 1).as_bytes())
                .is_err()
        );
        Ok(())
    }
}
