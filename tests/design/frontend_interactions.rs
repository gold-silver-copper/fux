use super::*;
use portable_pty::{Child as PtyChild, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};

struct Frontend {
    child: Box<dyn PtyChild + Send + Sync>,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    screens: std::sync::mpsc::Receiver<Screen>,
    capture: Option<thread::JoinHandle<Vec<u8>>>,
}
impl Frontend {
    fn start(server: &Server) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 18,
                cols: 70,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fux"));
        command.arg("attach");
        command.env("FUX_ENDPOINT", &server.endpoint);
        let child = {
            let _spawn = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            pair.slave.spawn_command(command).unwrap()
        };
        drop(pair.slave);
        let (tx, screens) = std::sync::mpsc::channel();
        let capture = thread::spawn(move || {
            let mut parser = vt100::Parser::new(18, 70, 0);
            let mut bytes = Vec::new();
            let mut chunk = [0; 8192];
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..n]);
                parser.process(&chunk[..n]);
                if bytes.ends_with(b"\x1b[?2026l") && tx.send(parser.screen().clone()).is_err() {
                    break;
                }
            }
            bytes
        });
        Self {
            child,
            master: Some(pair.master),
            writer: Some(writer),
            screens,
            capture: Some(capture),
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        self.writer.as_mut().unwrap().write_all(bytes).unwrap();
    }
    fn wait(&self, predicate: impl Fn(&Screen) -> bool) -> Screen {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let screen = self
                .screens
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if predicate(&screen) {
                return screen;
            }
        }
    }
    fn finish(mut self) -> Vec<u8> {
        eventually(|| self.child.try_wait().unwrap().is_some());
        self.writer.take();
        self.master.take();
        self.capture.take().unwrap().join().unwrap()
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
fn actual_default_shortcuts_decode_modifiers_pairs_and_menu_navigation() {
    let s = Server::start();
    let mut f = Frontend::start(&s);
    f.wait(|screen| row(screen, 17).starts_with(" main"));
    let v = s.query("fux::model::Viewer")[0]["entity"].as_u64().unwrap();
    let left = s.viewer(v)["focus"].clone();
    let left_input = s.directory.join("keys-left");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HLEFT'; cat > '{}'",
            left_input.display()
        ),
    );
    f.wait(|screen| screen.contents().starts_with("LEFT"));
    f.send(b"\x02h");
    eventually(|| s.viewer(v)["focus"] != left);
    let right = s.viewer(v)["focus"].clone();
    let right_input = s.directory.join("keys-right");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HRIGHT'; cat > '{}'",
            right_input.display()
        ),
    );
    f.wait(|screen| screen.contents().contains("RIGHT"));
    let nodes = || s.query("bevy_ui::ui_node::Node");
    let before = nodes();
    f.send(b"\x02\x1b[B");
    eventually(|| s.viewer(v)["help_scroll"] == 1);
    assert_eq!(nodes(), before);
    assert_eq!(s.viewer(v)["focus"], right);
    let selected = f.wait(|screen| {
        screen.contents().contains("Commands")
            && (0..17).any(|y| {
                row(screen, y).contains("split stacked")
                    && (0..70).any(|x| screen.cell(y, x).unwrap().inverse())
            })
    });
    f.send(b"\x1b");
    eventually(|| s.viewer(v)["prefix"] == false);
    f.send(b"\x02\x1b[1;5C");
    eventually(|| nodes() != before); // Ctrl+Right resize
    f.send(b"\x02\x1b[1;3D");
    eventually(|| s.viewer(v)["focus"] == left); // Alt+Left
    f.send(b"\x02\t");
    eventually(|| s.viewer(v)["focus"] == right);
    f.send(b"\x02\x1b[Z");
    eventually(|| s.viewer(v)["focus"] == left); // Shift+Tab
    f.send(b"\x02\x7f");
    eventually(|| s.viewer(v)["focus"] == right); // Backspace
    let tree = s.query("bevy_ecs::hierarchy::ChildOf");
    f.send(b"\x02\x1b[1;2D");
    eventually(|| s.query("bevy_ecs::hierarchy::ChildOf") != tree); // Shift+Left move
    assert_eq!(s.viewer(v)["focus"], right);
    let original_tab = s.viewer(v)["tab"].clone();
    let original_workspace = s.viewer(v)["workspace"].clone();
    f.send(b"\x02t");
    eventually(|| s.viewer(v)["tab"] != original_tab);
    let new_tab = s.viewer(v)["tab"].clone();
    f.send(b"\x02[");
    eventually(|| s.viewer(v)["tab"] == original_tab);
    f.send(b"\x02]");
    eventually(|| s.viewer(v)["tab"] == new_tab);
    f.send(b"\x02w");
    eventually(|| s.viewer(v)["workspace"] != original_workspace);
    let new_workspace = s.viewer(v)["workspace"].clone();
    f.send(b"\x02{");
    eventually(|| s.viewer(v)["workspace"] == original_workspace);
    f.send(b"\x02}");
    eventually(|| s.viewer(v)["workspace"] == new_workspace);
    f.send(b"\x02{\x02[");
    eventually(|| s.viewer(v)["tab"] == original_tab);
    assert!(fs::read(&left_input).unwrap().is_empty());
    assert!(fs::read(&right_input).unwrap().is_empty());
    f.send(b"OK");
    eventually(|| fs::read(&right_input).is_ok_and(|b| b == b"OK"));
    f.send(b"\x02d");
    let bytes = f.finish();
    if let Ok(directory) = std::env::var("FUX_DESIGN_CAPTURE") {
        fs::write(
            PathBuf::from(&directory).join("keybindings-frontend.ansi"),
            bytes,
        )
        .unwrap();
        fs::write(
            PathBuf::from(directory).join("keybindings-frontend.txt"),
            plain(&selected),
        )
        .unwrap();
    }
}

