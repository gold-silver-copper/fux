//! A program that is not fux and carries it, as koh does: fux's command line is under its word
//! `fux`, and the word `hold`, by which fux starts a pane's holder as the program it runs in, is
//! fux's too. Run by `cargo test` it is the test of that (`main` is its own: no test harness), and
//! it runs whatever words it is given there: they are a filter's, of the tests that have names.
#![allow(clippy::unwrap_used)]

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Output, Stdio};
use std::time::{Duration, Instant};

/// A variable of the test's own: set, this program was run by the test (or by the fux in it), and its
/// words are a program's. (Not set, `cargo test hold` has given it a filter and no word of fux's.)
const CARRIED: &str = "CARRIED";

fn main() -> ExitCode {
    if std::env::var_os(CARRIED).is_none() {
        return test();
    }
    let words: Vec<OsString> = std::env::args_os().skip(1).collect();
    match words.first().and_then(|word| word.to_str()) {
        Some("fux") => fux::command(words.into_iter().skip(1)),
        Some("hold") => fux::command(words),
        // (As koh has it for a terminal at the far end of a link: one pane, and the keyboard's
        // flags that terminal takes.)
        Some("show") => match words.get(1..) {
            Some([pane]) if fux::view_pane(pane.into(), 5).is_ok() => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        },
        _ => ExitCode::from(2),
    }
}

/// How long the test has, all of it. What it runs is fux, which ends, and a `ps`; the pane's
/// program ends by itself in half of this, and its holder with it.
const LIMIT: Duration = Duration::from_secs(60);

