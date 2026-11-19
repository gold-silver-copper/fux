//! The real viewer on a real terminal this test hosts.
use fux_vt::{Parser, Size};
use fuxix::Errno;
use fuxix::poll::{Events, PollFd, poll};
use fuxix::process::{Job, Reach};
use fuxix::pty::Master;
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
    Ok(dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

/// An elevated viewer on Windows must not run the user's file.
fn config_runs() -> Result<bool, fuxix::Errno> {
    #[cfg(windows)]
    return Ok(!fuxix::socket::User::current()?.elevated());
    #[cfg(unix)]
    Ok(true)
}

fn shell() -> &'static str {
    if cfg!(windows) { "cmd.exe" } else { "sh" }
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
        Ok(Lab { root, config: home })
    }

    fn fux2(&self, args: &[&str]) -> Result<Output, std::io::Error> {
        Command::new(beside_tests("fux2")?)
            .args(args)
            .env("FUX2_DIR", &self.root)
            .output()
    }

    fn start(&self, name: &str) -> Tested {
        let out = self.fux2(&["start", "--name", name, "--", shell()])?;
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
    fn drop(&mut self) {
        for entry in std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
        {
            let file = entry.file_name().to_string_lossy().into_owned();
            if let Some(name) = file.strip_prefix("p-") {
                let _ = self.fux2(&["kill", name]);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
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
        let _ = fux2::sys::prepare_terminal(&lab.root);
        let size = Size::new(30, 100)?;
        let (master, slave) = fuxix::pty::open(size.nonzero().into())?;
        let viewer = beside_tests("fux2-view")?;
        let mut argv = vec![viewer.into_os_string()];
        argv.extend_from_slice(args);
        let job = fux2::sys::spawn(slave, &argv, |command| {
            command
                .env("FUX2_DIR", &lab.root)
                .env("XDG_CONFIG_HOME", &lab.config)
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
(bind-key "F7" from-binding)
(define! spin (lambda! () (spin)))
(bind-key "F8" (lambda! () (spin)))
(unbind-key "C-b n")
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
    viewer.shows(" left ")?;
    viewer.shows(" right ")?;

    viewer.keys(b"echo typed-left\r")?;
    lab.pane_shows("left", "typed-left")?;
    viewer.shows("typed-left")?;

    let configured = config_runs()?;
    if configured {
        viewer.keys(b"\x1b[18~")?;
        lab.pane_shows("left", "from-binding")?;
    } else {
        viewer.shows("view.grift is not run")?;
    }

    viewer.keys(b"\x02o")?;
    viewer.keys(b"echo typed-right\r")?;
    lab.pane_shows("right", "typed-right")?;
    assert!(!lab.screen("left")?.contains("typed-right"));

    if configured {
        // The loop holds only the language's thread: keys still go through.
        viewer.keys(b"\x1b[19~")?;
        viewer.keys(b"echo while-looping\r")?;
        lab.pane_shows("right", "while-looping")?;

        // The configuration took C-b n away: it is refused out loud.
        viewer.keys(b"\x02n")?;
        viewer.shows("no binding for C-b n")?;
    }

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
    viewer.shows(" only ")?;
    if config_runs()? {
        viewer.keys(b"\x1b[19~")?;
        viewer.shows("a binding is running")?;
    }
    viewer.keys(b"\x02d")?;
    assert!(viewer.ended()?, "the viewer did not leave");
    let listed = lab.fux2(&["ls"])?;
    assert_eq!(String::from_utf8_lossy(&listed.stdout).trim(), "only");
    Ok(())
}

/// A layout file makes the tabs, a bad edit leaves them as they were, and
/// only the fit key changes a pane's size.
#[test]
fn a_layout_makes_tabs_and_only_fit_resizes() -> Tested {
    let lab = Lab::new("")?;
    for (name, group) in [("one", "a"), ("two", "b")] {
        let label = format!("group={group}");
        let args = ["start", "--name", name, "--label", &label, "--", shell()];
        let out = lab.fux2(&args)?;
        assert!(out.status.success(), "{out:?}");
    }
    let layout = lab.config.join("tabs.toml");
    let tabs = "[[tab]]\ntitle = \"first\"\nmatch = [\"group=a\"]\n\
                [[tab]]\ntitle = \"second\"\nmatch = [\"group=b\"]\n";
    std::fs::write(&layout, tabs)?;
    let args = ["--layout".into(), layout.clone().into_os_string()];
    let mut viewer = Viewer::with_args(&lab, &args)?;
    viewer.shows(" one ")?;
    assert!(!viewer.text().contains(" two "), "{}", viewer.text());
    let size = |lab: &Lab, name: &str| -> Result<String, std::io::Error> {
        let out = lab.fux2(&["ls", "--long"])?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let line = text.lines().find(|line| line.starts_with(name));
        Ok(line
            .and_then(|l| l.split('\t').nth(3))
            .unwrap_or("")
            .to_owned())
    };
    assert_eq!(size(&lab, "one")?, "80x24", "viewing resized a pane");

    viewer.keys(b"\x02n")?;
    viewer.shows(" two ")?;
    std::fs::write(&layout, "[[tab]]\naxis = \"sideways\"\n")?;
    viewer.shows("no axis")?;
    assert!(viewer.text().contains(" two "), "{}", viewer.text());

    viewer.keys(b"\x02f")?;
    let until = soon(PATIENCE);
    while size(&lab, "two")? != "100x28" {
        assert!(Instant::now() < until, "fit gave {}", size(&lab, "two")?);
        viewer.pump(Duration::from_millis(20))?;
    }
    assert_eq!(size(&lab, "one")?, "80x24");
    Ok(())
}

fn pager() -> &'static str {
    if cfg!(windows) { "findstr ." } else { "cat" }
}

/// Workspaces and tabs change what is shown and who gets the keys; a pane
/// started from a tab appears there and nowhere else.
#[test]
fn workspaces_and_tabs_choose_what_is_shown() -> Tested {
    let lab = Lab::new("")?;
    for (name, group) in [("alpha", "a"), ("beta", "b")] {
        let label = format!("group={group}");
        let args = ["start", "--name", name, "--label", &label, "--", shell()];
        let out = lab.fux2(&args)?;
        assert!(out.status.success(), "{out:?}");
    }
    let layout = lab.config.join("spaces.toml");
    let text = "[[workspace]]\nname = \"first\"\n[[workspace.tab]]\ntitle = \"ta\"\n\
                match = [\"group=a\"]\n[[workspace]]\nname = \"second\"\n\
                [[workspace.tab]]\ntitle = \"tb\"\nmatch = [\"group=b\"]\n";
    std::fs::write(&layout, text)?;
    let args = ["--layout".into(), layout.into_os_string()];
    let mut viewer = Viewer::with_args(&lab, &args)?;
    viewer.shows(" alpha ")?;
    viewer.shows("first 1/2 | ta 1/1")?;

    viewer.keys(b"\x02)")?;
    viewer.shows(" beta ")?;
    viewer.shows("second 2/2 | tb 1/1")?;
    assert!(!viewer.text().contains(" alpha "), "{}", viewer.text());
    viewer.keys(b"echo to-beta\r")?;
    lab.pane_shows("beta", "to-beta")?;
    assert!(!lab.screen("alpha")?.contains("to-beta"));

    viewer.keys(b"\x02c")?;
    viewer.shows(" shell-1 ")?;
    let listed = lab.fux2(&["ls", "--long"])?;
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    let line = listed.lines().find(|l| l.starts_with("shell-1"));
    assert!(line.is_some_and(|l| l.contains("group=b")), "{listed}");
    viewer.keys(b"echo to-new\r")?;
    lab.pane_shows("shell-1", "to-new")?;

    viewer.keys(b"\x02L")?;
    viewer.shows("first 1/2")?;
    assert!(!viewer.text().contains(" shell-1 "), "{}", viewer.text());
    Ok(())
}

/// The history key opens a pager in a pane of its own; when it ends, the
/// pane is gone and keys go where they went before.
#[test]
fn the_history_pane_goes_away_and_gives_the_focus_back() -> Tested {
    let lab = Lab::new("")?;
    lab.start("work")?;
    let mut viewer = Viewer::with_env(&lab, &[], &[("PAGER", pager())])?;
    viewer.shows(" work ")?;
    viewer.keys(b"echo before\r")?;
    lab.pane_shows("work", "before")?;
    viewer.keys(b"\x02[")?;
    let until = soon(PATIENCE);
    let mut saw_it_end = false;
    while Instant::now() < until {
        viewer.pump(Duration::from_millis(20))?;
        let listed = lab.fux2(&["ls"])?;
        if String::from_utf8_lossy(&listed.stdout).trim() == "work"
            && viewer.text().contains(" work ")
            && !viewer.text().contains("history-1")
        {
            saw_it_end = true;
            break;
        }
    }
    assert!(saw_it_end, "the history pane stayed:\n{}", viewer.text());
    viewer.keys(b"echo after\r")?;
    lab.pane_shows("work", "after")?;
    Ok(())
}
