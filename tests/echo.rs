//! The echo model against the kernel it models: random modes and keys,
//! from fixed seeds, typed into a real pty that nothing reads, and what
//! comes back on the master compared byte for byte with what the model
//! says the kernel echoes.
use fux::echo::Echo;
use fuxix::poll::{Events, PollFd, poll};
use fuxix::pty::{Master, Slave};
use fuxix::terminal::{self, Modes, Size};
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

const CASES: u64 = 300;

/// At most this many cases may type something the model leaves out.
const SKIPPED_PERCENT: u64 = 20;

/// A small xorshift: the same cases on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x.wrapping_shl(13);
        x ^= x.wrapping_shr(7);
        x ^= x.wrapping_shl(17);
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next().checked_rem(n).unwrap_or(0)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        let len = u64::try_from(items.len()).ok()?;
        items.get(usize::try_from(self.below(len)).ok()?)
    }
}

/// Characters an editing or signal character may be moved to, besides
/// its usual one: never NUL or 0xff, which disable one on Linux and macOS.
const OTHER_CHARS: &[u8] = b"\x08\x7f\x15\x17\x03\x05\x12\x04\x18\x1b\r#@x";

/// A character: its usual one, disabled, or another.
fn char(rng: &mut Rng, usual: Option<u8>) -> Option<u8> {
    match rng.below(100) {
        0..80 => usual,
        80..88 => None,
        _ => rng.pick(OTHER_CHARS).copied(),
    }
}

/// A character the model leaves out: its usual one, or disabled, but
/// never moved onto a key typed often.
fn rare_char(rng: &mut Rng, usual: u8) -> Option<u8> {
    rng.chance(85).then_some(usual)
}

fn modes(rng: &mut Rng) -> Modes {
    let mut flag = |percent| rng.chance(percent);
    let mut modes = Modes {
        canonical: flag(75),
        echo: flag(80),
        echoe: flag(70),
        echok: flag(50),
        echoke: flag(60),
        echoctl: flag(70),
        echonl: flag(15),
        echoprt: flag(2),
        iexten: flag(70),
        isig: flag(70),
        noflsh: flag(20),
        altwerase: flag(30),
        icrnl: flag(60),
        inlcr: flag(10),
        igncr: flag(10),
        istrip: flag(10),
        iutf8: flag(70),
        ixon: flag(50),
        parmrk: flag(5),
        opost: flag(85),
        onlcr: flag(75),
        ocrnl: flag(10),
        onocr: flag(2),
        onoeot: flag(20),
        expand_tabs: flag(4),
        ..Modes::default()
    };
    modes.erase = char(rng, Some(0x7f));
    modes.kill = char(rng, Some(0x15));
    modes.werase = char(rng, Some(0x17));
    modes.intr = char(rng, Some(0x03));
    modes.quit = char(rng, Some(0x1c));
    modes.susp = char(rng, Some(0x1a));
    modes.eof = char(rng, Some(0x04));
    modes.eol = char(rng, None);
    modes.eol2 = char(rng, None);
    modes.lnext = rare_char(rng, 0x16);
    modes.reprint = rare_char(rng, 0x12);
    modes.start = char(rng, Some(0x11));
    modes.stop = rare_char(rng, 0x13);
    modes.discard = rare_char(rng, 0x0f);
    modes.status = rare_char(rng, 0x14);
    modes
}

/// One key, as its bytes.
fn key(rng: &mut Rng) -> Vec<u8> {
    const PRINTABLE: &[u8] = b"abcxyzQW019_ -.#@";
    const UTF8: &[&str] = &["é", "€", "中", "😀", "ß"];
    const EDITING: &[u8] = b"\x7f\x08\x15\x17";
    const LINE_ENDS: &[u8] = b"\r\n";
    const SIGNALS: &[u8] = b"\x03\x1c\x1a";
    const CONTROLS: &[&[u8]] = &[
        b"\t", b"\x1b", b"\x1b[A", b"\x04", b"\x01", b"\x05", b"\x18", b"\x11", b"\x80", b"\xc3",
        b"\xff",
    ];
    // Reprint, status, discard, literal next and stop.
    const RARE: &[u8] = b"\x12\x14\x0f\x16\x13";
    let one = |byte: Option<&u8>| byte.map(|&b| vec![b]).unwrap_or_default();
    match rng.below(1000) {
        0..550 => one(rng.pick(PRINTABLE)),
        550..650 => rng
            .pick(UTF8)
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default(),
        650..800 => one(rng.pick(EDITING)),
        800..860 => one(rng.pick(LINE_ENDS)),
        860..890 => one(rng.pick(SIGNALS)),
        890..995 => rng.pick(CONTROLS).map(|k| k.to_vec()).unwrap_or_default(),
        _ => one(rng.pick(RARE)),
    }
}

/// Sets `modes` on the slave: the modes it then has, as the kernel kept
/// them.
fn set(slave: &Slave, modes: &Modes) -> Result<Modes, String> {
    let mut termios = terminal::attributes(slave).map_err(|e| e.to_string())?;
    termios.set_modes(modes);
    terminal::set_attributes(slave, &termios).map_err(|e| e.to_string())?;
    Ok(terminal::attributes(slave)
        .map_err(|e| e.to_string())?
        .modes())
}

