//! The real viewer on a real terminal this test hosts.
use fux_vt::{Parser, Size};
use fuxix::Errno;
use fuxix::poll::{Events, PollFd, poll};
use fuxix::process::{Job, Reach};
use fuxix::pty::Master;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
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

fn shell() -> &'static str {
    if cfg!(windows) { "cmd.exe" } else { "sh" }
}

struct Lab {
    root: PathBuf,
    config: PathBuf,
}

impl Lab {
    fn new(config: &str) -> Result<Lab, Box<dyn std::error::Error>> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let number = NEXT.fetch_add(1, Ordering::SeqCst);
        let base = fuxix::file::scratch()?;
        let root = base.join(format!("f2v-{}-{number}", std::process::id()));
        fuxix::file::private_dir(&root)?;
        let home = base.join(format!("f2c-{}-{number}", std::process::id()));
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
        let _ = fux2::sys::prepare_terminal(&lab.root);
        let size = Size::new(30, 100)?;
        let (master, slave) = fuxix::pty::open(size.nonzero().into())?;
        let viewer = beside_tests("fux2-view")?;
        let argv = [viewer.into_os_string()];
        let job = fux2::sys::spawn(slave, &argv, |command| {
            command
                .env("FUX2_DIR", &lab.root)
                .env("XDG_CONFIG_HOME", &lab.config)
                .env("TERM", "xterm-256color");
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
    viewer.shows("no binding for C-b n")?;

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
    viewer.keys(b"\x1b[19~")?;
    viewer.shows("a binding is running")?;
    viewer.keys(b"\x02d")?;
    assert!(viewer.ended()?, "the viewer did not leave");
    let listed = lab.fux2(&["ls"])?;
    assert_eq!(String::from_utf8_lossy(&listed.stdout).trim(), "only");
    Ok(())
}