#[test]
fn attached_tabs_confirmations_copy_and_cancelled_fragmented_paste_are_isolated() {
    let s = Server::start();
    let mut f = Frontend::start(&s);
    f.wait(|screen| row(screen, 17).starts_with(" main"));
    let v = s.query("fux::model::Viewer")[0]["entity"].as_u64().unwrap();
    let input = s.directory.join("frontend-input.bin");
    s.run(
        v,
        &format!(
            r"stty raw -echo; printf '\033[2J\033[HREADY'; cat > '{}'",
            input.display()
        ),
    );
    f.wait(|screen| screen.contents().starts_with("READY"));
    f.send(b"\x02t");
    f.wait(|screen| row(screen, 17).contains("tab-2"));
    f.send(b"\x02T");
    f.wait(|screen| screen.contents().contains("choose tab"));
    f.send(b"\r");
    f.wait(|screen| {
        screen.contents().starts_with("READY") && !screen.contents().contains("choose tab")
    });
    f.send(b"\x02x");
    let confirm = f.wait(|screen| screen.contents().contains("y confirm"));
    assert!(confirm.hide_cursor());
    f.send(b"n");
    f.wait(|screen| !screen.contents().contains("y confirm"));
    f.send(b"\x02c");
    f.wait(|screen| row(screen, 17).contains("Copy:"));
    f.send(b" \x1b[Cy"); // select RE, copy, return to live
    f.wait(|screen| row(screen, 17).contains("copied via OSC52"));
    f.send(b"\x02r");
    f.wait(|screen| screen.contents().contains("rename pane"));
    f.send(b"\x1b[200~SHOULD-");
    f.wait(|screen| row(screen, 17).contains("pasting..."));
    // Cancel through a concurrent stock input request while the actual frontend
    // is still waiting for the end marker. Its remaining paste must not hit cat.
    s.key(v, "escape", false);
    f.wait(|screen| !screen.contents().contains("rename pane"));
    f.send(b"NOT-LEAK\x1b[201~");
    f.wait(|screen| row(screen, 17).contains("paste owner changed"));
    f.send(b"OK");
    eventually(|| fs::read(&input).is_ok_and(|bytes| bytes == b"OK"));
    f.master
        .as_ref()
        .unwrap()
        .resize(PtySize {
            rows: 11,
            cols: 42,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    eventually(|| s.viewer(v)["rows"] == 11 && s.viewer(v)["cols"] == 42);
    f.send(b"\x02d");
    let bytes = f.finish();
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
        )
        .unwrap();
        fs::write(
            PathBuf::from(directory).join("frontend-confirm.txt"),
            plain(&confirm),
        )
        .unwrap();
    }
}
