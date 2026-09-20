use super::*;
use portable_pty::{Child as PtyChild, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};

#[test]
fn actual_attached_frontend_renders_bottom_chrome_and_consumes_prefix_keys() {
    struct Attached {
        child: Box<dyn PtyChild + Send + Sync>,
        master: Option<Box<dyn MasterPty + Send>>,
    }
    impl Drop for Attached {
        fn drop(&mut self) {
            self.master.take();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    let s = Server::start();
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 13,
            cols: 47,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fux"));
    command.arg("attach");
    command.env("FUX_ENDPOINT", &s.endpoint);
    let mut attached = Attached {
        child: {
            let _spawn = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
            pair.slave.spawn_command(command).unwrap()
        },
        master: Some(pair.master),
    };
    drop(pair.slave);
    let (tx, rx) = std::sync::mpsc::channel();
    let capture = thread::spawn(move || {
        let mut parser = vt100::Parser::new(13, 47, 0);
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
    let wait = |predicate: fn(&Screen) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let screen = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if predicate(&screen) {
                return screen;
            }
        }
    };
    let live = wait(|screen| row(screen, 12).starts_with(" main"));
    assert_eq!(live.cell(12, 46).unwrap().bgcolor(), Color::Idx(8));
    assert_eq!(live.cell(0, 0).unwrap().bgcolor(), Color::Default);
    writer.write_all(b"\x02?").unwrap();
    let help = wait(|screen| screen.contents().contains("Commands"));
    assert!(help.hide_cursor());
    assert_eq!(help.cell(11, 46).unwrap().bgcolor(), Color::Idx(8));
    writer.write_all(b"\x1b").unwrap();
    wait(|screen| !screen.contents().contains("Commands") && !screen.hide_cursor());
    writer.write_all(b"\x02r").unwrap();
    wait(|screen| screen.contents().contains("rename pane"));
    writer
        .write_all(b"\x1b[200~attached-proof\x1b[201~\r")
        .unwrap();
    let named = wait(|screen| row(screen, 12).contains("attached-proof"));
    assert!(!named.hide_cursor());
    writer.write_all(b"\x02d").unwrap();
    eventually(|| attached.child.try_wait().unwrap().is_some());
    drop(writer);
    attached.master.take();
    let bytes = capture.join().unwrap();
    assert!(bytes.ends_with(b"\x1b[?1049l"));
    if let Ok(directory) = std::env::var("FUX_DESIGN_CAPTURE") {
        fs::write(PathBuf::from(&directory).join("frontend.ansi"), bytes).unwrap();
        fs::write(
            PathBuf::from(directory).join("frontend-help.txt"),
            plain(&help),
        )
        .unwrap();
    }
}
