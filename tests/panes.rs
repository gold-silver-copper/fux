//! The program itself, driven the way a person drives it: with files.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A session directory of the test's own. Removing it ends its panes.
struct Session(PathBuf);
impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Session {
    fn new(name: &str) -> Session {
        // Short, as a deep path would not fit a Unix path limit on macOS.
        let dir = std::env::temp_dir().join(format!("fux3c-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Session(dir)
    }
    fn fux3(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fux3"));
        command.env("FUX3C_DIR", &self.0).env_remove("COLUMNS").env_remove("LINES");
        command
    }
    fn start(&self, name: &str, program: &[&str]) -> PathBuf {
        let out = self.fux3().arg("start").arg(name).arg("--").args(program).output().unwrap();
        assert!(out.status.success(), "start failed: {}", String::from_utf8_lossy(&out.stderr));
        PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// Waits up to 20 seconds for something to become true.
fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn version(pane: &Path) -> u64 {
    let screen = read(&pane.join("screen"));
    let word = screen.split(' ').find_map(|word| word.strip_prefix("v=")).unwrap_or("0");
    word.parse().unwrap_or(0)
}

#[test]
fn what_is_written_to_in_is_typed_and_what_the_program_shows_is_in_screen() {
    let session = Session::new("io");
    let pane = session.start("build", &["sh"]);
    assert_eq!(pane, session.0.join("main/build"));
    fs::write(pane.join("in"), "echo h''i; stty size\n").unwrap();
    until("the answers show", || read(&pane.join("screen")).contains("\nhi\n24 80\n"));
    // A resize reaches the program and line 1.
    fs::write(pane.join("ctl"), "resize 91 17\nfly\n").unwrap();
    fs::write(pane.join("in"), "stty size\n").unwrap();
    until("the new size shows", || read(&pane.join("screen")).contains("\n17 91\n"));
    assert!(read(&pane.join("screen")).lines().next().unwrap().contains("size=91x17"));
    assert_eq!(read(&pane.join("screen")).lines().count(), 18, "line 1 and one line a row");
    until("the refusal is logged", || read(&pane.join("log")).contains("fly: not a command"));
    // A second start of a live pane is refused; the first is not disturbed.
    assert!(!session.fux3().args(["start", "build", "--", "sh"]).output().unwrap().status.success());
    fs::write(pane.join("in"), "exit 3\n").unwrap();
    let waited = session.fux3().arg("wait").arg(&pane).output().unwrap();
    assert!(String::from_utf8_lossy(&waited.stdout).contains("state exited 3"));
    assert!(!pane.join("in").exists() && !pane.join("ctl").exists(), "the FIFOs go with the holder");
}

#[test]
fn kill_hangs_up_the_program_and_removing_the_directory_ends_the_pane() {
    let session = Session::new("end");
    let pane = session
        .start("a", &["sh", "-c", "trap 'echo hup > \"$FUX3C_PANE/saw\"; exit 0' HUP; while :; do sleep 1; done"]);
    fs::write(pane.join("ctl"), "kill\n").unwrap();
    until("the program saw the hangup", || read(&pane.join("saw")) == "hup\n");
    until("its end is in status", || read(&pane.join("status")).contains("state exited 0"));

    let other = session.start("b", &["sh", "-c", "echo $$ > \"$FUX3C_PANE/pid\"; exec sleep 600"]);
    until("the program runs", || !read(&other.join("pid")).is_empty());
    let pid = read(&other.join("pid")).trim().to_string();
    let alive = || Command::new("kill").args(["-0", &pid]).output().unwrap().status.success();
    assert!(alive());
    fs::remove_dir_all(&other).unwrap();
    until("the program is gone with its directory", || !alive());
}

#[test]
fn a_flood_is_published_a_bounded_number_of_times_and_loses_no_line() {
    let session = Session::new("flood");
    let pane = session.start("yes", &["sh", "-c", "yes | head -n 200000; echo done; sleep 600"]);
    let (start, first) = (Instant::now(), version(&pane));
    until("the flood ends", || read(&pane.join("screen")).contains("\ndone\n"));
    let seconds = start.elapsed().as_secs_f64() + 0.1;
    let publishes = (version(&pane) - first) as f64;
    assert!(publishes <= 41.0 * seconds + 2.0, "{publishes} publishes in {seconds} s is over 40 a second");
    let history = read(&pane.join("history.1")) + &read(&pane.join("history")) + &read(&pane.join("screen"));
    assert_eq!(history.lines().filter(|line| *line == "y").count(), 200_000);
}

#[test]
fn the_viewer_shows_panes_as_the_layout_says_and_runs_a_key_script() {
    // The viewer needs a terminal: it runs in a pane of an outer session.
    let (outer, inner) = (Session::new("outer"), Session::new("inner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    let script = "#!/bin/sh\n# split\np=$(fux3 start \"$FUX3C_WS/two\" -- sh) && fux3 layout split row two\necho made $FUX3C_KEYS\n";
    fs::write(keys.join("C-b/v"), script).unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/v")).status().unwrap();
    let one = inner.start("one", &["sh"]);
    let viewer = [env!("CARGO_BIN_EXE_fux3"), "view"];
    let command = ["env", &format!("FUX3C_DIR={}", inner.0.display()), &format!("FUX3C_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    let screen = || read(&view.join("screen"));
    fs::write(view.join("in"), "echo o''ne\r").unwrap();
    until("what is typed reaches the pane and its screen the viewer", || screen().contains("\none\n"));
    assert!(read(&one.join("screen")).contains("\none\n"));
    // The prefix opens the menu the directory gives; the key runs the script.
    fs::write(view.join("in"), "\x02").unwrap();
    until("the menu shows the script's second line", || screen().contains("v  split"));
    fs::write(view.join("in"), "v").unwrap();
    until("the layout file has the split", || read(&inner.0.join("main/layout")) == "* row(one two) focus=two\n");
    until("both panes are drawn with a line between", || {
        screen().contains("─ one ─") && screen().contains("│  two") && screen().contains('│')
    });
    until("the script's words are shown", || screen().contains("made "));
    fs::write(view.join("in"), "echo t''wo\r").unwrap();
    until("the new pane has the keys", || read(&inner.0.join("main/two/screen")).contains("\ntwo\n"));
    // Each pane was asked once to be the size of its tile.
    let width = |pane: &str| {
        read(&inner.0.join("main").join(pane).join("screen"))
            .split(' ')
            .find_map(|w| Some(w.strip_prefix("size=")?.to_string()))
    };
    until("the panes have their tiles' sizes", || {
        width("one") == Some("40x22".into()) && width("two") == Some("39x22".into())
    });
    fs::write(inner.0.join("main/layout"), "* col(two one) focus=one zoom\n").unwrap();
    until("an edit of the file by hand is followed", || width("one") == Some("80x23".into()));
}
