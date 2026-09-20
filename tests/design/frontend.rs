use super::*;
use portable_pty::{Child as PtyChild, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};

#[test]
fn actual_attached_frontend_renders_bottom_chrome_and_consumes_prefix_keys() -> Outcome {
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
    let s = Server::start()?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 13,
            cols: 47,
            pixel_width: 0,
            pixel_height: 0,
        })
        .need()?;
    let mut reader = pair.master.try_clone_reader().need()?;
    let mut writer = pair.master.take_writer().need()?;
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fux"));
    command.arg("attach");
    command.env("FUX_ENDPOINT", &s.endpoint);
    let mut attached = Attached {
        child: {
            let _spawn = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
            pair.slave.spawn_command(command).need()?
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
            bytes.extend_from_slice(chunk.get(..n).unwrap_or_default());
            parser.process(chunk.get(..n).unwrap_or_default());
            if bytes.ends_with(b"\x1b[?2026l") && tx.send(parser.screen().clone()).is_err() {
                break;
            }
        }
        bytes
    });
    let wait = |predicate: fn(&Screen) -> bool| -> Result<Screen, Fail> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let screen = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
            if predicate(&screen) {
                return Ok(screen);
            }
        }
    };
    let live = wait(|screen| row(screen, 12).starts_with(" main"))?;
    assert_eq!(live.cell(12, 46).need()?.bgcolor(), Color::Idx(8));
    assert_eq!(live.cell(0, 0).need()?.bgcolor(), Color::Default);
    writer.write_all(b"\x02?").need()?;
    let help = wait(|screen| screen.contents().contains("Commands"))?;
    assert!(help.hide_cursor());
    assert_eq!(help.cell(11, 46).need()?.bgcolor(), Color::Idx(8));
    writer.write_all(b"\x1b").need()?;
    wait(|screen| !screen.contents().contains("Commands") && !screen.hide_cursor())?;
    writer.write_all(b"\x02r").need()?;
    wait(|screen| screen.contents().contains("rename pane"))?;
    writer
        .write_all(b"\x1b[200~attached-proof\x1b[201~\r")
        .need()?;
    let named = wait(|screen| row(screen, 12).contains("attached-proof"))?;
    assert!(!named.hide_cursor());
    writer.write_all(b"\x02d").need()?;
    eventually(|| Ok(attached.child.try_wait()?.is_some()))?;
    drop(writer);
    attached.master.take();
    let bytes = capture.join().map_err(|_| "capture thread panicked")?;
    assert!(bytes.ends_with(b"\x1b[?1049l"));
    if let Ok(directory) = std::env::var("FUX_DESIGN_CAPTURE") {
        fs::write(PathBuf::from(&directory).join("frontend.ansi"), bytes).need()?;
        fs::write(
            PathBuf::from(directory).join("frontend-help.txt"),
            plain(&help),
        )
        .need()?;
    }
    Ok(())
}
