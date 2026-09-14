//! Optional Betamax raster evidence from the same raw PTY stream as the assertions.
//! No reconstructed vt100 screen is fed into Betamax. The vendored resize extension
//! preserves Ghostty state across PTY size changes.
use anyhow::Result;
use std::path::Path;

/// Render an already-bounded raw PTY recording used by the small lifecycle probes.
pub fn record(binary: &Path, rows: u16, columns: u16, bytes: &[u8], label: &str) -> Result<()> {
    if let Some(mut capture) = Capture::new(binary, &[label], rows, columns)? {
        capture.feed(bytes)?;
        capture.checkpoint(label)?;
    }
    Ok(())
}

/// Import a bounded raw PTY recording, preserving the original control sequences.
/// Unlike optional scenario capture, an explicitly requested import requires Betamax.
pub fn import_recording(
    binary: &Path,
    recording: &Path,
    rows: u16,
    columns: u16,
    label: &str,
) -> Result<()> {
    use std::io::Read;
    anyhow::ensure!(rows > 0 && columns > 0, "recording grid must be nonzero");
    let mut bytes = Vec::new();
    std::fs::File::open(recording)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 64 * 1024 * 1024, "recording exceeds 64 MiB");
    let mut capture = Capture::new(binary, &[label], rows, columns)?.ok_or_else(|| {
        anyhow::anyhow!("recording import requires the betamax feature and FUX_BETAMAX_DIR")
    })?;
    capture.feed(&bytes)?;
    capture.checkpoint(label)?;
    Ok(())
}

/// Index all captured frames without requiring the native rendering feature.
pub fn report(directory: &Path) -> Result<()> {
    use std::fs;
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    };
    let mut entries = Vec::new();
    for session in fs::read_dir(directory)? {
        let session = session?;
        if !session.file_type()?.is_dir() {
            continue;
        }
        #[cfg(feature = "betamax")]
        if session.path().join("session.json").is_file() {
            enabled::render_session(&session.path())?;
        }
        for entry in fs::read_dir(session.path())? {
            let path = entry?.path();
            if path.to_string_lossy().ends_with(".checkpoint.json") {
                entries.push(path);
            }
        }
    }
    entries.sort();
    anyhow::ensure!(
        !entries.is_empty(),
        "no Betamax checkpoints in {}",
        directory.display()
    );
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>fux Betamax evidence</title><style>body{background:#15171b;color:#eee;font:16px monospace;margin:24px}img{max-width:100%;image-rendering:auto;border:1px solid #555}figure{margin:32px 0}a{color:#8bf}</style><h1>fux Betamax evidence</h1><p>Captured test checkpoints. Rendering alone is not a visual approval.</p>",
    );
    for path in &entries {
        let metadata: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        let relative = path
            .strip_prefix(directory)?
            .to_string_lossy()
            .replace(".checkpoint.json", ".png");
        let label = metadata["label"].as_str().unwrap_or("checkpoint");
        anyhow::ensure!(
            directory.join(&relative).is_file(),
            "missing PNG {relative}; run betamax-report with the betamax feature enabled"
        );
        html.push_str(&format!("<figure><figcaption>{}: {}</figcaption><a href=\"{}\"><img loading=lazy src=\"{}\"></a></figure>", escape(&relative), escape(label), escape(&relative), escape(&relative)));
    }
    fs::write(directory.join("index.html"), html)?;
    println!(
        "Indexed {} Betamax frames in {}",
        entries.len(),
        directory.join("index.html").display()
    );
    Ok(())
}

#[cfg(feature = "betamax")]
mod enabled {
    use super::*;
    use anyhow::ensure;
    use betamax_core::{
        TerminalSession,
        ghostty::{
            CaptureRequest, GhosttyFrameCapture, GhosttySession, PixelSize, TerminalGrid,
            TerminalTheme, TextSettings,
        },
        media,
    };
    use serde_json::json;
    use std::{fs, io::Write, path::PathBuf};

