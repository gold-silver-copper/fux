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

    /// Starts a viewer called `name`, `size` big, in the harness, showing
    /// the workspaces `shown` names (none: `main`).
    fn viewer(&self, name: &str, size: &str, shown: &[&str]) -> Tested {
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
        args.extend(shown);
        let out = self.run(&self.harness, &args)?;
        out.status
            .success()
            .then_some(())
            .ok_or_else(|| format!("{out:?}"))
    }

    /// A file of pane `pane` of workspace `main`.
    fn read(&self, session: &Path, pane: &str, file: &str) -> String {
        std::fs::read_to_string(session.join("main").join(pane).join(file)).unwrap_or_default()
    }

    /// Types `bytes` into the viewer `name`.
    fn keys(&self, name: &str, bytes: &[u8]) -> Tested {
        fifo(&self.harness.join("main").join(name).join("in"), bytes)?;
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
            let spaces = std::fs::read_dir(session).into_iter().flatten().flatten();
            let panes = spaces.flat_map(|space| {
                std::fs::read_dir(space.path())
                    .into_iter()
                    .flatten()
                    .flatten()
            });
            for pane in panes.filter(|e| e.path().join("ctl").exists()) {
                let _ = fifo(&pane.path().join("ctl"), b"kill\n");
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
        std::fs::read_to_string(lab.view.join("main/.view/layout")).map_err(|e| e.to_string())?;
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
    lab.viewer("v", "250x30", &[])?;
    lab.shows("v", "\u{2502}")?;
    let file = lab.view.join("main/.view/layout");
    // A layout good but for its size: 64 tabs, each with a long filter.
    let test: String = std::iter::repeat_n('x', 60).collect();
    let tests: String = std::iter::repeat_n(format!(" \"k={test}\""), 16).collect();
    let filter = format!("  match{tests}\n");
    let big: String = (1..=64)
        .map(|n| format!("tab t{n} \"tab {n}\"\n{filter}"))
        .collect();
    let big = format!("fux3-layout 2\nversion 1\nnext 65\n{big}");
    assert!(big.len() > 64 << 10);
    for (wrong, why) in [
        ("fux3-layout 1\nversion x\n".to_owned(), "line 1: expected"),
        (big, "over 65536 bytes"),
    ] {
        std::fs::write(&file, &wrong).map_err(|e| e.to_string())?;
        let out = lab.run(&lab.view, &["--do", "zoom left"])?;
        assert!(!out.status.success(), "a change was made over a wrong file");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.contains(why), "--do said {said:?}, not {why:?}");
        assert_eq!(
            std::fs::read_to_string(&file).map_err(|e| e.to_string())?,
            wrong
        );
        lab.shows("v", why)?;
        // The last good layout still shows both panes, side by side.
        assert!(lab.read(&lab.harness, "v", "screen").contains('\u{2502}'));
    }
    // A layout file removed is refused too: no change writes a new one
    // over the layout the viewer still shows; only `reset` does.
    std::fs::remove_file(&file).map_err(|e| e.to_string())?;
    let out = lab.run(&lab.view, &["--do", "tab new unexpected"])?;
    assert!(
        !out.status.success(),
        "a change was made with the file gone"
    );
    assert!(!file.exists());
    let out = lab.run(&lab.view, &["--do", "reset"])?;
    assert!(out.status.success(), "{out:?}");
    assert!(file.exists());
    Ok(())
}

