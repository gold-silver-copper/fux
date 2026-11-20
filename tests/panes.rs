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
        let dir = std::env::temp_dir().join(format!("fux-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Session(dir)
    }
    fn fux(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fux"));
        command.env("FUXN_DIR", &self.0).env_remove("COLUMNS").env_remove("LINES");
        // Run in a pane of fux, the suite must not take that pane's workspace or keys for its own.
        command.env_remove("FUXN_PANE").env_remove("FUXN_WS").env_remove("FUXN_KEYS");
        command
    }
    fn start(&self, name: &str, program: &[&str]) -> PathBuf {
        let out = self.fux().arg("start").arg(name).arg("--").args(program).output().unwrap();
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
    fs::write(pane.join("ctl"), "kill \x1b]0;now\x07\n").unwrap();
    until("a refused line is logged without its control codes", || read(&pane.join("log")).contains("kill ?]0;now?"));
    // A pane whose program cannot start is not left behind.
    assert!(!session.fux().args(["start", "none", "--", "/no/such/program"]).output().unwrap().status.success());
    assert!(!session.0.join("main/none").exists());
    // A second start of a live pane is refused; the first is not disturbed.
    assert!(!session.fux().args(["start", "build", "--", "sh"]).output().unwrap().status.success());
    fs::write(pane.join("in"), "exit 3\n").unwrap();
    let waited = session.fux().arg("wait").arg(&pane).output().unwrap();
    assert!(String::from_utf8_lossy(&waited.stdout).contains("state exited 3"));
    assert!(!pane.join("in").exists() && !pane.join("ctl").exists(), "the FIFOs go with the holder");
    // A start in place that fails leaves what the last program left.
    assert!(!session.fux().args(["start", "build", "--", "/no/such/program"]).output().unwrap().status.success());
    assert!(read(&pane.join("screen")).contains("state=exited:3") && read(&pane.join("screen")).contains("exit 3"));
    assert!(
        !session.fux().args(["start", "layout"]).output().unwrap().status.success(),
        "the layout file's name is taken"
    );
}

#[test]
fn a_pane_knows_where_it_is_and_gets_no_name_that_is_not_its_own() {
    let session = Session::new("names");
    let mut start = session.fux();
    // What a key script would leave in the environment is not this pane's; another program's names are not ours to judge.
    start.env("FUXN_WS", "main").env("FUXN_VIEW", "/nowhere").env("FUX_SOCKET", "/another").env("COLUMNS", "90");
    assert!(start.args(["start", "w2/p", "--", "sh"]).status().unwrap().success());
    let pane = session.0.join("w2/p");
    let me = env!("CARGO_BIN_EXE_fux");
    let typed = format!(
        "env | grep -E '^(FUXN|FUX_SOCKET=|TERM=|TERM_PROGRAM|COLUMNS)' | LC_ALL=C sort > \"$FUXN_PANE/env\"; '{me}' layout > \"$FUXN_PANE/here\"\n"
    );
    fs::write(pane.join("in"), typed).unwrap();
    // A command typed in a pane means its own workspace, not `main`.
    until("the layout of its own workspace is printed", || read(&pane.join("here")) == "* p focus=p\n");
    let expected = format!(
        "FUXN_DIR={}\nFUXN_PANE={}\nFUX_SOCKET=/another\nTERM=xterm-256color\nTERM_PROGRAM=fux-next\nTERM_PROGRAM_VERSION={}\n",
        session.0.display(),
        pane.display(),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(read(&pane.join("env")), expected);
    assert_eq!(read(&pane.join("screen")).lines().next().map(|line| line.contains("size=90x24")), Some(true));
}

#[test]
fn kill_hangs_up_the_program_and_removing_the_directory_ends_the_pane() {
    let session = Session::new("end");
    let pane = session.start(
        "a",
        &["sh", "-c", "trap 'echo hup > \"$FUXN_PANE/saw\"; exit 0' HUP; echo r''eady; while :; do sleep 1; done"],
    );
    until("the program has set its trap", || read(&pane.join("screen")).contains("ready"));
    fs::write(pane.join("ctl"), "kill\n").unwrap();
    until("the program saw the hangup", || read(&pane.join("saw")) == "hup\n");
    until("its end is on line 1", || read(&pane.join("screen")).contains(" state=exited:0 "));

    // Whoever starts a pane may ignore signals; its program does not inherit that.
    let start = format!("trap '' INT HUP; exec '{}' start c -- sleep 600", env!("CARGO_BIN_EXE_fux"));
    assert!(Command::new("sh").args(["-c", &start]).env("FUXN_DIR", &session.0).status().unwrap().success());
    let interrupted = session.0.join("main/c");
    until("the program runs", || read(&interrupted.join("screen")).contains(" state=running "));
    fs::write(interrupted.join("in"), "\x03").unwrap();
    until("Ctrl-C ended it", || read(&interrupted.join("screen")).contains(" state=killed:2 "));

    let other = session.start("b", &["sh", "-c", "echo $$ > \"$FUXN_PANE/pid\"; exec sleep 600"]);
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
    let script = "#!/bin/sh\n# split\n# group: Panes\np=$(fux start \"$FUXN_WS/two\" -- sh) && fux layout split row two\necho made $FUXN_KEYS\n";
    fs::write(keys.join("C-b/v"), script).unwrap();
    fs::write(keys.join("C-b/e"), "#!/bin/sh\nsleep 0.3\nprintf 'echo ' 1<>\"$FUXN_PANE/in\"\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/v")).arg(keys.join("C-b/e")).status().unwrap();
    let one = inner.start("one", &["sh"]);
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let command = ["env", &format!("FUXN_DIR={}", inner.0.display()), &format!("FUXN_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    let screen = || read(&view.join("screen"));
    fs::write(view.join("in"), "echo o''ne\r").unwrap();
    until("what is typed reaches the pane and its screen the viewer", || screen().contains("\none\n"));
    assert!(read(&one.join("screen")).contains("\none\n"));
    // Keys typed after a key with a script wait for the script: here it types `echo `.
    fs::write(view.join("in"), "\x02ein or''der\r").unwrap();
    until("the script's typing came first", || screen().contains("\nin order\n"));
    // The prefix opens the menu the directory gives; the key runs the script.
    fs::write(view.join("in"), "\x02").unwrap();
    until("the menu and the bar show", || {
        screen().contains(" e\n")
            && screen().contains(" Panes\n")
            && screen().contains("v  split")
            && screen().contains(" main  one ")
            && screen().contains("C-b …")
    });
    fs::write(view.join("in"), "v").unwrap();
    until("the layout file has the split", || read(&inner.0.join("main/layout")) == "* row(one two) focus=two\n");
    until("both panes are drawn with a line between", || {
        screen().lines().filter(|line| line.contains('│')).count() >= 23
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
        width("one") == Some("40x24".into()) && width("two") == Some("39x24".into())
    });
    // A character the terminal outside may draw wider than fux counts it (three people who are one
    // family here) is not drawn where that could reach past its tile's edge: a `?` stands in each of
    // its columns, while the pane's own screen keeps it. One that is wide everywhere is drawn.
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
    fs::write(inner.0.join("main/one/in"), format!("clear; printf '\\033[9;39H{family}\\033[11;39H\u{6f22}\\n'\r"))
        .unwrap();
    until("the family is in its pane and a stand-in in the viewer", || {
        read(&inner.0.join("main/one/screen")).contains(family)
            && screen().contains("??│")
            && screen().contains("\u{6f22}│")
            && !screen().contains(family)
    });
    // A program that asks for the mouse gets it, counted from its own tile's corner (the tile starts at column 42).
    fs::write(view.join("in"), "printf '\\033[?1000h\\033[?1006h'; cat -v\r").unwrap();
    until("the pane's line 1 says so", || read(&inner.0.join("main/two/screen")).contains("mouse1000,mousesgr"));
    until("the viewer asked its own terminal", || read(&view.join("screen")).contains("mouse1000,mousesgr"));
    fs::write(view.join("in"), "\x1b[<0;50;3M\r").unwrap();
    until("the click arrives translated", || read(&inner.0.join("main/two/screen")).contains("^[[<0;9;3M"));
    fs::write(view.join("in"), "\x03").unwrap();
    fs::write(inner.0.join("main/layout"), "* one focus=one\n  two focus=two\n").unwrap();
    until("an edit of the file by hand is followed", || width("one") == Some("80x24".into()));
    // A folder with a screen of someone's own making: what it says reaches the terminal as text only.
    fs::create_dir(inner.0.join("main/odd")).unwrap();
    fs::write(inner.0.join("main/odd/screen.styled"), "pid=1 size=9x2 cursor=0,0 state=x\x1b]0;T\x07\x1b[2Jy\nhi\n")
        .unwrap();
    // The viewer gives the new folder a tab; a hand's edit comes after that, not across it.
    until("the viewer has placed the folder", || read(&inner.0.join("main/layout")).contains("odd"));
    fs::write(inner.0.join("main/layout"), "* row(one odd) focus=one\n").unwrap();
    until("its title is drawn with the codes as marks", || screen().contains(" x?]0;T??[2Jy\n"));
}