    pub struct Capture {
        session: GhosttySession,
        directory: PathBuf,
        stream: fs::File,
        bytes: usize,
        epoch_bytes: usize,
        epoch: usize,
        frame: usize,
        last_state: Vec<u8>,
    }

    fn session(rows: u16, columns: u16) -> Result<GhosttySession> {
        session_with_font(rows, columns, std::env::var("FUX_BETAMAX_FONT").ok())
    }

    fn session_with_font(rows: u16, columns: u16, font: Option<String>) -> Result<GhosttySession> {
        let text = TextSettings {
            font_size: 16.0,
            line_height: 1.25,
            font_family: font,
            padding: 0,
            ..TextSettings::default()
        };
        GhosttyFrameCapture
            .open(CaptureRequest {
                canvas: PixelSize::new(
                    u32::from(columns.max(1)) * text.cell_width(),
                    u32::from(rows.max(1)) * text.cell_height(),
                ),
                grid: TerminalGrid::new(columns, rows),
                text,
                theme: TerminalTheme::from_name("Ghostty Default Style Dark")
                    .map_err(|e| anyhow::anyhow!("{e}"))?,
            })
            .map_err(|e| anyhow::anyhow!("Betamax session: {e}"))
    }

    impl Capture {
        pub fn new(binary: &Path, args: &[&str], rows: u16, columns: u16) -> Result<Option<Self>> {
            let Some(base) = std::env::var_os("FUX_BETAMAX_DIR") else {
                return Ok(None);
            };
            Ok(Some(Self::new_in(
                Path::new(&base),
                binary,
                args,
                rows,
                columns,
            )?))
        }

        fn new_in(
            base: &Path,
            binary: &Path,
            args: &[&str],
            rows: u16,
            columns: u16,
        ) -> Result<Self> {
            fs::create_dir_all(base)?;
            let directory = tempfile::Builder::new()
                .prefix(&format!("terminal-{}-", std::process::id()))
                .tempdir_in(base)?
                .keep();
            fs::write(
                directory.join("session.json"),
                serde_json::to_vec_pretty(&json!({
                    "backend": "betamax-core 0.1.11 / libghostty-vt", "binary": binary,
                    "args": args, "rows": rows, "columns": columns,
                    "font": std::env::var("FUX_BETAMAX_FONT").unwrap_or_else(|_| "system monospace".into()), "font_size": 16,
                    "resize": "live Ghostty terminal and canvas resize; buffers preserved",
                }))?,
            )?;
            let stream = fs::File::create(directory.join("epoch-0.vt"))?;
            Ok(Self {
                session: session(rows, columns)?,
                directory,
                stream,
                bytes: 0,
                epoch_bytes: 0,
                epoch: 0,
                frame: 0,
                last_state: Vec::new(),
            })
        }

        pub fn feed(&mut self, bytes: &[u8]) -> Result<()> {
            self.bytes += bytes.len();
            self.epoch_bytes += bytes.len();
            ensure!(
                self.bytes <= 64 * 1024 * 1024,
                "Betamax PTY recording exceeds 64 MiB"
            );
            self.stream.write_all(bytes)?;
            self.session.write_vt(bytes);
            // Observation is passive: never inject duplicate terminal replies into the PTY.
            let _ = self.session.take_pending_pty_reply();
            Ok(())
        }

        pub fn resize(&mut self, rows: u16, columns: u16) -> Result<()> {
            self.checkpoint("before-resize")?;
            self.session
                .resize(
                    TerminalGrid::new(columns, rows),
                    PixelSize::new(u32::from(columns.max(1)) * 10, u32::from(rows.max(1)) * 20),
                )
                .map_err(|e| anyhow::anyhow!("Betamax resize: {e}"))?;
            self.epoch += 1;
            self.epoch_bytes = 0;
            fs::write(
                self.directory.join(format!("epoch-{}.json", self.epoch)),
                serde_json::to_vec(&json!({"rows": rows, "columns": columns}))?,
            )?;
            self.stream =
                fs::File::create(self.directory.join(format!("epoch-{}.vt", self.epoch)))?;
            self.last_state.clear();
            Ok(())
        }

