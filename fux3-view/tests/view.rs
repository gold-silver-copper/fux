//! The real viewer, driven as a person would: it runs in a pane of a fux3
//! session of the test's own (the harness), viewing another session; keys
//! go to its pane's `in`, and what it draws is read back from that pane's
//! `screen`.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

type Tested = Result<(), String>;

const PATIENCE: Duration = Duration::from_secs(20);

fn viewer() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fux3-view"))
}

/// The `fux3` beside the viewer, built first if this run did not build it
/// (`cargo test -p fux3-view` builds only the viewer).
fn fux3() -> Result<PathBuf, String> {
    let path = viewer().with_file_name("fux3");
    let target = (viewer()
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf))
    .ok_or("no target directory")?;
    if !path.exists() {
        let out = Command::new(env!("CARGO"))
            .args(["build", "-p", "fux3", "--locked", "--target-dir"])
            .arg(&target)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "building fux3: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    Ok(path)
}

/// Writes `bytes` to a pane's FIFO without waiting for a reader: one with
/// none (a holder gone) is an error, never a hang.
fn fifo(path: &Path, bytes: &[u8]) -> Tested {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = (std::fs::File::options()
        .write(true)
        .custom_flags(fuxix::file::NONBLOCK)
        .open(path))
    .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

fn private_dir(path: &Path) -> Tested {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_dir_all(path);
    std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())
}

