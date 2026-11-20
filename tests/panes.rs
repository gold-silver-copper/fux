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

/// One of a pane's facts: the text of its file, without the line end.
fn fact(pane: &Path, name: &str) -> String {
    read(&pane.join(name)).trim_end().to_string()
}

#[test]
fn what_is_written_to_in_is_typed_and_what_the_program_shows_is_in_screen() {
    let session = Session::new("io");
    let pane = session.start("build", &["sh"]);
    assert_eq!(pane, session.0.join("main/build"));
    fs::write(pane.join("in"), "echo h''i; stty size\n").unwrap();
    until("the answers show", || read(&pane.join("screen")).contains("\nhi\n24 80\n"));
    // A resize reaches the program and the `size` file; the screen is its rows and nothing else.
    fs::write(pane.join("ctl"), "resize 91 17\nfly\n").unwrap();
    fs::write(pane.join("in"), "stty size\n").unwrap();
    until("the new size shows", || read(&pane.join("screen")).contains("\n17 91\n"));
    assert_eq!(fact(&pane, "size"), "91 17");
    assert_eq!(read(&pane.join("screen")).lines().count(), 17, "one line a row");
    assert!(read(&pane.join("screen")).starts_with("$ ") || !read(&pane.join("screen")).starts_with("pid="));
    // Every fact has its file: where the cursor is, how input is sent, whether the program runs, who it is.
    let cursor: Vec<String> = fact(&pane, "cursor").split(' ').map(str::to_string).collect();
    assert!(cursor.len() == 4 && cursor[2] == "shown", "{cursor:?}");
    assert_eq!((fact(&pane, "state").as_str(), fact(&pane, "top").as_str()), ("running", "0"));
    assert!(read(&pane.join("status")).lines().nth(1).unwrap().starts_with("pid "));
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
    assert!(fact(&pane, "state") == "exited 3" && read(&pane.join("screen")).contains("exit 3"));
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
    assert_eq!(fact(&pane, "size"), "90 24");
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
    until("its end is in `state`", || fact(&pane, "state") == "exited 0");

    // A program that ends without a word: `state` changes and the text does not. The screen is
    // written again all the same, and after the fact: who watches that one file finds the end.
    use std::os::unix::fs::MetadataExt;
    let quiet = session.start("q", &["sh", "-c", "sleep 0.4"]);
    let stamp = || fs::metadata(quiet.join("screen.styled")).map(|file| (file.ino(), file.mtime_nsec())).ok();
    let (running, first) = (fact(&quiet, "state"), stamp());
    until("its screen is written again", || stamp() != first);
    assert_eq!((running.as_str(), fact(&quiet, "state").as_str()), ("running", "exited 0"));

    // Whoever starts a pane may ignore signals; its program does not inherit that.
    let start = format!("trap '' INT HUP; exec '{}' start c -- sleep 600", env!("CARGO_BIN_EXE_fux"));
    assert!(Command::new("sh").args(["-c", &start]).env("FUXN_DIR", &session.0).status().unwrap().success());
    let interrupted = session.0.join("main/c");
    until("the program runs", || fact(&interrupted, "state") == "running");
    fs::write(interrupted.join("in"), "\x03").unwrap();
    until("Ctrl-C ended it", || fact(&interrupted, "state") == "killed 2");

    let other = session.start("b", &["sh", "-c", "echo $$ > \"$FUXN_PANE/pid\"; exec sleep 600"]);
    until("the program runs", || !read(&other.join("pid")).is_empty());
    let pid = read(&other.join("pid")).trim().to_string();
    let alive = || Command::new("kill").args(["-0", &pid]).output().unwrap().status.success();
    assert!(alive());
    fs::remove_dir_all(&other).unwrap();
    until("the program is gone with its directory", || !alive());
}

#[test]
fn a_pane_made_narrow_and_wide_again_is_as_it_was_and_loses_no_text_on_the_way() {
    let session = Session::new("resize");
    let lines: Vec<String> = (1..=8).map(|n| format!("line{n}-{}", "abcdefghij".repeat(n % 4 + 1))).collect();
    let script = format!("printf '%s\\n' {}; printf ready; sleep 600", lines.join(" "));
    let pane = session.start("p", &["sh", "-c", &script]);
    let screen = || read(&pane.join("screen"));
    let rows = screen;
    until("the program has printed", || screen().contains("\nready\n"));
    let (before, history) = (rows(), read(&pane.join("history")));
    // Narrow: lines wrap, rows leave by the top into `history`, and every line is whole in
    // `history` and `screen` together (the screen's rows that go on are joined to tell).
    for size in ["12 24", "7 24", "40 5", "80 24"] {
        fs::write(pane.join("ctl"), format!("resize {size}\n")).unwrap();
        until("the pane has the size", || fact(&pane, "size") == size);
        let all = (read(&pane.join("history")) + &rows()).replace('\n', "");
        assert_eq!(all, lines.concat() + "ready", "at {size}");
    }
    // Back at the size it had, with nothing printed between: the screen as it was, and
    // `history` has given back what it was sent.
    assert_eq!((rows(), read(&pane.join("history"))), (before, history));
    // A size that is refused changes nothing and is in the log.
    fs::write(pane.join("ctl"), "resize 0 5\n").unwrap();
    until("the log says so", || read(&pane.join("log")).contains("resize"));
    assert_eq!(fact(&pane, "size"), "80 24");
}

