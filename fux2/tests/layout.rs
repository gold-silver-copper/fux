//! What the stored layout promises when several writers meet, against the
//! real `fux2 layout`. Each test has a session of its own; none starts a pane.
use fux2::dir::Session;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

type Tested = Result<(), Box<dyn std::error::Error>>;

struct Lab {
    session: Session,
}

impl Lab {
    fn new() -> Result<Lab, Box<dyn std::error::Error>> {
        let root: PathBuf =
            fuxix::file::scratch()?.join(format!("f2l-{}", fux2::wire::PaneId::random()));
        fuxix::file::private_dir(&root)?;
        Ok(Lab {
            session: Session { root },
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fux2"));
        command
            .arg("layout")
            .args(args)
            .env("FUX2_DIR", &self.session.root);
        command
    }

    fn layout(&self, args: &[&str]) -> Result<Output, std::io::Error> {
        self.command(args).output()
    }

    fn version(&self) -> Result<u64, Box<dyn std::error::Error>> {
        Ok(fux2::layout::read(&self.session)?.version)
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.session.root);
    }
}

/// Two writers at once: every edit either lands or is refused out loud, and
/// the file holds every edit that landed.
#[test]
fn two_writers_at_once_lose_nothing_silently() -> Tested {
    let lab = Lab::new()?;
    let writers: Vec<_> = (0..2)
        .map(|writer| {
            let script = (0..10)
                .map(|n| format!("\"$0\" layout tab new --title w{writer}-{n} >/dev/null; echo $?"))
                .collect::<Vec<_>>()
                .join("\n");
            let mut command = Command::new("sh");
            command
                .args(["-c", &script, env!("CARGO_BIN_EXE_fux2")])
                .env("FUX2_DIR", &lab.session.root)
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            command.spawn()
        })
        .collect::<Result<_, _>>()?;
    let mut landed = 0u64;
    for writer in writers {
        let out = writer.wait_with_output()?;
        for code in String::from_utf8_lossy(&out.stdout).lines() {
            match code {
                "0" => landed = landed.saturating_add(1),
                // Busy: refused out loud, nothing changed.
                "6" => {}
                other => return Err(format!("a writer exited {other}").into()),
            }
        }
    }
    let layout = fux2::layout::read(&lab.session)?;
    assert_eq!(layout.version, landed);
    // The default tab, and one for each edit that landed.
    assert_eq!(
        u64::try_from(layout.tabs().count())?,
        landed.saturating_add(1)
    );
    assert!(landed >= 1);
    Ok(())
}

/// A writer killed at any moment leaves the old file or the new one, and
/// the next writer carries on.
#[test]
fn a_writer_killed_mid_write_leaves_a_whole_file() -> Tested {
    let lab = Lab::new()?;
    let out = lab.layout(&["tab", "new", "--title", "first"])?;
    assert!(out.status.success(), "{out:?}");
    for round in 0..30u64 {
        let mut child = lab
            .command(&["tab", "new"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        std::thread::sleep(Duration::from_micros(round.saturating_mul(300)));
        let _ = child.kill();
        let _ = child.wait();
        // Readable after every kill, whatever it interrupted.
        lab.version()?;
    }
    // A half-written new file left behind is never read, and is replaced.
    std::fs::write(
        lab.session.root.join("layout.new"),
        "fux2-layout 1\nversion",
    )?;
    let before = lab.version()?;
    let out = lab.layout(&["workspace", "new", "--title", "after"])?;
    assert!(out.status.success(), "{out:?}");
    assert_eq!(lab.version()?, before.saturating_add(1));
    Ok(())
}

/// `--if` refuses an edit made from an old look, with nothing changed; a
/// file that cannot be read is never written over, except by `reset`.
#[test]
fn an_edit_from_an_old_look_is_stale_and_a_bad_file_is_kept() -> Tested {
    let lab = Lab::new()?;
    let out = lab.layout(&["tab", "new", "--title", "one"])?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.starts_with("version 1\nmade t"), "{text}");
    lab.layout(&["tab", "rename", "one", "two"])?;
    let stale = lab.layout(&["tab", "rename", "two", "three", "--if", "1"])?;
    assert_eq!(stale.status.code(), Some(4), "{stale:?}");
    assert_eq!(lab.version()?, 2);
    let fresh = lab.layout(&["tab", "rename", "two", "three", "--if", "2"])?;
    assert!(fresh.status.success(), "{fresh:?}");

    let path = lab.session.root.join("layout");
    std::fs::write(&path, "not a layout\n")?;
    let refused = lab.layout(&["tab", "new"])?;
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert_eq!(std::fs::read_to_string(&path)?, "not a layout\n");
    let reset = lab.layout(&["reset"])?;
    assert!(reset.status.success(), "{reset:?}");
    assert_eq!(fux2::layout::read(&lab.session)?.tabs().count(), 1);
    Ok(())
}