/// Waits until `test` holds, saying `what` if it never does.
fn until(what: &str, mut test: impl FnMut() -> bool) -> Tested {
    let start = Instant::now();
    while !test() {
        if start.elapsed() > PATIENCE {
            return Err(format!("never: {what}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// Two sessions: `view` holds the panes viewed, `harness` the viewers.
struct Lab {
    base: PathBuf,
    view: PathBuf,
    harness: PathBuf,
    fux3: PathBuf,
}

impl Lab {
    fn new(name: &str, config: &str) -> Result<Lab, String> {
        let scratch = fuxix::file::scratch().map_err(|e| e.to_string())?;
        let base = scratch.join(format!("f3v-{name}-{}", std::process::id()));
        let (view, harness) = (base.join("v"), base.join("h"));
        private_dir(&base)?;
        private_dir(&view)?;
        private_dir(&harness)?;
        std::fs::create_dir_all(base.join("config/fux3")).map_err(|e| e.to_string())?;
        std::fs::write(base.join("config/fux3/view.grift"), config).map_err(|e| e.to_string())?;
        Ok(Lab {
            base,
            view,
            harness,
            fux3: fux3()?,
        })
    }

    fn run(&self, session: &Path, args: &[&str]) -> Result<Output, String> {
        let program = match args.first() {
            Some(&"--do") => viewer(),
            _ => self.fux3.clone(),
        };
        (Command::new(program)
            .args(args)
            .env("FUX3_DIR", session)
            .output())
        .map_err(|e| e.to_string())
    }

    /// Starts pane `name` in the session viewed, running sh, 80x24.
    fn start(&self, name: &str) -> Tested {
        let out = self.run(&self.view, &["start", name, "--size", "80x24", "--", "sh"])?;
        out.status
            .success()
            .then_some(())
            .ok_or_else(|| format!("{out:?}"))
    }

    /// Starts a viewer called `name`, `size` big, in the harness.
    fn viewer(&self, name: &str, size: &str) -> Tested {
        let viewer = viewer();
        let config = self.base.join("config");
        let env = [
            format!("FUX3_DIR={}", self.view.display()),
            format!("XDG_CONFIG_HOME={}", config.display()),
        ];
        let mut args = vec!["start", name, "--size", size, "--", "env"];
        args.extend(env.iter().map(String::as_str));
        let program = viewer.to_string_lossy().into_owned();
        args.push(&program);
        let out = self.run(&self.harness, &args)?;
        out.status
            .success()
            .then_some(())
            .ok_or_else(|| format!("{out:?}"))
    }

    fn read(&self, session: &Path, pane: &str, file: &str) -> String {
        std::fs::read_to_string(session.join(pane).join(file)).unwrap_or_default()
    }

    /// Types `bytes` into the viewer `name`.
    fn keys(&self, name: &str, bytes: &[u8]) -> Tested {
        fifo(&self.harness.join(name).join("in"), bytes)?;
        std::thread::sleep(Duration::from_millis(150));
        Ok(())
    }

    fn shows(&self, name: &str, text: &str) -> Tested {
        until(&format!("viewer {name} shows {text:?}"), || {
            self.read(&self.harness, name, "screen").contains(text)
        })
        .map_err(|e| format!("{e}:\n{}", self.read(&self.harness, name, "screen")))
    }

    fn size(&self, pane: &str) -> String {
        let status = self.read(&self.view, pane, "status");
        status
            .lines()
            .find_map(|l| l.strip_prefix("size "))
            .unwrap_or_default()
            .to_owned()
    }
}

impl Drop for Lab {
    /// Ends every pane of both sessions, through its `ctl`.
    fn drop(&mut self) {
        for session in [&self.harness, &self.view] {
            let names = std::fs::read_dir(session).into_iter().flatten().flatten();
            for entry in names.filter(|e| e.path().join("ctl").exists()) {
                let _ = fifo(&entry.path().join("ctl"), b"kill\n");
            }
        }
        let gone = until("every pane gone", || {
            [&self.harness, &self.view]
                .iter()
                .all(|s| self.run(s, &["ls"]).is_ok_and(|o| o.stdout.is_empty()))
        });
        if gone.is_ok() {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }
}

/// Writers changing the layout at once lose nothing: every change is made
/// on the file as the one before left it.
#[test]
fn two_writers_of_the_layout_lose_nothing() -> Tested {
    let lab = Lab::new("writers", "")?;
    let writers: Vec<_> = (0..4)
        .map(|w| {
            let view = lab.view.clone();
            std::thread::spawn(move || {
                (0..5).all(|n| {
                    let line = format!("tab new w{w}n{n}");
                    let out = Command::new(viewer())
                        .args(["--do", &line])
                        .env("FUX3_DIR", &view)
                        .output();
                    out.is_ok_and(|o| o.status.success())
                })
            })
        })
        .collect();
    for writer in writers {
        assert!(writer.join().is_ok_and(|done| done), "a writer was refused");
    }
    let layout =
        std::fs::read_to_string(lab.view.join(".view/layout")).map_err(|e| e.to_string())?;
    assert!(layout.contains("\nversion 20\n"), "{layout}");
    for (w, n) in (0..4).flat_map(|w| (0..5).map(move |n| (w, n))) {
        assert!(
            layout.contains(&format!("\"w{w}n{n}\"")),
            "w{w}n{n} lost:\n{layout}"
        );
    }
    Ok(())
}

/// A wrong or oversized layout file is refused out loud: the viewer keeps
/// showing the last good one, and no change is written over it.
#[test]
fn a_wrong_layout_is_refused_and_the_last_good_one_kept() -> Tested {
    let lab = Lab::new("wrong", "")?;
    lab.start("left")?;
    lab.start("right")?;
    let out = lab.run(&lab.view, &["--do", "tab new spare"])?;
    assert!(out.status.success(), "{out:?}");
    lab.viewer("v", "100x30")?;
    lab.shows("v", "\u{2502}")?;
    let file = lab.view.join(".view/layout");
    for wrong in [
        "fux3-layout 1\nversion x\n".to_owned(),
        "x".repeat(70 << 10),
    ] {
        std::fs::write(&file, &wrong).map_err(|e| e.to_string())?;
        let out = lab.run(&lab.view, &["--do", "zoom left"])?;
        assert!(!out.status.success(), "a change was made over a wrong file");
        assert_eq!(
            std::fs::read_to_string(&file).map_err(|e| e.to_string())?,
            wrong
        );
        lab.shows("v", "layout refused")?;
        // The last good layout still shows both panes, side by side.
        assert!(lab.read(&lab.harness, "v", "screen").contains('\u{2502}'));
    }
    Ok(())
}

/// Opening a viewer, moving the focus and resizing the viewer's terminal
/// change no pane's size.
#[test]
fn viewing_focusing_and_resizing_the_viewer_resize_no_pane() -> Tested {
    let lab = Lab::new("sizes", "")?;
    lab.start("a")?;
    lab.start("b")?;
    lab.viewer("v", "100x30")?;
    lab.shows("v", "\u{2502}")?;
    lab.keys("v", b"\x02o")?;
    lab.keys("v", b"\x02h")?;
    fifo(&lab.harness.join("v/ctl"), b"resize 120 40\n")?;
    until("the viewer's pane resized", || {
        lab.read(&lab.harness, "v", "status")
            .contains("size 120 40")
    })?;
    lab.keys("v", b"\x02o")?;
    std::thread::sleep(Duration::from_millis(300));
    for pane in ["a", "b"] {
        assert_eq!(lab.size(pane), "80 24", "{pane} was resized by viewing");
    }
    Ok(())
}

/// A binding that loops forever is stopped, and detach works while it
/// runs; the panes outlive the viewer.
#[test]
fn a_looping_binding_is_stopped_and_detach_always_works() -> Tested {
    let config = "(define! spin (lambda! () (spin)))\n(bind-key \"F8\" \"spin\" \"Mine\" spin)\n";
    let lab = Lab::new("loop", config)?;
    lab.start("only")?;
    lab.viewer("v", "100x30")?;
    lab.shows("v", "$")?;
    lab.keys("v", b"\x1b[19~")?;
    lab.shows("v", "F8: interrupted")?;
    lab.keys("v", b"\x1b[19~")?;
    lab.keys("v", b"\x02d")?;
    until("the viewer left", || {
        lab.read(&lab.harness, "v", "status")
            .contains("state exited 0")
    })?;
    let listed = lab.run(&lab.view, &["ls"])?;
    assert_eq!(String::from_utf8_lossy(&listed.stdout).trim(), "only");
    Ok(())
}
