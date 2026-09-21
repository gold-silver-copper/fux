use super::*;
use portable_pty::{Child as PtyChild, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};

struct Frontend {
    child: Box<dyn PtyChild + Send + Sync>,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    screens: std::sync::mpsc::Receiver<Screen>,
    capture: Option<thread::JoinHandle<Result<Vec<u8>, String>>>,
}
impl Frontend {
    fn start(server: &Server) -> Result<Self, Fail> {
        let pair = native_pty_system().openpty(PtySize {
            rows: 18,
            cols: 70,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fux"));
        command.arg("attach");
        command.env("FUX_ENDPOINT", &server.endpoint);
        let child = {
            let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            pair.slave.spawn_command(command)?
        };
        drop(pair.slave);
        let (tx, screens) = std::sync::mpsc::channel();
        let capture = thread::spawn(move || {
            let mut parser = fux_vt::Parser::new(18, 70, 0).need()?;
            let mut bytes = Vec::new();
            let mut chunk = [0; 8192];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(chunk.get(..n).unwrap_or_default());
                parser.process(chunk.get(..n).unwrap_or_default()).need()?;
                if bytes.ends_with(b"\x1b[?2026l") && tx.send(parser.screen().clone()).is_err() {
                    break;
                }
            }
            Ok::<_, String>(bytes)
        });
        Ok(Self {
            child,
            master: Some(pair.master),
            writer: Some(writer),
            screens,
            capture: Some(capture),
        })
    }
    fn send(&mut self, bytes: &[u8]) -> Result<(), Fail> {
        self.writer.as_mut().need()?.write_all(bytes)?;
        Ok(())
    }
    fn wait(&self, predicate: impl Fn(&Screen) -> bool) -> Result<Screen, Fail> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let screen = self
                .screens
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
            if predicate(&screen) {
                return Ok(screen);
            }
        }
    }
    fn finish(mut self) -> Result<Vec<u8>, Fail> {
        eventually(|| Ok(self.child.try_wait()?.is_some()))?;
        self.writer.take();
        self.master.take();
        self.capture
            .take()
            .need()?
            .join()
            .map_err(|_| "frontend capture thread panicked")?
            .map_err(Into::into)
    }
}
impl Drop for Frontend {
    fn drop(&mut self) {
        self.writer.take();
        self.master.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn actual_default_shortcuts_decode_modifiers_pairs_and_menu_navigation() -> Outcome {
    let s = Server::start()?;
    let mut f = Frontend::start(&s)?;
    f.wait(|screen| row(screen, 17).starts_with(" main"))?;
    let v = s
        .query("fux::model::Viewer")?
        .at(0)
        .at("entity")
        .as_u64()
        .need()?;
    let left = s.focused(v)?;
    let left_input = s.directory.join("keys-left");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HLEFT'; cat > '{}'",
            left_input.display()
        ),
    )?;
    f.wait(|screen| screen.contents().starts_with("LEFT"))?;
    f.send(b"\x02h")?;
    eventually(|| Ok(s.focused(v)? != left))?;
    let right = s.focused(v)?;
    let right_input = s.directory.join("keys-right");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HRIGHT'; cat > '{}'",
            right_input.display()
        ),
    )?;
    f.wait(|screen| screen.contents().contains("RIGHT"))?;
    let nodes = || s.query("bevy_ui::ui_node::Node");
    let before = nodes()?;
    f.send(b"\x02\x1b[B")?;
    eventually(|| Ok(s.selected(v, 18, 70)?.as_deref() == Some("v  split stacked")))?;
    assert_eq!(nodes()?, before);
    assert_eq!(s.focused(v)?, right);
    let selected = f.wait(|screen| {
        screen.contents().contains("Commands")
            && (0..17).any(|y| {
                row(screen, y).contains("split stacked")
                    && (0..70).any(|x| screen.cell(y, x).is_some_and(|c| c.inverse()))
            })
    })?;
    f.send(b"\x1b")?;
    eventually(|| Ok(!s.column_open(v, 18, 70)?))?;
    f.send(b"\x02\x1b[1;5C")?;
    eventually(|| Ok(nodes()? != before))?; // Ctrl+Right resize
    f.send(b"\x02\x1b[1;3D")?;
    eventually(|| Ok(s.focused(v)? == left))?; // Alt+Left
    f.send(b"\x02\t")?;
    eventually(|| Ok(s.focused(v)? == right))?;
    f.send(b"\x02\x1b[Z")?;
    eventually(|| Ok(s.focused(v)? == left))?; // Shift+Tab
    f.send(b"\x02\x7f")?;
    eventually(|| Ok(s.focused(v)? == right))?; // Backspace
    let tree = s.query("bevy_ecs::hierarchy::ChildOf")?;
    f.send(b"\x02\x1b[1;2D")?;
    eventually(|| Ok(s.query("bevy_ecs::hierarchy::ChildOf")? != tree))?; // Shift+Left move
    assert_eq!(s.focused(v)?, right);
    let original_tab = s.on_tab(v)?;
    let original_workspace = s.viewing(v)?;
    f.send(b"\x02t")?;
    eventually(|| Ok(s.on_tab(v)? != original_tab))?;
    let new_tab = s.on_tab(v)?;
    f.send(b"\x02[")?;
    eventually(|| Ok(s.on_tab(v)? == original_tab))?;
    f.send(b"\x02]")?;
    eventually(|| Ok(s.on_tab(v)? == new_tab))?;
    f.send(b"\x02w")?;
    eventually(|| Ok(s.viewing(v)? != original_workspace))?;
    let new_workspace = s.viewing(v)?;
    f.send(b"\x02{")?;
    eventually(|| Ok(s.viewing(v)? == original_workspace))?;
    f.send(b"\x02}")?;
    eventually(|| Ok(s.viewing(v)? == new_workspace))?;
    f.send(b"\x02{\x02[")?;
    eventually(|| Ok(s.on_tab(v)? == original_tab))?;
    assert!(fs::read(&left_input)?.is_empty());
    assert!(fs::read(&right_input)?.is_empty());
    f.send(b"OK")?;
    eventually(|| Ok(fs::read(&right_input).is_ok_and(|b| b == b"OK")))?;
    f.send(b"\x02d")?;
    let bytes = f.finish()?;
    if let Ok(directory) = std::env::var("FUX_DESIGN_CAPTURE") {
        fs::write(
            PathBuf::from(&directory).join("keybindings-frontend.ansi"),
            bytes,
        )?;
        fs::write(
            PathBuf::from(directory).join("keybindings-frontend.txt"),
            plain(&selected),
        )?;
    }
    Ok(())
}