        pub fn checkpoint(&mut self, label: &str) -> Result<Option<String>> {
            ensure!(
                !self.frame_pending()?,
                "checkpoint {label} is inside an unfinished synchronized frame"
            );
            let state = self
                .session
                .terminal_state()
                .map_err(|e| anyhow::anyhow!("Betamax state: {e}"))?;
            let encoded = serde_json::to_vec_pretty(&state)?;
            let text = state.viewport_text.clone();
            if encoded != self.last_state {
                let stem = format!("{:04}", self.frame);
                fs::write(self.directory.join(format!("{stem}.json")), &encoded)?;
                fs::write(
                    self.directory.join(format!("{stem}.checkpoint.json")),
                    serde_json::to_vec_pretty(
                        &json!({"label":label,"epoch":self.epoch,"epoch_bytes":self.epoch_bytes,"frame":self.frame,"raw_bytes":self.bytes}),
                    )?,
                )?;
                self.last_state = encoded;
                self.frame += 1;
            }
            Ok(Some(text))
        }

        pub fn frame_pending(&self) -> Result<bool> {
            self.session
                .synchronized_output()
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
    }

    /// Replay exact checkpoint byte offsets after timing-sensitive interactions finish.
    pub(super) fn render_session(directory: &Path) -> Result<()> {
        use anyhow::Context;
        let read_json = |path: &Path| -> Result<serde_json::Value> {
            Ok(serde_json::from_slice(&fs::read(path)?)?)
        };
        let meta = read_json(&directory.join("session.json"))?;
        let size = |value: &serde_json::Value| -> Result<(u16, u16)> {
            Ok((
                u16::try_from(value["rows"].as_u64().context("rows")?)?,
                u16::try_from(value["columns"].as_u64().context("columns")?)?,
            ))
        };
        let (rows, cols) = size(&meta)?;
        let font = meta["font"]
            .as_str()
            .filter(|font| *font != "system monospace")
            .map(str::to_owned);
        let mut terminal = session_with_font(rows, cols, font)?;
        let mut entries = fs::read_dir(directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.retain(|path| path.to_string_lossy().ends_with(".checkpoint.json"));
        entries.sort();
        let read_vt = |epoch| -> Result<Vec<u8>> {
            let path = directory.join(format!("epoch-{epoch}.vt"));
            ensure!(
                fs::metadata(&path)?.len() <= 64 * 1024 * 1024,
                "oversized VT recording"
            );
            Ok(fs::read(path)?)
        };
        let mut epoch = 0;
        let mut offset = 0;
        let mut bytes = read_vt(epoch)?;
        for path in entries {
            let checkpoint = read_json(&path)?;
            let target_epoch = checkpoint["epoch"].as_u64().context("epoch")?;
            ensure!(target_epoch >= epoch, "checkpoint epochs went backwards");
            while epoch < target_epoch {
                terminal.write_vt(&bytes[offset..]);
                epoch += 1;
                let (rows, cols) =
                    size(&read_json(&directory.join(format!("epoch-{epoch}.json")))?)?;
                terminal
                    .resize(
                        TerminalGrid::new(cols, rows),
                        PixelSize::new(u32::from(cols.max(1)) * 10, u32::from(rows.max(1)) * 20),
                    )
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                bytes = read_vt(epoch)?;
                offset = 0;
            }
            let end = usize::try_from(
                checkpoint["epoch_bytes"]
                    .as_u64()
                    .context("checkpoint byte offset")?,
            )?;
            ensure!(
                end >= offset && end <= bytes.len(),
                "checkpoint offset outside recorded VT"
            );
            terminal.write_vt(&bytes[offset..end]);
            let _ = terminal.take_pending_pty_reply();
            offset = end;
            let stem = path
                .file_name()
                .context("checkpoint filename")?
                .to_string_lossy()
                .replace(".checkpoint.json", "");
            let expected = read_json(&directory.join(format!("{stem}.json")))?;
            ensure!(
                !terminal
                    .synchronized_output()
                    .map_err(|e| anyhow::anyhow!("{e}"))?,
                "checkpoint {} contains an unfinished synchronized frame",
                path.display()
            );
            let actual = terminal
                .terminal_state()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            ensure!(
                serde_json::to_value(actual)? == expected,
                "Betamax replay differs from live checkpoint {}",
                path.display()
            );
            let frame = terminal
                .capture_frame()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            media::write_png(&directory.join(format!("{stem}.png")), &frame)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn checkpoints_require_completed_synchronized_frames() -> Result<()> {
            let root = tempfile::tempdir()?;
            let mut capture = Capture::new_in(root.path(), Path::new("fixture"), &[], 4, 20)?;
            capture.feed(b"\x1b[?2026")?;
            capture.feed(b"hpartial")?;
            ensure!(capture.frame_pending()?, "synchronized frame not detected");
            ensure!(
                capture.checkpoint("partial").is_err(),
                "accepted partial frame"
            );
            capture.feed(b" complete\x1b[?2026l")?;
            ensure!(!capture.frame_pending()?, "completed frame still pending");
            capture.checkpoint("complete")?;
            super::super::report(root.path())?;
            Ok(())
        }

        #[test]
        fn deferred_render_replays_chunked_vt_and_live_resizes_exactly() -> Result<()> {
            let root = tempfile::tempdir()?;
            let mut capture = Capture::new_in(root.path(), Path::new("fixture"), &[], 4, 20)?;
            capture.feed(b"primary\x1b[?1049h\x1b[")?;
            capture.feed(b"H\x1b[31mred\x1b[0m")?;
            capture.checkpoint("styled alternate")?;
            capture.resize(6, 30)?;
            capture.feed(b"\x1b[2;1Hresized")?;
            capture.checkpoint("resized alternate")?;
            capture.feed(b"\x1b[?1049l")?;
            capture.checkpoint("restored primary")?;
            super::super::report(root.path())?;
            let pngs = fs::read_dir(&capture.directory)?
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
                .count();
            ensure!(
                pngs == 3 && root.path().join("index.html").is_file(),
                "missing replay artifacts: {pngs}"
            );
            Ok(())
        }

        #[test]
        fn hidden_cursor_does_not_leave_a_block_in_the_raster() -> Result<()> {
            let mut terminal = session(4, 20)?;
            terminal.write_vt(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[?25h");
            let _ = terminal
                .terminal_state()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let _ = terminal
                .capture_frame()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            terminal.write_vt(b"\x1b[?2026h\x1b[?25l\x1b[4;20H\x1b[?25l\x1b[?2026l");
            let state = terminal
                .terminal_state()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            ensure!(!state.cursor.visible, "cursor must be hidden");
            let frame = terminal
                .capture_frame()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            ensure!(
                frame
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel == &frame.pixels[frame.pixels.len() - 4..]),
                "hidden cursor left pixels on the otherwise blank terminal"
            );
            Ok(())
        }

        #[test]
        fn raw_vt_renders_styles_wide_text_and_cursor_in_a_bounded_png() -> Result<()> {
            let mut terminal = session(4, 20)?;
            terminal.write_vt("\x1b[2J\x1b[H\x1b[31mRED\x1b[0m 界\r\nsecond\x1b[?25l".as_bytes());
            let state = terminal
                .terminal_state()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            ensure!(state.viewport_text.contains("RED 界\nsecond"), "{state:?}");
            ensure!(
                !state.cursor.visible && !state.styles.is_empty(),
                "styles/cursor missing"
            );
            let frame = terminal
                .capture_frame()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            ensure!(
                frame.width == 200 && frame.height == 80,
                "unexpected raster dimensions"
            );
            ensure!(
                frame
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel != &frame.pixels[..4]),
                "blank raster"
            );
            let root = tempfile::tempdir()?;
            media::write_png(&root.path().join("frame.png"), &frame)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            ensure!(
                fs::read(root.path().join("frame.png"))?.starts_with(b"\x89PNG\r\n\x1a\n"),
                "not a PNG"
            );
            Ok(())
        }

        #[test]
        fn live_resize_preserves_alternate_and_primary_buffers() -> Result<()> {
            let mut terminal = session(4, 20)?;
            terminal.write_vt(b"primary\x1b[?1049h\x1b[Halternate\x1b[?25l");
            terminal
                .resize(TerminalGrid::new(30, 6), PixelSize::new(300, 120))
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let state = terminal
                .terminal_state()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            ensure!(
                state.size == [30, 6]
                    && state.viewport_text.contains("alternate")
                    && !state.cursor.visible,
                "{state:?}"
            );
            let frame = terminal
                .capture_frame()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            ensure!(
                (frame.width, frame.height) == (300, 120),
                "canvas not resized"
            );
            terminal.write_vt(b"\x1b[?1049l");
            let text = terminal.screen_text().map_err(|e| anyhow::anyhow!("{e}"))?;
            ensure!(
                text.contains("primary") && !text.contains("alternate"),
                "{text:?}"
            );
            Ok(())
        }

        #[test]
        fn box_drawing_connects_across_cells_and_font_weights() -> Result<()> {
            let mut terminal = session(3, 3)?;
            terminal.write_vt("\x1b[H │ \r\n─┼─\r\n\x1b[1m │ \x1b[?25l".as_bytes());
            let frame = terminal
                .capture_frame()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let rgba = frame.rgba().map_err(|e| anyhow::anyhow!("{e}"))?;
            let pixel = |x: usize, y: usize| &rgba[(y * 30 + x) * 4..(y * 30 + x) * 4 + 4];
            let line = pixel(15, 30);
            ensure!(line != pixel(0, 0), "box drawing is blank");
            ensure!(
                (0..60).all(|y| pixel(15, y) == line),
                "vertical stroke breaks at a cell or weight boundary"
            );
            ensure!(
                (0..30).all(|x| pixel(x, 30) == line),
                "horizontal stroke breaks at a cell boundary"
            );
            Ok(())
        }

        #[test]
        fn wide_glyph_second_cell_is_not_painted_over() -> Result<()> {
            let mut terminal = session(2, 4)?;
            terminal.write_vt("\x1b[H界\x1b[?25l".as_bytes());
            let frame = terminal
                .capture_frame()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let rgba = frame.rgba().map_err(|e| anyhow::anyhow!("{e}"))?;
            let background = &rgba[(30 * 40 + 30) * 4..(30 * 40 + 30) * 4 + 4];
            ensure!(
                (0..20).any(|y| (10..20)
                    .any(|x| &rgba[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4] != background)),
                "wide glyph right half missing (requires a CJK-capable fallback font)"
            );
            Ok(())
        }
    }
}

#[cfg(feature = "betamax")]
pub use enabled::Capture;

#[cfg(not(feature = "betamax"))]
pub struct Capture;
#[cfg(not(feature = "betamax"))]
impl Capture {
    pub fn new(_: &Path, _: &[&str], _: u16, _: u16) -> Result<Option<Self>> {
        anyhow::ensure!(
            std::env::var_os("FUX_BETAMAX_DIR").is_none(),
            "FUX_BETAMAX_DIR requires the harness --features betamax build"
        );
        Ok(None)
    }
    pub fn feed(&mut self, _: &[u8]) -> Result<()> {
        Ok(())
    }
    pub fn resize(&mut self, _: u16, _: u16) -> Result<()> {
        Ok(())
    }
    pub fn checkpoint(&mut self, _: &str) -> Result<Option<String>> {
        Ok(None)
    }
    pub fn frame_pending(&self) -> Result<bool> {
        Ok(false)
    }
}