#[test]
fn a_flood_is_published_a_bounded_number_of_times_and_loses_no_line() {
    let session = Session::new("flood");
    let pane = session.start("yes", &["sh", "-c", "yes | head -n 200000; echo done; sleep 600"]);
    // Each publish replaces `screen.styled`: they are counted by looking at it far more often than it is written.
    use std::os::unix::fs::MetadataExt;
    let (start, mut seen, mut publishes) = (Instant::now(), None, 0.0);
    while !read(&pane.join("screen")).contains("\ndone\n") {
        let now = fs::metadata(pane.join("screen.styled")).ok().map(|file| (file.ino(), file.mtime_nsec()));
        publishes += f64::from(u8::from(std::mem::replace(&mut seen, now) != now));
        assert!(start.elapsed() < Duration::from_secs(20), "the flood did not end");
        std::thread::sleep(Duration::from_millis(1));
    }
    let seconds = start.elapsed().as_secs_f64() + 0.1;
    assert!(publishes >= 2.0, "the flood was watched");
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
    // The split key starts its pane at the size the viewer says the new tile will have; the
    // program notes the size it found when it started, before the pane has a tile at all.
    let script = "#!/bin/sh\n# split\n# group: Panes\np=$(COLUMNS=${FUXN_ROW%x*} LINES=${FUXN_ROW#*x} fux start \"$FUXN_WS/two\" -- sh -c 'stty size > \"$FUXN_PANE/first\"; exec sh') && until [ -s \"$p/first\" ]; do sleep 0.01; done && fux layout split row two\necho made $FUXN_KEYS\n";
    fs::write(keys.join("C-b/v"), script).unwrap();
    fs::write(keys.join("C-b/e"), "#!/bin/sh\nsleep 0.3\nprintf 'echo ' 1<>\"$FUXN_PANE/in\"\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/v")).arg(keys.join("C-b/e")).status().unwrap();
    let one = inner.start("one", &["sh"]);
    // The terminal the viewer runs in says what its colours are: here the outer session's file does.
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("colours"), "foreground #102030\nbackground #fdf6e3\n4 #268bd2\n").unwrap();
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
    let width = |pane: &str| Some(fact(&inner.0.join("main").join(pane), "size").replace(' ', "x"));
    until("the panes have their tiles' sizes", || {
        width("one") == Some("40x24".into()) && width("two") == Some("39x24".into())
    });
    // And the new pane's program started at its tile's size: it was never drawn at another.
    assert_eq!(read(&inner.0.join("main/two/first")), "24 39\n");
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
    // The viewer asked its terminal once what the colours are and wrote them down for the
    // session: a program in a pane that asks is told the same, viewer or no viewer.
    let told = "foreground #102030\nbackground #fdf6e3\n4 #268bd2\n";
    until("the session knows its colours", || read(&inner.0.join("colours")) == told);
    fs::write(view.join("in"), "clear; printf '\\033]11;?\\007\\033]4;4;?\\007\\033]4;5;?\\007'; cat -v\r").unwrap();
    let answers = || read(&inner.0.join("main/two/screen")).replace('\n', "");
    until("the program is told", || answers().contains("^[]11;rgb:fdfd/f6f6/e3e3^G^[]4;4;rgb:2626/8b8b/d2d2^G"));
    assert!(!answers().contains("]4;5;rgb"));
    // (The interrupt empties what was typed after it: the next line waits until it is through.)
    fs::write(view.join("in"), "\x03").unwrap();
    until("the program is interrupted", || answers().contains("^G^C"));
    // A program that asks for the mouse gets it, counted from its own tile's corner (the tile starts at column 42).
    fs::write(view.join("in"), "printf '\\033[?1000h\\033[?1006h'; cat -v\r").unwrap();
    until("the pane's modes say so", || fact(&inner.0.join("main/two"), "modes").contains("mouse1000 mousesgr"));
    until("the viewer asked its own terminal", || fact(&view, "modes").contains("mouse1000 mousesgr"));
    fs::write(view.join("in"), "\x1b[<0;50;3M\r").unwrap();
    until("the click arrives translated", || read(&inner.0.join("main/two/screen")).contains("^[[<0;9;3M"));
    fs::write(view.join("in"), "\x03").unwrap();
    fs::write(inner.0.join("main/layout"), "* one focus=one\n  two focus=two\n").unwrap();
    until("an edit of the file by hand is followed", || width("one") == Some("80x24".into()));
    // A folder with a screen of someone's own making: what it says reaches the terminal as text only.
    fs::create_dir(inner.0.join("main/odd")).unwrap();
    fs::write(inner.0.join("main/odd/state"), "x\x1b]0;T\x07\x1b[2Jy\n").unwrap();
    fs::write(inner.0.join("main/odd/cursor"), "99 99 shown 7\n").unwrap();
    fs::write(inner.0.join("main/odd/screen.styled"), "hi\x1b]0;T\x07\x1b[?1049h\n").unwrap();
    // The viewer gives the new folder a tab; a hand's edit comes after that, not across it.
    until("the viewer has placed the folder", || read(&inner.0.join("main/layout")).contains("odd"));
    fs::write(inner.0.join("main/layout"), "* row(one odd) focus=one\n").unwrap();
    until("its title is drawn with the codes as marks", || screen().contains(" x?]0;T??[2Jy\n"));
}