/// Opening a viewer, moving the focus and resizing the viewer's terminal
/// change no pane's size.
#[test]
fn viewing_focusing_and_resizing_the_viewer_resize_no_pane() -> Tested {
    let lab = Lab::new("sizes", "")?;
    lab.start("a")?;
    lab.start("b")?;
    lab.viewer("v", "100x30", &[])?;
    lab.shows("v", "\u{2502}")?;
    lab.keys("v", b"\x02o")?;
    lab.keys("v", b"\x02h")?;
    fifo(&lab.harness.join("main/v/ctl"), b"resize 120 40\n")?;
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

/// A binding that loops forever is stopped, a chord that leads to itself
/// (bound to it, or run by its own binding) is refused, a redefined command
/// is what its default key runs, and detach works
/// while a binding runs; the panes outlive the viewer.
#[test]
fn a_looping_binding_is_stopped_and_detach_always_works() -> Tested {
    let config = "(define! spin (lambda! () (spin)))\n\
        (bind-key \"F8\" \"spin\" \"Mine\" spin)\n\
        (bind-verb \"F9\" \"itself\" \"Mine\" \"chord\" \"F9\")\n\
        (bind-key \"F7\" \"again\" \"Mine\" (lambda! () (act \"chord\" \"F7\")))\n\
        (define! history (lambda! () (say \"my own history\")))\n";
    let lab = Lab::new("loop", config)?;
    lab.start("only")?;
    lab.viewer("v", "100x30", &[])?;
    lab.shows("v", "$")?;
    lab.keys("v", b"\x1b[20~")?;
    lab.shows("v", "F9: refused")?;
    lab.keys("v", b"\x1b[18~")?;
    lab.shows("v", "F7: refused")?;
    // A default key calls the command by name when pressed: redefined, it
    // runs the new one.
    lab.keys("v", b"\x02c")?;
    lab.shows("v", "my own history")?;
    lab.keys("v", b"\x1b[19~")?;
    lab.shows("v", "F8: interrupted")?;
    lab.keys("v", b"\x1b[19~")?;
    // Detach comes at once, not once the binding is stopped (its limit is
    // 5 s, and 1 s more before it is forced).
    let asked = Instant::now();
    fifo(&lab.harness.join("main/v/in"), b"\x02d")?;
    until("the viewer left", || {
        lab.read(&lab.harness, "v", "status")
            .contains("state exited 0")
    })?;
    let took = asked.elapsed();
    assert!(took < Duration::from_secs(2), "detach took {took:?}");
    let listed = lab.run(&lab.view, &["ls"])?;
    assert_eq!(String::from_utf8_lossy(&listed.stdout).trim(), "main/only");
    Ok(())
}

/// A layout command written to one workspace's `.view/ctl` changes that
/// workspace's layout and not another's, with a viewer on each at once;
/// and a binding that switches workspace can't send to a pane of the same
/// name in the one it switched to.
#[test]
fn a_ctl_line_changes_only_the_layout_of_its_workspace() -> Tested {
    let config = "(bind-key \"F6\" \"over\" \"Mine\" \
        (lambda! () (show-workspace \"two\") (send \"a\" \"echo cro''ssed\\r\")))\n";
    let lab = Lab::new("spaces", config)?;
    lab.start("one/a")?;
    lab.start("two/b")?;
    lab.start("two/a")?;
    lab.viewer("v1", "100x30", &["one"])?;
    lab.viewer("v2", "100x30", &["two"])?;
    let space = |name: &str| lab.view.join(name).join(".view");
    for (name, line) in [("one", "tab new mine\n"), ("two", "tab new theirs\n")] {
        let ctl = space(name).join("ctl");
        until(&format!("{name}'s viewer serves its ctl"), || {
            fifo(&ctl, line.as_bytes()).is_ok()
        })?;
    }
    let layout =
        |name: &str| std::fs::read_to_string(space(name).join("layout")).unwrap_or_default();
    until("each workspace's layout has its own tab", || {
        layout("one").contains("\"mine\"") && layout("two").contains("\"theirs\"")
    })?;
    assert!(!layout("one").contains("theirs"), "{}", layout("one"));
    assert!(!layout("two").contains("mine"), "{}", layout("two"));
    lab.keys("v1", b"\x1b[17~")?;
    lab.shows("v1", "workspace one, no longer shown")?;
    let screen = |space: &str| {
        std::fs::read_to_string(lab.view.join(space).join("a/screen")).unwrap_or_default()
    };
    assert!(!screen("two").contains("crossed"), "{}", screen("two"));
    assert!(!screen("one").contains("crossed"), "{}", screen("one"));
    Ok(())
}

/// A paste reaches the focused pane, and its file stays for its whole wait
/// even when other writers' commands move the pane's `commands` count on
/// (the viewer can't tell which command was its paste).
#[test]
fn a_paste_file_outlives_other_writers_commands() -> Tested {
    let lab = Lab::new("paste", "")?;
    lab.start("only")?;
    lab.viewer("v", "100x30", &[])?;
    lab.shows("v", "$")?;
    lab.keys("v", b"\x1b[200~echo pas''ted\x1b[201~\r")?;
    until("the paste ran", || {
        lab.read(&lab.view, "only", "screen").contains("pasted")
    })?;
    let ctl = lab.view.join("main/only/ctl");
    for _ in 0..3 {
        fifo(&ctl, b"cell 10 20\n")?;
    }
    std::thread::sleep(Duration::from_millis(500));
    let pastes = std::fs::read_dir(lab.view.join("main/.view"))
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("paste-"))
        .count();
    assert_eq!(pastes, 1, "the paste file went before its wait ended");
    Ok(())
}

/// Two viewers of one workspace split no `.view/ctl` line between them:
/// one reads it at a time, so a line that crosses a read's end still runs
/// whole.
#[test]
fn two_viewers_of_a_workspace_split_no_ctl_line() -> Tested {
    let lab = Lab::new("twoctl", "")?;
    lab.start("only")?;
    lab.viewer("v1", "100x30", &[])?;
    lab.viewer("v2", "100x30", &[])?;
    let view = lab.view.join("main/.view");
    let ctl = view.join("ctl");
    until("a viewer serves ctl", || fifo(&ctl, b"\n").is_ok())?;
    // Each a line just past a 4096-byte read: a second reader would take
    // its end alone.
    for n in 0..8 {
        let mut bytes = vec![b'\n'; 4094];
        bytes.extend(format!("tab new kept{n}\n").as_bytes());
        fifo(&ctl, &bytes)?;
        std::thread::sleep(Duration::from_millis(50));
    }
    let layout = || std::fs::read_to_string(view.join("layout")).unwrap_or_default();
    until("every line ran", || {
        (0..8).all(|n| layout().contains(&format!("\"kept{n}\"")))
    })
    .map_err(|e| format!("{e}\n{}", layout()))?;
    let errors = std::fs::read_to_string(view.join("errors")).unwrap_or_default();
    assert!(errors.is_empty(), "lines were refused:\n{errors}");
    Ok(())
}
