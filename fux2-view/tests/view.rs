//! The real viewer on a real terminal this test hosts.
use fux_vt::{Parser, Size};
use fuxix::Errno;
use fuxix::poll::{Events, PollFd, poll};
use fuxix::process::{Job, Reach};
use fuxix::pty::Master;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

type Tested = Result<(), Box<dyn std::error::Error>>;

fn soon(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait).unwrap_or(now)
}

const PATIENCE: Duration = Duration::from_secs(20);

fn beside_tests(name: &str) -> Result<PathBuf, std::io::Error> {
    let exe = std::env::current_exe()?;
    let deps = exe.parent().and_then(Path::parent).map(Path::to_path_buf);
    let dir = deps.ok_or_else(|| std::io::Error::other("no target directory"))?;
    Ok(dir.join(name))
}

fn shell() -> &'static str {
    "sh"
}

struct Lab {
    root: PathBuf,
    config: PathBuf,
}

impl Lab {
    fn new(config: &str) -> Result<Lab, Box<dyn std::error::Error>> {
        let unique = fux2::wire::PaneId::random();
        let base = fuxix::file::scratch()?;
        let root = base.join(format!("f2v-{unique}"));
        fuxix::file::private_dir(&root)?;
        let home = base.join(format!("f2c-{unique}"));
        std::fs::create_dir_all(home.join("fux2"))?;
        std::fs::write(home.join("fux2").join("view.grift"), config)?;
        // The shell a pane the viewer starts runs: it says which pane it is.
        let fresh = home.join("fresh.sh");
        std::fs::write(
            &fresh,
            format!("#!/bin/sh\necho \"fresh-$FUX2_PANE\"\nexec {}\n", shell()),
        )?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Lab { root, config: home })
    }

    fn fux2(&self, args: &[&str]) -> Result<Output, std::io::Error> {
        Command::new(beside_tests("fux2")?)
            .args(args)
            .env("FUX2_DIR", &self.root)
            .output()
    }

    /// Starts a shell that first prints `pane-NAME`, so a viewer showing
    /// it can be seen to: fux2 draws no titles.
    fn start(&self, name: &str) -> Tested {
        self.start_with(name, &[])
    }

    fn start_with(&self, name: &str, labels: &[&str]) -> Tested {
        let mut args = vec!["start", "--name", name];
        for label in labels {
            args.extend(["--label", label]);
        }
        let hello = format!("echo pane-{name}; exec {}", shell());
        args.extend(["--", shell(), "-c", &hello]);
        let out = self.fux2(&args)?;
        assert!(out.status.success(), "{out:?}");
        Ok(())
    }

    fn screen(&self, name: &str) -> Result<String, std::io::Error> {
        let out = self.fux2(&["read", name])?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn pane_shows(&self, name: &str, text: &str) -> Tested {
        let until = soon(PATIENCE);
        loop {
            let screen = self.screen(name)?;
            if screen.lines().any(|line| line.trim_end() == text) {
                return Ok(());
            }
            if Instant::now() > until {
                return Err(format!("{name} never showed {text:?}: {screen}").into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Lab {
    /// Ends every pane, by its daemon's pid if `kill` fails, and removes the
    /// session only once none answers.
    fn drop(&mut self) {
        let names = || -> Vec<String> {
            let entries = std::fs::read_dir(&self.root)
                .into_iter()
                .flatten()
                .flatten();
            entries
                .filter_map(|entry| {
                    let file = entry.file_name().to_string_lossy().into_owned();
                    file.strip_prefix("p-").map(str::to_owned)
                })
                .collect()
        };
        for name in names() {
            let pid = self.fux2(&["ls", "--long"]).ok().and_then(|out| {
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                let line = text
                    .lines()
                    .find(|line| line.split('\t').next() == Some(&name))?;
                line.split('\t').nth(2).map(str::to_owned)
            });
            let killed = self
                .fux2(&["kill", &name])
                .is_ok_and(|out| out.status.success());
            if !killed && let Some(pid) = pid {
                let _ = Command::new("kill").args(["-9", &pid]).output();
            }
        }
        let listed = self.fux2(&["ls"]).map(|out| out.stdout).unwrap_or_default();
        if listed.is_empty() {
            let _ = std::fs::remove_dir_all(&self.root);
        } else {
            let mut err = std::io::stderr();
            let left = String::from_utf8_lossy(&listed).into_owned();
            let _ = writeln!(err, "panes outlived the test: {left}");
        }
        let _ = std::fs::remove_dir_all(&self.config);
    }
}

/// A viewer on a terminal of this test's: what it draws lands in a parser.
struct Viewer {
    master: Master,
    job: Option<Job>,
    parser: Parser,
}

impl Viewer {
    fn start(lab: &Lab) -> Result<Viewer, Box<dyn std::error::Error>> {
        Viewer::with_args(lab, &[])
    }

    fn with_args(
        lab: &Lab,
        args: &[std::ffi::OsString],
    ) -> Result<Viewer, Box<dyn std::error::Error>> {
        Viewer::with_env(lab, args, &[])
    }

    fn with_env(
        lab: &Lab,
        args: &[std::ffi::OsString],
        env: &[(&str, &str)],
    ) -> Result<Viewer, Box<dyn std::error::Error>> {
        let size = Size::new(30, 100)?;
        let (master, slave) = fuxix::pty::open(size.nonzero().into())?;
        let viewer = beside_tests("fux2-view")?;
        let mut argv = vec![viewer.into_os_string()];
        argv.extend_from_slice(args);
        let job = fux2::sys::spawn(slave, &argv, |command| {
            command
                .env("FUX2_DIR", &lab.root)
                .env("XDG_CONFIG_HOME", &lab.config)
                .env("SHELL", lab.config.join("fresh.sh"))
                .env("TERM", "xterm-256color")
                .envs(env.iter().copied());
        })?;
        Ok(Viewer {
            master,
            job: Some(job),
            parser: Parser::with_options(size, 0, fux_vt::Options::new())?,
        })
    }

    /// Takes what the viewer drew, for up to `wait`.
    fn pump(&mut self, wait: Duration) -> Tested {
        let mut fds = [PollFd::new(&self.master, Events::IN)];
        match poll(&mut fds, Some(wait)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(errno) => return Err(errno.into()),
        }
        let mut buffer = [0u8; 16 << 10];
        loop {
            match fuxix::io::read(&self.master, &mut buffer) {
                Ok(0) | Err(Errno::AGAIN | Errno::IO) => return Ok(()),
                Ok(n) => self
                    .parser
                    .process_all(buffer.get(..n).unwrap_or_default(), &mut ()),
                Err(Errno::INTR) => {}
                Err(errno) if errno.is_peer_gone() => return Ok(()),
                Err(errno) => return Err(errno.into()),
            }
        }
    }

    fn text(&self) -> String {
        let window = self.parser.screen().window();
        let rows: Vec<String> = window.iter().map(|(_, row)| row.text()).collect();
        rows.join("\n")
    }

    /// The bar, as the prefix shows it; the prefix's menu is then closed.
    fn bar(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        self.keys(b"\x02")?;
        self.shows("C-b \u{2026}")?;
        let bar = self.text().lines().last().unwrap_or_default().to_owned();
        self.keys(b"\x1b")?;
        let until = soon(PATIENCE);
        while self.text().contains("C-b \u{2026}") {
            assert!(Instant::now() < until, "the menu stayed open");
            self.pump(Duration::from_millis(20))?;
        }
        Ok(bar)
    }

    fn shows(&mut self, wanted: &str) -> Tested {
        let until = soon(PATIENCE);
        while !self.text().contains(wanted) {
            if Instant::now() > until {
                return Err(format!("never drew {wanted:?}:\n{}", self.text()).into());
            }
            self.pump(Duration::from_millis(50))?;
        }
        Ok(())
    }

    fn keys(&mut self, bytes: &[u8]) -> Tested {
        let mut rest = bytes;
        let until = soon(PATIENCE);
        while !rest.is_empty() {
            match fuxix::io::write(&self.master, rest) {
                Ok(n) => rest = rest.get(n..).unwrap_or_default(),
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(errno) => return Err(errno.into()),
            }
            self.pump(Duration::from_millis(5))?;
            if Instant::now() > until {
                return Err("the terminal took no keys".into());
            }
        }
        Ok(())
    }

    fn ended(&mut self) -> Result<bool, Box<dyn std::error::Error>> {
        let until = soon(PATIENCE);
        loop {
            self.pump(Duration::from_millis(20))?;
            let job = self.job.as_ref().ok_or("no job")?;
            if job.ended()?.is_some() {
                return Ok(true);
            }
            if Instant::now() > until {
                return Ok(false);
            }
        }
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            let _ = job.kill(Reach::Session);
            let until = soon(Duration::from_secs(2));
            while !matches!(job.ended(), Ok(Some(_)) | Err(_)) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = job.reap();
        }
    }
}

const CONFIG: &str = r#"
(define! from-binding
  (lambda! () (send (focused) "echo from-binding\r")))
(bind-key "F7" "type a line" "Mine" from-binding)
(define! spin (lambda! () (spin)))
(bind-key "F8" "spin forever" "Mine"
  (lambda! () (send (focused) "echo spinning\r") (spin)))
(bind-verb "F9" "an alias" "Mine" "chord" "F10")
(bind-verb "F10" "its alias back" "Mine" "chord" "F9")
(unbind-key "C-b n")
(bind-key "C-b F5" "my own command" "Mine" from-binding)
(define! command-line (lambda! () (say "my-own-prompt")))
"#;

/// Two panes split, keys to the focused one, a rebound key running the
/// user's own definition; a binding that never ends freezes nothing; and
/// the viewer killed outright leaves both programs steerable.
#[test]
fn the_viewer_steers_panes_and_they_outlive_it() -> Tested {
    let lab = Lab::new(CONFIG)?;
    lab.start("left")?;
    lab.start("right")?;
    let mut viewer = Viewer::start(&lab)?;
    viewer.shows("pane-left")?;
    viewer.shows("pane-right")?;

    viewer.keys(b"echo typed-left\r")?;
    lab.pane_shows("left", "typed-left")?;
    viewer.shows("typed-left")?;

    viewer.keys(b"\x1b[18~")?;
    lab.pane_shows("left", "from-binding")?;

    viewer.keys(b"\x02o")?;
    viewer.keys(b"echo typed-right\r")?;
    lab.pane_shows("right", "typed-right")?;
    assert!(!lab.screen("left")?.contains("typed-right"));

    // The loop holds only the language's thread: keys still go through.
    viewer.keys(b"\x1b[19~")?;
    viewer.keys(b"echo while-looping\r")?;
    lab.pane_shows("right", "while-looping")?;

    // The configuration took C-b n away: it is refused out loud.
    viewer.keys(b"\x02n")?;
    viewer.shows("C-b n is not bound")?;
    viewer.keys(b"\x1b")?;

    // The prefix shows the keys under their headings, the configuration's
    // own among them (at the end: End scrolls there); Escape closes it.
    viewer.keys(b"\x02")?;
    viewer.shows("split side by side")?;
    viewer.shows("Workspaces")?;
    viewer.keys(b"\x1b[F")?;
    viewer.shows("F5  my own command")?;
    viewer.keys(b"\x1b")?;
    // A default key runs the command as the configuration redefined it.
    viewer.keys(b"\x02e")?;
    viewer.shows("my-own-prompt")?;

    // An alias that leads back to itself is stopped, out loud.
    viewer.keys(b"\x1b[20~")?;
    viewer.shows("leads back to itself")?;

    let job = viewer.job.as_ref().ok_or("no job")?;
    let _ = job.kill(Reach::Session);
    assert!(viewer.ended()?, "the viewer did not die");
    for (pane, line) in [("left", "after-left"), ("right", "after-right")] {
        let typed = format!("echo {line}\\n");
        let out = lab.fux2(&["send", pane, &typed])?;
        assert!(out.status.success(), "{out:?}");
        lab.pane_shows(pane, line)?;
    }
    Ok(())
}

/// Detaching works while a binding is stuck, and leaves the panes alone.
#[test]
fn detach_works_while_a_binding_runs() -> Tested {
    let lab = Lab::new(CONFIG)?;
    lab.start("only")?;
    let mut viewer = Viewer::start(&lab)?;
    viewer.shows("pane-only")?;
    // Repeating keys give the prefix back: C-b d there detaches too, so
    // first leave the mode that way and come back.
    viewer.keys(b"\x02rh")?;
    viewer.bar()?;
    viewer.keys(b"\x1b[19~")?;
    // The binding has started, and spins.
    lab.pane_shows("only", "spinning")?;
    viewer.keys(b"\x02rh")?;
    viewer.keys(b"\x02d")?;
    assert!(viewer.ended()?, "the viewer did not leave");
    let listed = lab.fux2(&["ls"])?;
    assert_eq!(String::from_utf8_lossy(&listed.stdout).trim(), "only");
    Ok(())
}

fn layout(lab: &Lab, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let mut all = vec!["layout"];
    all.extend_from_slice(args);
    let out = lab.fux2(&all)?;
    assert!(out.status.success(), "layout {args:?}: {out:?}");
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn size(lab: &Lab, name: &str) -> Result<String, std::io::Error> {
    let out = lab.fux2(&["ls", "--long"])?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text
        .lines()
        .find(|line| line.split('\t').next() == Some(name));
    Ok(line
        .and_then(|l| l.split('\t').nth(3))
        .unwrap_or("")
        .to_owned())
}

/// The stored layout's tabs choose what is shown, a bad edit of the file
/// leaves them as they were, and only the fit key changes a pane's size.
#[test]
fn stored_tabs_choose_what_is_shown_and_only_fit_resizes() -> Tested {
    let lab = Lab::new("")?;
    lab.start_with("one", &["group=a"])?;
    lab.start_with("two", &["group=b"])?;
    layout(
        &lab,
        &["tab", "new", "--title", "first", "--match", "group=a"],
    )?;
    layout(
        &lab,
        &["tab", "new", "--title", "second", "--match", "group=b"],
    )?;
    layout(&lab, &["tab", "close", "all"])?;
    let mut viewer = Viewer::start(&lab)?;
    viewer.shows("pane-one")?;
    assert!(!viewer.text().contains("pane-two"), "{}", viewer.text());
    assert_eq!(size(&lab, "one")?, "80x24", "viewing resized a pane");

    viewer.keys(b"\x02n")?;
    viewer.shows("pane-two")?;
    let file = lab.root.join("layout");
    let good = std::fs::read_to_string(&file)?;
    std::fs::write(&file, "fux2-layout 1\nversion x\n")?;
    viewer.shows("expected version N")?;
    assert!(viewer.text().contains("pane-two"), "{}", viewer.text());
    std::fs::write(&file, good)?;

    // A prompt's key and what is typed for it, all in one read, reach the
    // prompt, not the pane.
    viewer.keys(b"\x02e\x1b[200~(string-join (list \"panel\" \"got-it\") \"-\")\x1b[201~\r")?;
    viewer.shows("panel-got-it")?;
    assert!(
        !lab.screen("two")?.contains("got-it"),
        "the pane got the prompt's keys"
    );

    viewer.keys(b"\x02f")?;
    let until = soon(PATIENCE);
    while size(&lab, "two")? != "100x30" {
        assert!(Instant::now() < until, "fit gave {}", size(&lab, "two")?);
        viewer.pump(Duration::from_millis(20))?;
    }
    assert_eq!(size(&lab, "one")?, "80x24");
    Ok(())
}

/// A layout edit from the command line shows in every running viewer, a
/// split made in one viewer shows in the other, and a pane that ended keeps
/// its tile while a new pane of its name gets its own.
#[test]
fn every_viewer_shows_the_one_stored_layout() -> Tested {
    let lab = Lab::new("")?;
    lab.start("left")?;
    lab.start("right")?;
    let mut first = Viewer::start(&lab)?;
    let mut second = Viewer::start(&lab)?;
    for viewer in [&mut first, &mut second] {
        viewer.shows("pane-left")?;
        viewer.shows("pane-right")?;
    }
    layout(&lab, &["zoom", "right"])?;
    for viewer in [&mut first, &mut second] {
        let until = soon(PATIENCE);
        while viewer.text().contains("pane-left") {
            assert!(Instant::now() < until, "not zoomed:\n{}", viewer.text());
            viewer.pump(Duration::from_millis(20))?;
        }
    }
    layout(&lab, &["zoom", "right"])?;
    first.shows("pane-left")?;

    // Split by hand in the first viewer: the second shows it too.
    first.keys(b"\x02v")?;
    first.shows("fresh-shell-1")?;
    first.keys(b"echo in-the-split\r")?;
    lab.pane_shows("shell-1", "in-the-split")?;
    second.shows("in-the-split")?;

    // An ended pane keeps its tile; its name, reused, takes another.
    let out = lab.fux2(&["kill", "right"])?;
    assert!(out.status.success(), "{out:?}");
    second.shows(" right missing ")?;
    lab.start("right")?;
    second.shows("pane-right")?;
    assert!(
        second.text().contains(" right missing "),
        "{}",
        second.text()
    );
    let shown = layout(&lab, &["show"])?;
    assert_eq!(shown.matches("pane right@").count(), 2, "{shown}");
    Ok(())
}

/// Opening a viewer and moving the focus resize nothing; a split resizes
/// exactly the panes whose tiles it changed, to those tiles.
#[test]
fn only_a_split_resizes_and_only_what_it_changed() -> Tested {
    let lab = Lab::new("")?;
    lab.start("a")?;
    lab.start("b")?;
    let mut viewer = Viewer::start(&lab)?;
    viewer.shows("pane-a")?;
    viewer.shows("pane-b")?;
    viewer.keys(b"\x02o")?;
    viewer.keys(b"\x02o")?;
    for name in ["a", "b"] {
        assert_eq!(size(&lab, name)?, "80x24", "{name} was resized by viewing");
    }
    viewer.keys(b"\x02o")?;
    // b is focused. Away to another tab and back, the focus is still b.
    layout(&lab, &["tab", "new", "--title", "elsewhere"])?;
    let until = soon(PATIENCE);
    while !viewer.bar()?.contains(" elsewhere ") {
        assert!(Instant::now() < until, "the new tab never showed");
    }
    viewer.keys(b"\x02n")?;
    viewer.shows("no panes here")?;
    viewer.keys(b"\x02b")?;
    viewer.shows("pane-b")?;
    viewer.keys(b"echo still-on-b\r")?;
    lab.pane_shows("b", "still-on-b")?;
    // b is focused: split it, side by side.
    viewer.keys(b"\x02v")?;
    // In 100 columns: a keeps 49; b and the new pane share b's 50, one
    // column of it their border.
    let until = soon(PATIENCE);
    while size(&lab, "b")? != "24x30" || size(&lab, "shell-1")? != "25x30" {
        let sizes = (size(&lab, "b")?, size(&lab, "shell-1")?);
        assert!(
            Instant::now() < until,
            "b and shell-1 are {sizes:?}:\n{}",
            viewer.text()
        );
        viewer.pump(Duration::from_millis(20))?;
    }
    assert_eq!(size(&lab, "a")?, "80x24", "a's tile did not change");
    Ok(())
}

/// A pager that shows what it is given and stays until a key is pressed.
fn pager(lab: &Lab) -> Result<String, std::io::Error> {
    let script = lab.config.join("pager.sh");
    std::fs::write(&script, "cat\nread line < /dev/tty\n")?;
    Ok(format!("sh {}", script.display()))
}

/// Workspaces and tabs change what is shown and who gets the keys; a pane
/// started from a tab appears there and nowhere else.
#[test]
fn workspaces_and_tabs_choose_what_is_shown() -> Tested {
    let lab = Lab::new("")?;
    lab.start_with("alpha", &["group=a"])?;
    lab.start_with("beta", &["group=b"])?;
    layout(&lab, &["workspace", "rename", "main", "first"])?;
    layout(&lab, &["tab", "rename", "all", "ta"])?;
    layout(&lab, &["tab", "match", "ta", "group=a"])?;
    let made = layout(&lab, &["workspace", "new", "--title", "second"])?;
    assert!(made.contains("made w"), "{made}");
    layout(&lab, &["tab", "rename", "1", "tb"])?;
    layout(&lab, &["tab", "match", "tb", "group=b"])?;
    let mut viewer = Viewer::start(&lab)?;
    viewer.shows("pane-alpha")?;
    let bar = viewer.bar()?;
    assert!(bar.starts_with(" first  ta "), "{bar:?}");

    viewer.keys(b"\x02wl")?;
    viewer.shows("pane-beta")?;
    let bar = viewer.bar()?;
    assert!(bar.starts_with(" second  tb "), "{bar:?}");
    // The workspace list starts on the workspace shown: Enter keeps it.
    viewer.keys(b"\x02wg")?;
    viewer.shows("workspaces")?;
    viewer.keys(b"\r")?;
    let bar = viewer.bar()?;
    assert!(bar.starts_with(" second  tb "), "{bar:?}");
    assert!(!viewer.text().contains("pane-alpha"), "{}", viewer.text());
    viewer.keys(b"echo to-beta\r")?;
    lab.pane_shows("beta", "to-beta")
        .map_err(|e| format!("{e}\n{}", viewer.text()))?;
    assert!(!lab.screen("alpha")?.contains("to-beta"));

    viewer.keys(b"\x02v")?;
    viewer.shows("fresh-shell-1")?;
    viewer.keys(b"echo to-new\r")?;
    lab.pane_shows("shell-1", "to-new")?;
    viewer.shows("to-new")?;

    viewer.keys(b"\x02wh")?;
    viewer.shows("pane-alpha")?;
    assert!(!viewer.text().contains("to-new"), "{}", viewer.text());
    Ok(())
}

/// The history key opens a pager in a pane of its own; when it ends, the
/// pane is gone and keys go where they went before.
#[test]
fn the_history_pane_goes_away_and_gives_the_focus_back() -> Tested {
    let lab = Lab::new("")?;
    lab.start("work")?;
    let pager = pager(&lab)?;
    let mut viewer = Viewer::with_env(&lab, &[], &[("PAGER", &pager)])?;
    viewer.shows("pane-work")?;
    viewer.keys(b"echo marker-line\r")?;
    lab.pane_shows("work", "marker-line")?;
    viewer.keys(b"\x02c")?;
    // The pager's pane below, showing the history, and holding the focus.
    viewer.shows("\u{2500}\u{2500}\u{2500}")?;
    lab.pane_shows("history-1", "marker-line")?;
    viewer.keys(b"\r")?;
    let until = soon(PATIENCE);
    loop {
        viewer.pump(Duration::from_millis(20))?;
        let listed = lab.fux2(&["ls"])?;
        let gone = String::from_utf8_lossy(&listed.stdout).trim() == "work";
        // No border of either kind: the pane is gone from the viewer too.
        let single = !viewer.text().contains('\u{2500}') && !viewer.text().contains('\u{2502}');
        if gone && single {
            break;
        }
        assert!(
            Instant::now() < until,
            "the history pane stayed:\n{}",
            viewer.text()
        );
    }
    viewer.keys(b"echo after\r")?;
    lab.pane_shows("work", "after")
        .map_err(|e| format!("{e}\nthe viewer:\n{}", viewer.text()))?;
    Ok(())
}