#[test]
fn keys_reach_a_program_as_it_asked_for_them_and_the_viewer_still_finds_its_own() {
    // The viewer runs in a pane of an outer session whose terminal is said to take the kitty
    // keyboard protocol; what is written to that pane's `in` is what such a terminal would send.
    let (outer, inner) = (Session::new("kouter"), Session::new("kinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    fs::write(keys.join("C-b/o"), "#!/bin/sh\n# other\nfux layout focus next\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/o")).status().unwrap();
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "5\n").unwrap();
    // A program asks to have keys told apart (on its alternate screen, as an editor does) before
    // any viewer has said what its terminal takes: it is told that nothing is in force.
    let asking = "stty raw -echo; printf '\\033[?1049h\\033[>1u\\033[?u'; exec cat -vt";
    let early = inner.start("early", &["sh", "-c", asking]);
    let plain = inner.start("plain", &["sh", "-c", "stty raw -echo; exec cat -vt"]);
    until("the first answer", || read(&early.join("screen")).contains("^[[?0u"));
    fs::create_dir_all(inner.0.join("main")).unwrap();
    fs::write(inner.0.join("main/layout"), "* row(early plain) focus=early\n").unwrap();
    // The terminal the viewer finds has flags of its own on its alternate screen (4), and after
    // the viewer it is asked what it has.
    let view = format!(
        "printf '\\033[?1049h\\033[>4u\\033[?1049l'; FUXN_DIR={} FUXN_KEYS={} {} view; stty raw -echo; printf '\\033[?1049h\\033[?u'; exec cat -vt",
        inner.0.display(),
        keys.display(),
        env!("CARGO_BIN_EXE_fux")
    );
    let view = outer.start("view", &["sh", "-c", &view]);
    let line1 = |pane: &Path| format!("{} state={}", fact(pane, "modes"), fact(pane, "state").replace(' ', ":"));
    let shown = |pane: &Path| read(&pane.join("screen")).replace('\n', "");
    // The viewer learns what its terminal takes and says so for the session. A program that
    // asks from then on has its flags in force, on its own line 1 and, asked of the terminal,
    // on the viewer's.
    until("the session knows", || read(&inner.0.join("keyboard")) == "5\n");
    let asks = inner.start("asks", &["sh", "-c", asking]);
    until("the second answer", || shown(&asks).contains("^[[?1u") && line1(&asks).contains("keys1"));
    fs::write(inner.0.join("main/layout"), "* row(asks plain) focus=asks\n  early focus=early\n").unwrap();
    until("the viewer asked its terminal for them", || line1(&view).contains("keys1"));
    // Ctrl-I is not Tab and Shift-Enter is not Enter: each reaches the program as it was sent.
    fs::write(view.join("in"), "a\x1b[105;5ub\tc\x1b[13;2ud\r").unwrap();
    until("the keys arrive apart", || shown(&asks).ends_with("^[[?1ua^[[105;5ub^Ic^[[13;2ud^M"));
    // The prefix is found in that form too, its key runs the script, and neither is typed.
    fs::write(view.join("in"), "\x1b[98;5uo").unwrap();
    until("the other pane has the keys", || read(&inner.0.join("main/layout")).contains("focus=plain"));
    // That program asked for nothing: the terminal is put back to telling keys the old way.
    until("the viewer took the flags back", || !line1(&view).contains("keys"));
    fs::write(view.join("in"), "x\ty").unwrap();
    until("plain keys arrive", || shown(&plain) == "x^Iy");
    assert!(shown(&asks).ends_with("d^M"), "{}", shown(&asks));
    // Back on the pane that asked, they are in force again.
    fs::write(view.join("in"), "\x02o").unwrap();
    until("the flags are asked for again", || line1(&view).contains("keys1"));
    // The program that was told "none" before the viewer came still gets keys the old way: what
    // it was told stays true.
    assert!(!line1(&early).contains("keys") && shown(&early) == "^[[?0u", "{}", line1(&early));
    // When the viewer leaves, the terminal has the flags it had: 4, on its alternate screen.
    let ctl = fs::read_dir(&inner.0)
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().starts_with(".view."));
    fs::write(ctl.unwrap().path(), "quit\n").unwrap();
    until("the terminal is as it was found", || shown(&view).contains("^[[?4u"));
}