/// Reads what the master has into `got` until it holds `want` bytes, or
/// `wait` passes.
fn read(master: &Master, got: &mut Vec<u8>, want: usize, wait: Duration) -> Result<(), String> {
    let deadline = Instant::now()
        .checked_add(wait)
        .unwrap_or_else(Instant::now);
    let mut buffer = [0u8; 4096];
    loop {
        loop {
            match fuxix::io::read(master, &mut buffer) {
                Ok(0) | Err(fuxix::Errno::AGAIN) => break,
                Ok(n) => got.extend_from_slice(buffer.get(..n).unwrap_or_default()),
                Err(e) => return Err(e.to_string()),
            }
        }
        let now = Instant::now();
        if got.len() >= want || now >= deadline {
            return Ok(());
        }
        let mut fds = [PollFd::new(master, Events::IN)];
        poll(&mut fds, Some(deadline.saturating_duration_since(now))).map_err(|e| e.to_string())?;
    }
}

/// Reads what was typed on the slave, as a program would before it
/// changes its modes.
fn drain(reader: &OwnedFd) {
    let mut buffer = [0u8; 4096];
    while fuxix::io::read(reader, &mut buffer).is_ok_and(|n| n > 0) {}
}

enum Outcome {
    Agreed,
    Skipped,
    Differed(String),
}

fn run(seed: u64) -> Result<Outcome, String> {
    let mut rng = Rng(seed);
    let size = Size::new(24, 80).ok_or("a size")?;
    let (master, slave) = fuxix::pty::open(size).map_err(|e| e.to_string())?;
    let reader = terminal::reopen(&slave).map_err(|e| e.to_string())?;
    let mut modes = set(&slave, &modes(&mut rng))?;
    let mut echo: Echo = Echo::default();
    // What was set and typed, to show when the two differ.
    let mut log = vec![format!("{modes:?}")];
    let (mut expected, mut got) = (Vec::new(), Vec::new());
    let mut typed = 0usize;
    let mut quiet = false;
    for _ in 0..rng.below(17).saturating_add(4) {
        if rng.chance(10) {
            // Let the kernel take the last write, which echoed nothing to
            // wait for, before the modes change under it.
            if quiet {
                std::thread::sleep(Duration::from_millis(10));
            }
            drain(&reader);
            modes = set(&slave, &self::modes(&mut rng))?;
            log.push(format!("{modes:?}"));
        }
        let mut input = Vec::new();
        let keys = if rng.chance(15) { 40 } else { 4 };
        for _ in 0..rng.below(keys).saturating_add(1) {
            let key = key(&mut rng);
            if input.len().saturating_add(key.len()) > 64 {
                break;
            }
            input.extend(key);
        }
        typed = typed.saturating_add(input.len());
        if typed > 400 {
            break;
        }
        let Some(echoed) = echo.echo(&modes, &input) else {
            return Ok(Outcome::Skipped);
        };
        quiet = echoed.is_empty();
        let model = format!("{}", echoed.escape_ascii());
        expected.extend(echoed);
        let written = fuxix::io::write(&master, &input).map_err(|e| e.to_string())?;
        if written != input.len() {
            return Err(format!("wrote {written} of {} bytes", input.len()));
        }
        let before = got.len();
        read(&master, &mut got, expected.len(), Duration::from_secs(1))?;
        log.push(format!(
            "typed \"{}\"\n    model  \"{model}\"\n    kernel \"{}\"",
            input.escape_ascii(),
            got.get(before..).unwrap_or_default().escape_ascii()
        ));
        if !expected.starts_with(&got) && !got.starts_with(&expected) {
            break;
        }
    }
    // Anything more the kernel echoed.
    read(&master, &mut got, usize::MAX, Duration::from_millis(5))?;
    if got == expected {
        return Ok(Outcome::Agreed);
    }
    Ok(Outcome::Differed(format!(
        "seed {seed:#x}:\n  {}\n  kernel: \"{}\"\n  model:  \"{}\"",
        log.join("\n  "),
        got.escape_ascii(),
        expected.escape_ascii(),
    )))
}

#[test]
fn the_model_echoes_what_the_kernel_does() -> Result<(), String> {
    let (mut skipped, mut differed) = (0u64, Vec::new());
    for case in 0..CASES {
        match run(0x9e37_79b9_7f4a_7c15 ^ case.wrapping_mul(0x2545_f491_4f6c_dd1d))? {
            Outcome::Agreed => {}
            Outcome::Skipped => skipped = skipped.saturating_add(1),
            Outcome::Differed(what) => differed.push(what),
        }
    }
    println!(
        "{CASES} cases, {skipped} skipped, {} differed",
        differed.len()
    );
    if !differed.is_empty() {
        return Err(format!(
            "{} of {CASES} cases differ:\n\n{}",
            differed.len(),
            differed.join("\n\n")
        ));
    }
    let limit = CASES
        .saturating_mul(SKIPPED_PERCENT)
        .checked_div(100)
        .unwrap_or(0);
    if skipped > limit {
        return Err(format!("{skipped} of {CASES} cases skipped, over {limit}"));
    }
    Ok(())
}