fn test() -> ExitCode {
    std::thread::spawn(|| {
        std::thread::sleep(LIMIT);
        eprintln!("carrier: not over in {LIMIT:?}");
        std::process::exit(124);
    });
    let me = std::env::current_exe().unwrap();
    let tmp = std::env::temp_dir();
    let root = tmp.canonicalize().unwrap_or(tmp).join(format!("fux-carrier-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    type Test = fn(&Path, &Path) -> Result<(), String>;
    let tests: [(&str, Test); 5] = [
        ("a_program_that_is_not_fux_starts_a_pane_and_is_its_holder", carries),
        ("a_viewer_of_one_pane_asks_its_terminal_nothing_believes_it_nothing_and_ends_with_the_pane", shows),
        ("a_megabyte_typed_at_a_viewer_of_one_pane_is_typed_whole_with_the_keys_around_it_in_their_order", types),
        ("a_viewer_of_one_pane_whose_program_reads_nothing_still_draws_and_ends_when_told", waits),
        ("a_viewer_of_one_pane_passes_a_bell_and_a_notice_on_and_writes_neither_on_the_screen", rings),
    ];
    let mut failed = false;
    for (name, test) in tests {
        let wrong = test(&me, &root).err();
        // Whatever became of it, the pane goes: a holder that still lives finds its folder gone and
        // hangs up (and may write a file while the folder is being removed: then once more).
        let end = Instant::now() + Duration::from_secs(5);
        while std::fs::remove_dir_all(&root).is_err() && root.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        match &wrong {
            None => println!("test {name} ... ok"),
            Some(why) => eprintln!("test {name} ... FAILED\n{why}"),
        }
        failed |= wrong.is_some();
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// A file's bytes, read without waiting.
fn bytes(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = fuxix::open(path, false).and_then(|mut file| file.read_to_end(&mut bytes));
    bytes
}

/// A file's text, read without waiting.
fn read(path: &Path) -> String {
    let mut text = String::new();
    let _ = fuxix::open(path, false).and_then(|mut file| file.read_to_string(&mut text));
    text
}

/// This program with some words, where nothing of whoever runs the tests is found.
fn carried(me: &Path, root: &Path, words: &[&str]) -> Command {
    let mut command = Command::new(me);
    command.args(words).stdin(Stdio::null()).env_remove("COLUMNS").env_remove("LINES");
    command.env("XDG_RUNTIME_DIR", root).env("XDG_CONFIG_HOME", root).env("HOME", root).env(CARRIED, "1");
    command
}

fn check(right: bool, wrong: String) -> Result<(), String> {
    if right { Ok(()) } else { Err(wrong) }
}

/// A pane of the test's own workspace, running `says` in a shell.
fn start(me: &Path, root: &Path, name: &str, says: &str) -> Result<std::path::PathBuf, String> {
    let pane = root.join("work").join(name);
    let out = carried(me, root, &["fux", "start", &pane.to_string_lossy(), "--", "sh", "-c", says]).output();
    check(out.is_ok_and(|out| out.status.success()), format!("{name} was not started")).map(|()| pane)
}

/// `fux::view_pane` of a pane, as koh runs it: on a pty of the test's, 40 by 10, which is the
/// terminal. What the viewer writes there is read as bytes, and what is written there is typed.
struct Shown {
    terminal: std::fs::File,
    viewer: std::process::Child,
    drawn: Vec<u8>,
}

impl Drop for Shown {
    fn drop(&mut self) {
        let _ = self.viewer.kill().and_then(|()| self.viewer.wait());
    }
}

impl Shown {
    fn of(me: &Path, root: &Path, pane: &Path) -> Result<Shown, String> {
        let size = fuxix::Winsize { cols: 40, rows: 10, xpixel: 400, ypixel: 200 };
        let (terminal, slave) = fuxix::pty(size).map_err(|e| format!("no pty: {e}"))?;
        let end = |slave: &std::fs::File| slave.try_clone().map(Stdio::from).map_err(|e| e.to_string());
        // (The pty's other end is the viewer's alone once it runs: when the viewer ends, the pty does.)
        let mut viewer = carried(me, root, &["show", &pane.to_string_lossy()]);
        viewer.stdin(end(&slave)?).stdout(end(&slave)?).stderr(Stdio::from(slave));
        let viewer = viewer.spawn().map_err(|e| format!("the viewer was not started: {e}"))?;
        Ok(Shown { terminal, viewer, drawn: Vec::new() })
    }

    /// Reads what the viewer writes until `ready` says so of all it wrote, the viewer has ended,
    /// or twenty seconds are over: all it wrote so far.
    fn draws(&mut self, mut ready: impl FnMut(&str) -> bool) -> String {
        let by = Instant::now() + Duration::from_secs(20);
        loop {
            let mut piece = [0; 4096];
            let read = self.terminal.read(&mut piece);
            let waits = matches!(&read, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock);
            let n = read.unwrap_or(0);
            self.drawn.extend(piece.iter().take(n));
            let drawn = String::from_utf8_lossy(&self.drawn).into_owned();
            // (No more comes of a pty whose other end is closed: the viewer has ended.)
            if ready(&drawn) || (n == 0 && !waits) || Instant::now() > by {
                return drawn;
            }
            if waits {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// Types at the terminal: as much as it takes within `long`, and how much that was.
    fn types(&mut self, mut keys: &[u8], long: Duration) -> usize {
        let (all, by) = (keys.len(), Instant::now() + long);
        while !keys.is_empty() && Instant::now() < by {
            match self.terminal.write(keys) {
                Ok(n) => keys = keys.get(n..).unwrap_or_default(),
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
            // (A terminal reads what is drawn on it while it is typed at.)
            self.draws(|_| true);
        }
        all - keys.len()
    }

    /// How the viewer ended, if it has within `long`.
    fn ended(&mut self, long: Duration) -> Option<std::process::ExitStatus> {
        let by = Instant::now() + long;
        while matches!(self.viewer.try_wait(), Ok(None)) && Instant::now() < by {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.viewer.try_wait().ok().flatten()
    }
}

/// Waits, twenty seconds at most, until `done` says so.
fn until(what: &str, mut done: impl FnMut() -> bool) -> Result<(), String> {
    let by = Instant::now() + Duration::from_secs(20);
    while !done() {
        check(Instant::now() < by, format!("not in 20 s: {what}"))?;
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// `fux::view_pane`, as koh runs it on a pty for a terminal at the far end of a link.
fn shows(me: &Path, root: &Path) -> Result<(), String> {
    // Two panes of one workspace, and the first is the one shown: a program that reads its
    // terminal raw into a file.
    let reads = "stty raw -echo; printf shown; exec cat > \"$0\"";
    let got = root.join("got");
    let pane = start(me, root, "one", &reads.replace("$0", &got.to_string_lossy()))?;
    start(me, root, "two", "printf another; exec sleep 30")?;
    let mut shown = Shown::of(me, root, &pane)?;
    let first = shown.draws(|drawn| drawn.contains("shown"));
    // It draws that pane on the alternate screen, and sets the keyboard's flags aside as one that
    // was told the terminal takes some; it asks nothing: not what the terminal is, its colours, its
    // keyboard or a mode. And of the workspace's other pane it draws nothing.
    check(
        first.contains("shown") && first.contains("\x1b[?1049h\x1b[2J") && first.contains("\x1b[>u"),
        format!("{first:?}"),
    )?;
    let asked = ["\x1b[c", "\x1b[?u", ";?\x1b\\", "$p", "another"].into_iter().find(|asked| first.contains(asked));
    check(asked.is_none(), format!("the viewer wrote {asked:?}: {first:?}"))?;
    // What a terminal sends that looks like an answer was typed there: it is the pane's, as it
    // came, and the viewer believes none of it (no mode is turned on for it, no keyboard taken).
    // A line on its `ctl` that would have a viewer show another workspace changes nothing.
    let answers = b"\x1b[?2027;2$y\x1b[?0u\x1b[?62;4c\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\typed";
    shown.types(answers, Duration::from_secs(5));
    until("the pane has what was typed", || bytes(&got) == answers)?;
    let own = std::fs::read_dir(root).map_err(|e| e.to_string())?.flatten().map(|entry| entry.path());
    let own =
        own.into_iter().find(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(".view.")));
    let ctl = own.ok_or("the viewer has no folder")?.join("ctl");
    let tell = |line: &[u8]| fuxix::open(&ctl, true).and_then(|mut ctl| ctl.write_all(line)).map_err(|e| e.to_string());
    tell(b"workspace elsewhere\nsome words\n")?;
    std::thread::sleep(Duration::from_millis(300));
    let then = shown.draws(|_| true);
    let believed =
        ["\x1b[?2027h", "\x1b[=1u", "elsewhere", "some words", "another"].into_iter().find(|it| then.contains(it));
    check(believed.is_none(), format!("the viewer wrote {believed:?}: {then:?}"))?;
    // Nothing was written of the terminal or of a layout, and it has no keys anywhere.
    let made =
        ["work/layout", "colours", "keyboard", "keys", "fux/keys"].into_iter().find(|name| root.join(name).exists());
    check(made.is_none(), format!("the viewer made {made:?}"))?;
    // The pane ends, and the viewer with it, by itself: with the last screen drawn, and nothing
    // written after it (whoever has the terminal puts it back).
    let mut ctl = fuxix::open(pane.join("ctl"), true).map_err(|e| format!("ctl: {e}"))?;
    ctl.write_all(b"kill\n").map_err(|e| format!("ctl: {e}"))?;
    let last = shown.draws(|_| false);
    let ended = shown.ended(Duration::from_secs(5));
    check(ended.is_some_and(|ended| ended.success()), format!("the viewer did not end with its pane: {ended:?}"))?;
    check(last.contains("killed 1") && last.ends_with("\x1b[?2026l"), format!("the end: {last:?}"))?;
    let left = ["\x1b[?1049l", "\x1b[?2027l", "\x1b[<u"].into_iter().find(|it| last.contains(it));
    check(left.is_none(), format!("the viewer put the terminal back itself: {left:?}"))
}

/// A megabyte pasted at a viewer of one pane, into a program that begins to read a moment later
/// and then reads slowly (perl, as the tests of panes have it too); before it a key's press, after
/// it that key's release, as a terminal tells them to a program that asks (the kitty protocol's
/// flag 2). While the paste waits the program asks to be told of the focus, and is: behind what
/// waits, not in place of it.
fn types(me: &Path, root: &Path) -> Result<(), String> {
    std::fs::create_dir_all(root)
        .and_then(|()| std::fs::write(root.join("keyboard"), "31\n"))
        .map_err(|e| e.to_string())?;
    // (It reads a piece every two milliseconds: slower than a viewer has what is typed.)
    let slowly = "while (sysread(STDIN, $b, 4096)) { syswrite(STDOUT, $b); select(undef, undef, undef, 0.002) }";
    let reads = format!(
        "stty raw -echo; printf '\\033[>3u'; until [ -e \"$0.ask\" ]; do sleep 0.05; done; printf '\\033[?1004h'; until [ -e \"$0.go\" ]; do sleep 0.05; done; exec perl -e '{slowly}' > \"$0\""
    );
    let got = root.join("got");
    let pane = start(me, root, "typed", &reads.replace("$0", &got.to_string_lossy()))?;
    until("the program asks for keys let go", || read(&pane.join("modes")).contains("keys"))?;
    let mut shown = Shown::of(me, root, &pane)?;
    shown.draws(|drawn| drawn.contains("\x1b[?2026l"));
    let (press, release, focus) = ("\x1b[97;1:1u", "\x1b[97;1:3u", "\x1b[I");
    let pasted: String = (0..1 << 17).map(|n| format!("{n:07}\n")).collect();
    let typed = format!("{press}\x1b[200~{pasted}\x1b[201~{release}");
    // The terminal takes a part of it and then no more: the viewer reads no further while the
    // pane has not taken what it has.
    let first = shown.types(typed.as_bytes(), Duration::from_secs(1));
    check(first < typed.len(), format!("all {first} bytes were taken while nothing read them"))?;
    // The program asks for the focus now, and the viewer tells it: into a pane that is full.
    std::fs::write(root.join("got.ask"), "").map_err(|e| e.to_string())?;
    until("the program asks to be told of the focus", || {
        shown.draws(|_| true);
        read(&pane.join("modes")).contains("focus")
    })?;
    shown.types(&[], Duration::from_millis(300));
    std::thread::sleep(Duration::from_millis(300));
    // Then the program reads, and the rest is taken.
    std::fs::write(root.join("got.go"), "").map_err(|e| e.to_string())?;
    let rest = shown.types(typed.as_bytes().get(first..).unwrap_or_default(), Duration::from_secs(60));
    check(first + rest == typed.len(), format!("{} of {} bytes were taken", first + rest, typed.len()))?;
    // All of it arrives, once and in its order: the press, the paste, the release (the paste
    // without its marks: this program did not ask for them). And the focus report, whole, once,
    // somewhere in the paste: after what the pane had not taken when it was told.
    let whole = format!("{press}{pasted}{release}");
    let arrived = || String::from_utf8_lossy(&bytes(&got)).into_owned();
    until("all of it arrives", || {
        shown.draws(|_| true);
        arrived().len() >= whole.len() + focus.len()
    })?;
    let arrived = arrived();
    let told = arrived.find(focus).ok_or(format!("{} bytes arrived, and no focus report", arrived.len()))?;
    check(told > press.len() + (64 << 10), format!("the focus report came at byte {told}, before what waited"))?;
    let typed = arrived.replacen(focus, "", 1);
    let same = typed.bytes().zip(whole.bytes()).take_while(|(a, b)| a == b).count();
    check(typed == whole, format!("{} bytes arrived, the first {same} as they were typed", typed.len()))
}

/// A program rings the bell and then says some words (OSC 9) at a viewer of one pane: the terminal
/// is rung and told the words, once each, and nothing is written over the program's screen, whose
/// every row is the program's.
fn rings(me: &Path, root: &Path) -> Result<(), String> {
    let says = "printf ready; until [ -e \"$0\" ]; do sleep 0.05; done; printf '\\a'; until [ -e \"$0.words\" ]; do sleep 0.05; done; printf '\\033]9;said-aloud\\033\\\\'; exec sleep 30";
    let go = root.join("go");
    let pane = start(me, root, "rings", &says.replace("$0", &go.to_string_lossy()))?;
    let mut shown = Shown::of(me, root, &pane)?;
    shown.draws(|drawn| drawn.contains("ready"));
    // (What is there at a viewer's first look is old, and is not rung: the bell comes after it.)
    std::thread::sleep(Duration::from_millis(600));
    std::fs::write(&go, "").map_err(|e| e.to_string())?;
    let rung = shown.draws(|drawn| drawn.contains('\x07'));
    check(rung.contains('\x07'), format!("the terminal was not rung: {rung:?}"))?;
    std::fs::write(root.join("go.words"), "").map_err(|e| e.to_string())?;
    let told = shown.draws(|drawn| drawn.contains("said-aloud\x1b\\"));
    check(told.contains("\x1b]9;rings: said-aloud\x1b\\"), format!("the terminal was not told: {told:?}"))?;
    // (A frame that had it in the corner would be drawn by now.)
    std::thread::sleep(Duration::from_millis(300));
    let all = shown.draws(|_| true);
    let rings = all.matches('\x07').count();
    check(rings == 1 && all.matches("said-aloud").count() == 1, format!("rung {rings} times: {all:?}"))?;
    check(!all.contains("bell"), format!("the bell was written on the screen: {all:?}"))
}

/// A program that reads nothing, and more is typed at its viewer than its pane takes: the
/// viewer waits for it, and whoever types with it. It draws meanwhile, and ends when it is told.
fn waits(me: &Path, root: &Path) -> Result<(), String> {
    let pane = start(me, root, "deaf", "stty raw -echo; printf deaf; exec sleep 600")?;
    let mut shown = Shown::of(me, root, &pane)?;
    shown.draws(|drawn| drawn.contains("deaf"));
    let typed = vec![b'x'; 2 << 20];
    let taken = shown.types(&typed, Duration::from_secs(2));
    check((1..typed.len()).contains(&taken), format!("{taken} bytes were taken by a program that reads nothing"))?;
    // Nothing of it was dropped to make room: a moment later the terminal takes no more.
    let more = shown.types(&typed, Duration::from_millis(500));
    check(more < 64 << 10, format!("{more} more bytes were taken"))?;
    // The terminal is made another size: the viewer draws at it, and asks the pane to be it.
    let size = fuxix::Winsize { cols: 50, rows: 12, xpixel: 500, ypixel: 240 };
    let before = shown.drawn.len();
    fuxix::set_winsize(&shown.terminal, size).map_err(|e| e.to_string())?;
    until("the pane is asked to be the terminal's size", || {
        shown.draws(|_| true);
        read(&pane.join("size")).trim() == "50 12"
    })?;
    let drawn = String::from_utf8_lossy(shown.drawn.get(before..).unwrap_or_default()).into_owned();
    check(drawn.contains("\x1b[2J") && drawn.contains("deaf"), format!("not drawn anew: {drawn:?}"))?;
    // Told to leave on its `ctl`, it leaves, with all of that still waiting.
    let own = std::fs::read_dir(root).map_err(|e| e.to_string())?.flatten().map(|entry| entry.path());
    let own =
        own.into_iter().find(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(".view.")));
    let ctl = own.ok_or("the viewer has no folder")?.join("ctl");
    fuxix::open(&ctl, true).and_then(|mut ctl| ctl.write_all(b"quit\n")).map_err(|e| e.to_string())?;
    let ended = shown.ended(Duration::from_secs(5));
    check(ended.is_some_and(|ended| ended.success()), format!("the viewer did not leave when told: {ended:?}"))
}

fn carries(me: &Path, root: &Path) -> Result<(), String> {
    let carried = |words: &[&str]| -> Result<Output, String> {
        let mut command = carried(me, root, &["fux"]);
        command.args(words).output().map_err(|e| format!("{command:?}: {e}"))
    };
    let said = |out: &Output| format!("{:?}", (out.status, String::from_utf8_lossy(&out.stderr)));

    // `start` under that word starts a pane: the folder it prints is the one named, by a path.
    let pane = root.join("work").join("pane");
    let name = pane.to_str().ok_or("the temporary directory is not text")?;
    let out = carried(&["start", name, "--", "sleep", "30"])?;
    check(out.status.success(), format!("start failed: {}", said(&out)))?;
    check(String::from_utf8_lossy(&out.stdout).trim_end() == name, format!("start printed {:?}", out.stdout))?;

    // The pane's holder is this program, with the word `hold`: there is no `fux` to be it.
    let status = read(&pane.join("status"));
    let holder = status.lines().find_map(|line| line.strip_prefix("holder ")?.parse::<u32>().ok());
    let holder = holder.ok_or(format!("no holder in status: {status:?}"))?;
    let mut ps = Command::new("ps");
    ps.args(["-ww", "-o", "args=", "-p", &holder.to_string()]).env_remove("COLUMNS").stdin(Stdio::null());
    let line = String::from_utf8_lossy(&ps.output().map_err(|e| format!("ps: {e}"))?.stdout).into_owned();
    check(line.starts_with(&format!("{} hold ", me.display())), format!("the holder {holder} is: {line:?}"))?;
    check(me.file_name().is_some_and(|name| name != "fux"), format!("{}: the test is to be no fux", me.display()))?;

    // It holds the pane's lock (the `start` that took it has ended), for as long as it lives.
    // (`kill` hangs the program up, which ends a `sleep`, and `wait` returns when the lock is free.)
    let folder = fuxix::open(&pane, false).map_err(|e| format!("{name}: {e}"))?;
    check(folder.try_lock_shared().is_err(), "nobody holds the lock of a pane that runs".into())?;
    let mut ctl = fuxix::open(pane.join("ctl"), true).map_err(|e| format!("{name}/ctl: {e}"))?;
    ctl.write_all(b"kill\n").map_err(|e| format!("{name}/ctl: {e}"))?;
    let out = carried(&["wait", name])?;
    let ended = String::from_utf8_lossy(&out.stdout).into_owned();
    check(out.status.success() && ended.ends_with("state killed 1\n"), format!("wait: {ended:?} {}", said(&out)))?;
    check(folder.try_lock_shared().is_ok(), "the lock is still held after the holder has gone".into())?;

    // The usage names fux, whatever the program is: after the program's own word it reads as it is typed.
    let out = carried(&["nothing"])?;
    let usage = String::from_utf8_lossy(&out.stderr).into_owned();
    check(!out.status.success() && usage.starts_with("fux: usage:\nfux dir "), format!("usage: {}", said(&out)))
}