#[test]
fn attached_tabs_confirmations_copy_and_cancelled_fragmented_paste_are_isolated() -> Outcome {
    let s = Server::start()?;
    let mut f = Frontend::start(&s)?;
    f.wait(|screen| row(screen, 17).starts_with(" main"))?;
    let v = s
        .query("fux::model::Viewer")?
        .at(0)
        .at("entity")
        .as_u64()
        .need()?;
    let input = s.directory.join("frontend-input.bin");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HREADY'; cat > '{}'",
            input.display()
        ),
    )?;
    f.wait(|screen| screen.contents().starts_with("READY"))?;
    f.send(b"\x02t")?;
    f.wait(|screen| row(screen, 17).contains("tab-2"))?;
    f.send(b"\x02T")?;
    f.wait(|screen| screen.contents().contains("choose tab"))?;
    f.send(b"\r")?;
    f.wait(|screen| {
        screen.contents().starts_with("READY") && !screen.contents().contains("choose tab")
    })?;
    f.send(b"\x02x")?;
    let confirm = f.wait(|screen| screen.contents().contains("y confirm"))?;
    assert!(confirm.hide_cursor());
    f.send(b"n")?;
    f.wait(|screen| !screen.contents().contains("y confirm"))?;
    f.send(b"\x02c")?;
    f.wait(|screen| row(screen, 17).contains("Copy:"))?;
    f.send(b" \x1b[Cy")?; // select RE, copy, return to live
    f.wait(|screen| row(screen, 17).contains("copied via OSC52"))?;
    f.send(b"\x02r")?;
    f.wait(|screen| screen.contents().contains("rename pane"))?;
    f.send(b"\x1b[200~SHOULD-")?;
    f.wait(|screen| row(screen, 17).contains("pasting..."))?;
    // Cancel through a concurrent stock input request while the actual frontend
    // is still waiting for the end marker. Its remaining paste must not hit cat.
    s.key(v, "escape", false)?;
    f.wait(|screen| !screen.contents().contains("rename pane"))?;
    f.send(b"NOT-LEAK\x1b[201~")?;
    f.wait(|screen| row(screen, 17).contains("paste owner changed"))?;
    f.send(b"OK")?;
    eventually(|| Ok(fs::read(&input).is_ok_and(|bytes| bytes == b"OK")))?;
    f.master.as_ref().need()?.resize(PtySize {
        rows: 11,
        cols: 42,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    eventually(|| Ok(s.viewer(v)?.at("rows") == 11 && s.viewer(v)?.at("cols") == 42))?;
    f.send(b"\x02d")?;
    let bytes = f.finish()?;
    assert!(
        bytes
            .windows(b"\x1b]52;c;UkU=\x07".len())
            .any(|part| part == b"\x1b]52;c;UkU=\x07")
    );
    assert!(bytes.ends_with(b"\x1b[?1049l"));
    if let Ok(directory) = std::env::var("FUX_DESIGN_CAPTURE") {
        fs::write(
            PathBuf::from(&directory).join("frontend-interactions.ansi"),
            bytes,
        )?;
        fs::write(
            PathBuf::from(directory).join("frontend-confirm.txt"),
            plain(&confirm),
        )?;
    }
    Ok(())
}
