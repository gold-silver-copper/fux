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
fn until(what: &str, ready: impl FnMut() -> bool) {
    until_or(what, String::new, ready);
}

/// The same, and when it does not become true, says what `more` has to say of it.
fn until_or(what: &str, more: impl Fn() -> String, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting until {what}{}", more());
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
    // Every fact is written before the screens and `screen.styled` last of all: in a pane that
    // has ended, no fact is younger than it.
    let written = |name: &str| fs::metadata(quiet.join(name)).unwrap().modified().unwrap();
    let wait = session.fux().arg("wait").arg(&quiet).output().unwrap();
    assert!(String::from_utf8_lossy(&wait.stdout).ends_with("state exited 0\n"));
    for name in ["size", "cursor", "modes", "state", "status", "top", "wraps", "pictures", "screen"] {
        assert!(written(name) <= written("screen.styled"), "{name} was written after screen.styled");
    }

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
        // (The size is written a moment before the text that was wrapped for it.)
        until("the pane has the size and every line whole", || {
            fact(&pane, "size") == size
                && (read(&pane.join("history")) + &rows()).replace('\n', "") == lines.concat() + "ready"
        });
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
    // family here) is not drawn where that could reach past its tile's edge, unless the terminal
    // says that it joins characters as fux does (the viewer asks: mode 2027). This one, a pane of
    // fux, says so: the family is drawn at the very edge, as a wide character is.
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
    fs::write(inner.0.join("main/one/in"), format!("clear; printf '\\033[9;39H{family}\\033[11;39H\u{6f22}\\n'\r"))
        .unwrap();
    until("the family is in its pane and in the viewer", || {
        read(&inner.0.join("main/one/screen")).contains(family)
            && screen().contains(&format!("{family}│"))
            && screen().contains("\u{6f22}│")
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
    fs::write(inner.0.join("main/odd/size"), "5 9\n").unwrap();
    fs::write(inner.0.join("main/odd/screen.styled"), "h\x1b[1mi\x1b]0;T\x07\x1b[?1049h ho-hum\n").unwrap();
    // The viewer gives the new folder a tab; a hand's edit comes after that, not across it.
    until("the viewer has placed the folder", || read(&inner.0.join("main/layout")).contains("odd"));
    fs::write(inner.0.join("main/layout"), "* row(one odd) focus=one\n").unwrap();
    until("its title is drawn with the codes as marks", || screen().contains(" x?]0;T??[2Jy\n"));
    // Its row is text and colours and nothing more, cut where it says the pane ends (5 columns).
    assert!(screen().contains("│hi?]0\n") || screen().contains("│hi?]0 "), "{}", screen());
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
    // The layout is changed as a person changes it, by `fux layout`, which takes the lock the viewer takes.
    let layout = |edits: &[&[&str]]| {
        for edit in edits {
            let out = inner.fux().arg("layout").args(*edit).output().unwrap();
            assert!(out.status.success(), "layout {edit:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        read(&inner.0.join("main/layout"))
    };
    assert_eq!(
        layout(&[&["split", "row", "plain", "beside", "early"], &["pane", "early"]]),
        "* row(early plain) focus=early\n"
    );
    // The terminal the viewer finds has flags of its own on its alternate screen (4), and after
    // the viewer it is asked what it has.
    let view = format!(
        "printf '\\033[?1049h\\033[>4u\\033[?1049l'; FUXN_DIR={} FUXN_KEYS={} {} view; stty raw -echo; printf '\\033[?1049h\\033[?u'; exec cat -vt",
        inner.0.display(),
        keys.display(),
        env!("CARGO_BIN_EXE_fux")
    );
    let view = outer.start("view", &["sh", "-c", &view]);
    let modes = |pane: &Path| fact(pane, "modes");
    let shown = |pane: &Path| read(&pane.join("screen")).replace('\n', "");
    // (When the viewer is waited for in vain, what its pane says is printed.)
    let seen = || {
        format!(
            ": the viewer's pane has modes {:?}, state {:?} and shows\n{}",
            modes(&view),
            fact(&view, "state"),
            read(&view.join("screen"))
        )
    };
    // The viewer learns what its terminal takes and says so for the session. A program that
    // asks from then on has its flags in force, on its own `modes` and, asked of the terminal,
    // on the viewer's.
    until("the session knows", || read(&inner.0.join("keyboard")) == "5\n");
    let asks = inner.start("asks", &["sh", "-c", asking]);
    until("the second answer", || shown(&asks).contains("^[[?1u") && modes(&asks).contains("keys1"));
    let edits: [&[&str]; 4] =
        [&["split", "row", "asks", "beside", "plain"], &["swap", "prev"], &["pane", "early"], &["break"]];
    assert_eq!(layout(&edits), "- row(asks plain) focus=asks\n* early focus=early\n");
    assert_eq!(layout(&[&["pane", "asks"]]), "* row(asks plain) focus=asks\n- early focus=early\n");
    // (And has drawn the last of the edits: the pane has its half of the terminal.)
    until_or("the viewer asked its terminal for them", seen, || {
        modes(&view).contains("keys1") && fact(&asks, "size") == "40 24"
    });
    // Ctrl-I is not Tab and Shift-Enter is not Enter: each reaches the program as it was sent.
    fs::write(view.join("in"), "a\x1b[105;5ub\tc\x1b[13;2ud\r").unwrap();
    until("the keys arrive apart", || shown(&asks).ends_with("^[[?1ua^[[105;5ub^Ic^[[13;2ud^M"));
    // The prefix is found in that form too, its key runs the script, and neither is typed.
    fs::write(view.join("in"), "\x1b[98;5uo").unwrap();
    until("the other pane has the keys", || read(&inner.0.join("main/layout")).contains("focus=plain"));
    // That program asked for nothing: the terminal is put back to telling keys the old way.
    until_or("the viewer took the flags back", seen, || !modes(&view).contains("keys"));
    fs::write(view.join("in"), "x\ty").unwrap();
    until("plain keys arrive", || shown(&plain) == "x^Iy");
    assert!(shown(&asks).ends_with("d^M"), "{}", shown(&asks));
    // Back on the pane that asked, they are in force again.
    fs::write(view.join("in"), "\x02o").unwrap();
    until_or("the flags are asked for again", seen, || modes(&view).contains("keys1"));
    // The program that was told "none" before the viewer came still gets keys the old way: what
    // it was told stays true.
    assert!(!modes(&early).contains("keys") && shown(&early) == "^[[?0u", "{}", modes(&early));
    // When the viewer leaves, the terminal has the flags it had: 4, on its alternate screen.
    let ctl = fs::read_dir(&inner.0)
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().starts_with(".view."));
    fs::write(ctl.unwrap().path(), "quit\n").unwrap();
    until("the terminal is as it was found", || shown(&view).contains("^[[?4u"));
}

#[test]
fn a_pane_says_where_its_program_is_and_what_it_wants_noticed_and_the_viewer_passes_it_on() {
    let (outer, inner) = (Session::new("gouter"), Session::new("ginner"));
    // The keys as they come with fux.
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&inner.0).unwrap();
    let copied = Command::new("cp").arg("-R").arg(concat!(env!("CARGO_MANIFEST_DIR"), "/keys")).arg(&keys).status();
    assert!(copied.unwrap().success());
    let raw = "stty raw -echo; printf '\\033[?1004h'; exec cat -vt";
    let (p1, p2) = (inner.start("p1", &["sh", "-c", raw]), inner.start("p2", &["sh", "-c", raw]));
    fs::write(inner.0.join("main/layout"), "* p1 focus=p1\n  p2 focus=p2\n").unwrap();
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let path = format!("PATH={}", std::env::var("PATH").unwrap());
    let command = ["env", &path, &format!("FUXN_DIR={}", inner.0.display()), &format!("FUXN_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    let shown = |pane: &Path| read(&pane.join("screen")).replace('\n', "");
    // The pane that has the keys is told so, having asked (mode 1004), and the viewer asks its
    // own terminal to tell it of the window's focus.
    until("the first pane is told it has the keys", || shown(&p1) == "^[[I");
    until("the viewer asked its terminal", || fact(&view, "modes").contains("focus"));
    // Where a program says it is (OSC 7) is the pane's `cwd`; a place on another machine is no place here.
    let print = inner.start("print", &["sh", "-c", "printf '\\033]7;file:///usr/share\\007'; sleep 600"]);
    until("the directory is told", || read(&print.join("cwd")) == "/usr/share\n");
    let far = inner.start(
        "far",
        &["sh", "-c", "printf '\\033]7;file:///usr\\007\\033]7;file://elsewhere.example/home/x\\007'; sleep 600"],
    );
    until("the far pane runs", || {
        fact(&far, "state") == "running" && read(&far.join("screen.styled")).is_empty().eq(&false)
    });
    assert!(!far.join("cwd").exists());
    // Anyone may have something noticed through `ctl`; a program rings the bell. Each is the
    // pane's `notice`, with a number that grows. The viewer passes a new one on to its own
    // terminal once and marks the tab until its pane has had the keys.
    until("the viewer has looked at the panes once", || fact(&view, "modes").contains("focus"));
    std::thread::sleep(Duration::from_millis(600));
    fs::write(p2.join("ctl"), "notify the build is done\n").unwrap();
    until("the notice is written", || read(&p2.join("notice")) == "1 the build is done\n");
    until("the viewer passed it on", || read(&view.join("notice")).ends_with(" p2: the build is done\n"));
    fs::write(view.join("in"), "\x02").unwrap();
    until("the tab is marked", || read(&view.join("screen")).contains(" p2! "));
    // The terminal's window loses and gains the keys with the menu open: the menu stays, and
    // the pane that asked is told.
    fs::write(view.join("in"), "\x1b[O\x1b[I").unwrap();
    until("the pane is told of the window", || shown(&p1) == "^[[I^[[O^[[I");
    assert!(read(&view.join("screen")).contains(" p2! "));
    // The next tab: the pane has the keys, the mark goes, and the panes are told who has them.
    fs::write(view.join("in"), "n").unwrap();
    until("the second pane is told", || shown(&p2) == "^[[I" && shown(&p1) == "^[[I^[[O^[[I^[[O");
    fs::write(view.join("in"), "\x02").unwrap();
    until("the mark is gone", || {
        read(&view.join("screen")).contains(" p2 ") && !read(&view.join("screen")).contains("p2!")
    });
    fs::write(view.join("in"), "\x1b").unwrap();
    // A program that stops asking (it ends, and a shell is there again) is told nothing more:
    // not when it stops, and not when its pane loses the keys.
    let asks_then_stops = "stty raw -echo; printf '\\033[?1004h'; until [ -e \"$FUXN_PANE/go\" ]; do sleep 0.05; done; printf '\\033[?1004l'; exec cat -vt";
    let quits = inner.start("quits", &["sh", "-c", asks_then_stops]);
    until("it asks", || fact(&quits, "modes") == "focus");
    fs::write(inner.0.join("main/layout"), "  p1 focus=p1\n  p2 focus=p2\n* quits focus=quits\n").unwrap();
    until("the second pane lost the keys", || shown(&p2).ends_with("^[[I^[[O"));
    std::thread::sleep(Duration::from_millis(300));
    fs::write(quits.join("go"), "").unwrap();
    until("it was told it had the keys, and stops asking", || {
        shown(&quits) == "^[[I" && fact(&quits, "modes").is_empty()
    });
    std::thread::sleep(Duration::from_millis(300));
    fs::write(inner.0.join("main/layout"), "  p1 focus=p1\n* p2 focus=p2\n  quits focus=quits\n").unwrap();
    until("the second pane has the keys again", || shown(&p2).ends_with("^[[O^[[I"));
    assert_eq!(shown(&quits), "^[[I");
    let bell = inner.start("bell", &["sh", "-c", "sleep 0.7; printf '\\007'; sleep 600"]);
    until("the bell is a notice with no words", || read(&bell.join("notice")) == "1\n");
    // A pane that is new, and has something to notice at once: that is new too.
    inner.start("quick", &["sh", "-c", "printf '\\033]9;done at once\\007'; sleep 600"]);
    until("the viewer passed it on", || read(&view.join("notice")).ends_with(" quick: done at once\n"));
    // A program in the middle of drawing (mode 2026) is not shown until it is done: half a
    // frame is never seen. One that never says it is done is shown after half a second, however
    // often it says that it draws.
    let draws = inner.start("draws", &["sh", "-c", "printf '\\033[?2026hhalf'; sleep 0.3; printf ' whole\\033[?2026l'; sleep 0.3; printf '\\033[?2026h stuck'; while sleep 0.1; do printf '\\033[?2026h'; done"]);
    let start = Instant::now();
    while !read(&draws.join("screen")).contains("stuck") {
        assert!(!read(&draws.join("screen")).starts_with("half\n"), "half a frame was shown");
        assert!(start.elapsed() < Duration::from_secs(10), "a program that never ends its drawing froze its pane");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(start.elapsed() > Duration::from_millis(900));
    // The find key lists the panes in a pane of its own, with whatever finder there is (a
    // numbered list if none): the pane chosen gets the keys.
    fs::write(inner.0.join("main/layout"), "* p1 focus=p1\n  p2 focus=p2\n").unwrap();
    until("the first tab is shown", || shown(&p1).ends_with("^[[I"));
    fs::write(view.join("in"), "\x02fp").unwrap();
    until("the list is shown", || {
        read(&view.join("screen")).contains("p2") && read(&view.join("screen")).contains("draws")
    });
    std::thread::sleep(Duration::from_millis(300));
    // (A finder takes a moment to narrow its list to what was typed.)
    fs::write(view.join("in"), "p2").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    fs::write(view.join("in"), "\r").unwrap();
    until("the pane chosen has the keys", || read(&inner.0.join("main/layout")).contains("* p2 focus=p2"));
}

#[test]
fn the_marks_of_a_shell_reach_into_history_and_the_keys_open_an_output_and_a_prompt() {
    let session = Session::new("marks");
    fs::create_dir_all(&session.0).unwrap();
    let keys = session.0.join(".keys");
    assert!(
        Command::new("cp")
            .arg("-R")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/keys"))
            .arg(&keys)
            .status()
            .unwrap()
            .success()
    );
    // A program that marks as a shell does: two commands (the first with more output than the
    // screen has rows, and a line that wraps), then a prompt.
    let shell = "m() { printf '\\033]133;%s\\007' \"$1\"; }; \
        m A; printf '$ '; m B; echo first; m C; seq 101 112; echo a-long-line-that-wraps-in-twelve; m 'D;0'; \
        m A; printf '$ '; m B; echo second; m C; echo alpha; echo beta; m 'D;1'; m A; printf '$ '; m B; sleep 600";
    let mut start = session.fux();
    start.env("COLUMNS", "12").env("LINES", "5").args(["start", "sh", "--", "sh", "-c", shell]);
    let pane = PathBuf::from(String::from_utf8(start.output().unwrap().stdout).unwrap().trim());
    until("the marks are there", || read(&pane.join("marks")).lines().count() == 10);
    let marks = read(&pane.join("marks"));
    let (first, last) =
        ("0 0 prompt\n0 2 command\n1 0 output\n14 0 end 0\n14 0 prompt\n", "17 0 end 1\n17 0 prompt\n17 2 command\n");
    assert!(marks.starts_with(first) && marks.ends_with(last), "{marks}");
    assert!(fact(&pane, "top").parse::<usize>().unwrap() > 10);
    // An editor for the test: it is called `vim`, so it is given a line number; it notes that
    // and copies the file it was to open.
    let out = session.0.join("out");
    fs::write(
        session.0.join("vim"),
        "#!/bin/sh\nfor a; do f=$a; done\necho \"$1\" > \"$OUT.at\"\ncp \"$f\" \"$OUT\"\n",
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(session.0.join("vim")).status().unwrap();
    let key = |name: &str| {
        let _ = fs::remove_file(&out);
        let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
        let mut script = Command::new(keys.join("C-b").join(name));
        script.env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap())).env("EDITOR", session.0.join("vim"));
        script.env("OUT", &out).env("FUXN_DIR", &session.0).env("FUXN_KEYS", &keys).env("FUXN_WS", "main");
        assert!(script.env("FUXN_PANE", &pane).status().unwrap().success());
        until("the editor was given the file", || out.exists() && session.0.join("out.at").exists());
        (read(&out), read(&session.0.join("out.at")).trim().trim_start_matches('+').parse::<usize>().unwrap())
    };
    // The last command's output, and nothing else.
    assert_eq!(key("S-c"), ("alpha\nbeta\n".to_string(), 1));
    // The whole text at the prompt before this one; its first command's output is whole, the
    // wrapped line one line, though most of it is in history.
    let (text, line) = key("p");
    assert_eq!(text.lines().nth(line - 1), Some("$ second"), "{text}");
    assert!(
        text.starts_with("$ first\n101\n")
            && text.contains("\n112\na-long-line-that-wraps-in-twelve\n$ second\nalpha\n"),
        "{text}"
    );
    // History in the editor opens at the text's end.
    let (text, line) = key("c");
    assert_eq!((text.lines().count(), text.lines().last()), (line, Some("$")));
    // A command that prints nothing has no output to open, and the key says so; one whose
    // output does not end its line has that output, every character of it whole, without the
    // prompt that follows on the line. (And the editor is run as `FUXN_EDIT` says.)
    let quiet = "m() { printf '\\033]133;%s\\007' \"$1\"; }; \
        m A; printf '$ '; m B; echo printf; m C; printf 'e\\314\\201\\347\\225\\214'; m 'D;0'; \
        m A; printf '$ '; m B; sleep 1; echo true; m C; m 'D;0'; m A; printf '$ '; m B; sleep 600";
    let pane = session.start("quiet", &["sh", "-c", quiet]);
    until("the first command is done", || read(&pane.join("marks")).lines().count() == 6);
    let run_in = |name: &str, pane: &Path, edit: String| {
        let bin = Path::new(env!("CARGO_BIN_EXE_fux")).parent().unwrap().display().to_string();
        let mut script = Command::new(keys.join("C-b").join(name));
        script.env("PATH", format!("{bin}:{}", std::env::var("PATH").unwrap())).env("FUXN_EDIT", edit);
        script.env("OUT", &out).env("FUXN_DIR", &session.0).env("FUXN_KEYS", &keys).env("FUXN_WS", "main");
        String::from_utf8(script.env("FUXN_PANE", pane).output().unwrap().stdout).unwrap()
    };
    let run = |name: &str| run_in(name, &pane, format!("{} line-%l %f", session.0.join("vim").display()));
    let _ = fs::remove_file(&out);
    assert_eq!(run("S-c"), "");
    until("the editor was given the output", || read(&out) == "e\u{301}界\n");
    assert_eq!(read(&session.0.join("out.at")), "line-1\n");
    until("the second command is done", || read(&pane.join("marks")).lines().count() == 10);
    assert_eq!(
        run("S-c"),
        "nothing of the last command's output is here (it printed none, or the screen was cleared)\n"
    );
    // An editor still open when its pane is closed with the close key: the file it was given is
    // in the pane's own folder, and goes with it.
    assert_eq!(run_in("c", &pane, "tail -f %f".into()), "");
    let editor =
        || fs::read_dir(session.0.join("main")).unwrap().flatten().find(|pane| pane.path().join("text").exists());
    until("the editor has its file", || editor().is_some());
    assert_eq!(run_in("x/y", &editor().unwrap().path(), String::new()), "");
    let left = || fs::read_dir(&session.0).unwrap().flatten().filter(|file| file.file_name() != "out.at").count();
    let before = left();
    until("the pane and its file are gone", || {
        fs::read_dir(&session.0).unwrap().flatten().all(|file| {
            let name = file.file_name().to_string_lossy().into_owned();
            !name.starts_with(".text.") && !name.starts_with(".closed.")
        })
    });
    assert!(left() <= before);
}

#[test]
fn a_key_the_viewer_took_is_its_own_to_its_release_and_a_release_follows_its_press() {
    // As the test before: the viewer in a pane of an outer session whose terminal is said to
    // take every flag of the kitty keyboard protocol.
    let (outer, inner) = (Session::new("router"), Session::new("rinner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(keys.join("C-b")).unwrap();
    fs::write(keys.join("C-b/o"), "#!/bin/sh\n# other\nfux layout focus next\n").unwrap();
    Command::new("chmod").arg("+x").arg(keys.join("C-b/o")).status().unwrap();
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "31\n").unwrap();
    let raw = "stty raw -echo; exec cat -vt";
    let plain = inner.start("plain", &["sh", "-c", raw]);
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let command = ["env", &format!("FUXN_DIR={}", inner.0.display()), &format!("FUXN_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    until("the session knows what the terminal takes", || read(&inner.0.join("keyboard")) == "31\n");
    // A program asks for escape codes told apart, presses and releases, and every key as a code.
    let asking = "stty raw -echo; printf '\\033[>11u\\033[?u'; exec cat -vt";
    let asks = inner.start("asks", &["sh", "-c", asking]);
    let shown = |pane: &Path| read(&pane.join("screen")).replace('\n', "");
    until("it is told they are in force", || shown(&asks) == "^[[?11u" && fact(&asks, "modes") == "keys11");
    fs::write(inner.0.join("main/layout"), "* row(asks plain) focus=asks\n").unwrap();
    until("the viewer asked its terminal for them", || fact(&view, "modes").contains("keys11"));
    // A key typed: its press and its release reach the program. One held down stays down for now.
    fs::write(view.join("in"), "\x1b[97;1:1u\x1b[97;1:3u\x1b[120;1:1u").unwrap();
    until("the keys arrive", || shown(&asks) == "^[[?11u^[[97;1:1u^[[97;1:3u^[[120;1:1u");
    // Ctrl goes down (a terminal that tells of every key tells of that too: the pane has the keys
    // and gets it), then the prefix, held and let go; Shift by itself, down and up, with the menu
    // open; Ctrl is let go. The menu stays open through all of it; Ctrl's release follows its
    // press to the pane, and the pane hears of nothing else.
    let ctrl = "\x1b[57442;5:1u\x1b[98;5:1u\x1b[98;5:2u\x1b[98;5:3u\x1b[57441;2:1u\x1b[57441;1:3u\x1b[57442;1:3u";
    fs::write(view.join("in"), ctrl).unwrap();
    until("the menu is open, and stays", || read(&view.join("screen")).contains("o  other"));
    let held = "^[[?11u^[[97;1:1u^[[97;1:3u^[[120;1:1u^[[57442;5:1u^[[57442;1:3u";
    until("Ctrl went down and up in the pane", || shown(&asks) == held);
    assert!(read(&view.join("screen")).contains("o  other"));
    // The menu's key and its release: the script runs once and the other pane has the keys.
    fs::write(view.join("in"), "\x1b[111;1:1u\x1b[111;1:3u").unwrap();
    until("the other pane has the keys", || read(&inner.0.join("main/layout")).contains("focus=plain"));
    until("the terminal is asked for no flags", || !fact(&view, "modes").contains("keys"));
    // The keys left the first pane with a key of its still down: it is told there that the key
    // is let go, at once (this terminal, asked for no flags now, would never tell of it). What
    // the terminal still tells of that key reaches no pane, nor does a release of a key nobody
    // pressed. What is typed here arrives here.
    until("the first pane's key is let go there", || shown(&asks) == format!("{held}^[[120;1:3u"));
    fs::write(view.join("in"), "\x1b[120;1:2u\x1b[120;1:3u\x1b[121;1:3uz").unwrap();
    until("the plain pane has what was typed there", || shown(&plain) == "z");
    assert_eq!(shown(&asks), format!("{held}^[[120;1:3u"));
    let held = format!("{held}^[[120;1:3u");
    // A key the viewer took whose release never came (the terminal tells of none without the
    // flag) does not take a later press of that key for its own: the prefix, then the menu's key,
    // and `b` typed in the pane that has the keys again, down and up.
    fs::write(view.join("in"), "\x1b[98;5:1u").unwrap();
    until("the menu is open", || read(&view.join("screen")).contains("o  other"));
    fs::write(view.join("in"), "\x1b[111;1:1u\x1b[111;1:3u").unwrap();
    until("the first pane has the keys", || fact(&view, "modes").contains("keys11"));
    fs::write(view.join("in"), "\x1b[98;1:1u\x1b[98;1:3u").unwrap();
    until("b went down and up", || shown(&asks) == format!("{held}^[[98;1:1u^[[98;1:3u"));
    // Only a press makes a key someone's: a repeat with no press goes nowhere, and so does the
    // release after it. A sign sent as itself when pressed and as a sequence when let go (the
    // number of its key, then its own) is one key.
    fs::write(view.join("in"), "\x1b[119;1:2u\x1b[119;1:3u!\x1b[49:33;2:3u").unwrap();
    let held = format!("{held}^[[98;1:1u^[[98;1:3u!^[[49:33;2:3u");
    until("the sign went down and up", || shown(&asks) == held);
    // The viewer leaves with a key down in the pane: the pane is told that it is let go.
    fs::write(view.join("in"), "\x1b[99;1:1u").unwrap();
    until("c is down", || shown(&asks) == format!("{held}^[[99;1:1u"));
    let ctl = fs::read_dir(&inner.0)
        .unwrap()
        .flatten()
        .find(|entry| entry.file_name().to_string_lossy().starts_with(".view."));
    fs::write(ctl.unwrap().path(), "quit\n").unwrap();
    until("c is let go", || shown(&asks) == format!("{held}^[[99;1:1u^[[99;1:3u"));
    assert_eq!(shown(&plain), "z");
}

#[test]
fn a_held_key_does_again_what_its_press_did_and_there_only_and_a_program_started_anew_gets_none_of_it() {
    let (outer, inner) = (Session::new("houter"), Session::new("hinner"));
    let keys = inner.0.join(".keys");
    // Two menus that stay open, with the same key in both.
    for menu in ["r", "m"] {
        fs::create_dir_all(keys.join("C-b").join(menu)).unwrap();
        fs::write(keys.join("C-b").join(menu).join(".repeat"), "").unwrap();
        let script = keys.join("C-b").join(menu).join("h");
        fs::write(&script, format!("#!/bin/sh\n# act\necho {menu} >> \"$FUXN_KEYS/ran\"\n")).unwrap();
        Command::new("chmod").arg("+x").arg(script).status().unwrap();
    }
    fs::create_dir_all(&outer.0).unwrap();
    fs::write(outer.0.join("keyboard"), "31\n").unwrap();
    fs::create_dir_all(&inner.0).unwrap();
    fs::write(inner.0.join("keyboard"), "31\n").unwrap();
    let asking = ["sh", "-c", "stty raw -echo; printf '\\033[>11u\\033[?u'; exec cat -vt"];
    let (first, second) = (inner.start("first", &asking), inner.start("second", &asking));
    let layout = |edit: &[&str]| assert!(inner.fux().arg("layout").args(edit).status().unwrap().success());
    layout(&["pane", "first"]);
    let viewer = [env!("CARGO_BIN_EXE_fux"), "view"];
    let command = ["env", &format!("FUXN_DIR={}", inner.0.display()), &format!("FUXN_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &viewer[..]].concat());
    let shown = |pane: &Path| read(&pane.join("screen")).replace('\n', "");
    until("the viewer asked its terminal for the flags", || fact(&view, "modes").contains("keys11"));
    // A key (down and up) for each of: the prefix, a menu, Escape; and `h` down, held, up.
    let tap = |key: &str| format!("\x1b[{key}:1u\x1b[{key}:3u");
    let (prefix, escape, h) = (tap("98;5"), tap("27;1"), ["\x1b[104;1:1u", "\x1b[104;1:2u", "\x1b[104;1:3u"]);
    let ran = || read(&keys.join("ran"));
    // Held in the one menu, the key acts again there.
    fs::write(view.join("in"), format!("{prefix}{}{}{}", tap("114;1"), h[0], h[1])).unwrap();
    until("the key acted twice", || ran() == "r\nr\n");
    // Still held when that menu is closed and the other opened, it does nothing in the other:
    // not what it did before, and not what a press would do there. Pressed there, it does.
    fs::write(view.join("in"), format!("{escape}{prefix}{}{}{}{}{}", tap("109;1"), h[1], h[2], h[0], h[2])).unwrap();
    until("the key acted in the other menu", || ran().ends_with("m\n"));
    // A key goes down in the first pane, and the other tab is shown: the key is let go there,
    // and a key typed right after the command that showed it is the second pane's.
    fs::write(view.join("in"), format!("{escape}\x1b[97;1:1u")).unwrap();
    until("the key is down", || shown(&first) == "^[[?11u^[[97;1:1u");
    assert_eq!(ran(), "r\nr\nm\n");
    layout(&["tab", "next"]);
    fs::write(view.join("in"), tap("122;1")).unwrap();
    until("the second pane has the keys", || shown(&second) == "^[[?11u^[[122;1:1u^[[122;1:3u");
    until("the key is let go in the first", || shown(&first) == "^[[?11u^[[97;1:1u^[[97;1:3u");
    // The first pane's program ends and another is started under its name, in the tab that is
    // not shown: what the terminal tells of the key after that is nothing to the new program.
    fs::write(first.join("ctl"), "kill\n").unwrap();
    until("the program has ended", || fact(&first, "state") != "running");
    inner.start("first", &asking);
    until("the new program is told its flags", || {
        fact(&first, "state") == "running" && shown(&first).ends_with("^[[?11u")
    });
    let new = shown(&first);
    fs::write(view.join("in"), format!("\x1b[97;1:2u\x1b[97;1:3u{}", tap("121;1"))).unwrap();
    until("the second pane has the next key", || shown(&second).ends_with("^[[121;1:1u^[[121;1:3u"));
    assert_eq!(shown(&first), new);
}

#[test]
fn a_link_a_program_makes_is_a_line_of_links_and_the_viewer_draws_it_inside_its_tile() {
    // The viewer in a pane of an outer session: what it draws as a link is in that pane's `links`.
    let (outer, inner) = (Session::new("louter"), Session::new("linner"));
    let keys = inner.0.join(".keys");
    fs::create_dir_all(&keys).unwrap();
    let address = "https://example.org/a?b=c#d";
    // At columns 3 to 6 of its second row, and again where it runs past the tile's right edge.
    // Above them two characters a terminal would join into one (a flag's two halves, the right
    // one printed first), each a link of its own, and in the third row's last cell before the
    // tile's edge a character that takes the next one into itself (U+0600), a link too.
    let prints = format!(
        "printf '\\033[1;2H\\033]8;;{address}2\\033\\\\🇧\\033]8;;\\007\\033[1;1H\\033]8;;{address}1\\033\\\\🇦\\033]8;;\\007\\033[3;40H\\033]8;;{address}3\\033\\\\\u{600}\\033]8;;\\007\\033[1;3H\\n  \\033]8;;{address}\\033\\\\link\\033]8;;\\033\\\\ plain\\n\\033[4;35H\\033]8;;{address}\\033\\\\0123456789\\033]8;;\\007'; sleep 600"
    );
    let left = inner.start("left", &["sh", "-c", &prints]);
    inner.start("right", &["sh", "-c", "echo right; sleep 600"]);
    let above = format!("0 0 1 {address}1\n0 1 1 {address}2\n1 2 4 {address}\n2 39 1 {address}3\n");
    until("the link is the pane's", || read(&left.join("links")) == format!("{above}3 34 10 {address}\n"));
    assert!(!read(&left.join("screen.styled")).contains("\x1b]"));
    fs::create_dir_all(inner.0.join("main")).unwrap();
    fs::write(inner.0.join("main/layout"), "* row(left right) focus=right\n").unwrap();
    // (Whatever ran before the viewer left a link open on its terminal: no cell of the viewer's is part of it.)
    let viewer = format!("printf '\\033]8;;http://open/\\033\\\\'; exec {} view", env!("CARGO_BIN_EXE_fux"));
    let command = ["env", &format!("FUXN_DIR={}", inner.0.display()), &format!("FUXN_KEYS={}", keys.display())];
    let view = outer.start("view", &[&command[..], &["sh", "-c", &viewer]].concat());
    // The tile is 40 columns wide and the pane takes that size: the second link's text wraps,
    // and the link is drawn on its cells in both rows, up to the tile's edge and not on the
    // line between the tiles. Each cell has its own address and no other: the flag's halves are
    // two characters still, and the line between the tiles is no part of the character before it.
    until("the pane has its tile's size", || {
        read(&left.join("links")) == format!("{above}3 34 10 {address}\n") && fact(&left, "size") == "40 24"
    });
    let drawn = format!("{above}3 34 6 {address}\n4 0 4 {address}\n");
    until("the viewer drew the links", || read(&view.join("links")) == drawn);
    assert!(read(&view.join("screen")).lines().nth(3).unwrap().contains("012345│"));
    // The pane's text scrolls into history and its first link with it; the viewer's terminal
    // then has that one no more.
    fs::write(left.join("ctl"), "resize 40 2\n").unwrap();
    until("the first link is in history", || read(&left.join("links")) == format!("3 34 10 {address}\n"));
    until("the viewer follows", || read(&view.join("links")) == format!("0 34 6 {address}\n1 0 4 {address}\n"));
}
