//! The `fux2` command.
use fux2::client::{ANSWER_WAIT, Client};
use fux2::dir::{self, Session};
use fux2::model::{apply, checked_size};
use fux2::pane::{self, DEFAULT_HISTORY, MAX_HISTORY, MAX_INPUT, Spec};
use fux2::wire::{
    Answer, Change, Code, Failure, MAX_HISTORY_ROWS, MAX_PARTS, MAX_REQUEST, MAX_WAIT_MS, Meta,
    PaneId, Part, Request, Row, State, Token, screen_text,
};
use std::ffi::OsString;
use std::io::{BufRead, Read, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
fux2: a terminal multiplexer with no server

  fux2 start --name NAME [--label K=V].. [--cols N] [--rows N] [--history BYTES] [-- PROGRAM ARG..]
  fux2 ls [--label K=V].. [--long]
  fux2 send NAME [TEXT] [--key KEY].. [--paste TEXT] [--stdin] [--if TOKEN]
  fux2 read NAME [--json] [--since TOKEN [--wait MS] [--settle MS] [--until TEXT] [--until-line TEXT] [--text]]
  fux2 read NAME --history EPOCH FIRST COUNT
  fux2 read NAME --history-text
  fux2 page NAME [--close-pane]
  fux2 wait NAME --after TOKEN [--ms MS]
  fux2 resize NAME COLS ROWS [--if TOKEN]
  fux2 signal NAME SIGNAL [--if TOKEN]
  fux2 kill NAME [--if TOKEN]
  fux2 kill --label K=V..
  fux2 view [--layout FILE]

TEXT understands \\n (the Enter key), \\r, \\\\ and \\xHH. See the README for the rest.
";

/// How long `start` waits to hear from its pane. Windows may first fetch
/// its ConPTY.
const START_WAIT: Duration = Duration::from_secs(if cfg!(windows) { 120 } else { 10 });
/// How long output may rest before `read --wait` calls it settled.
const DEFAULT_SETTLE_MS: u32 = 100;
/// How long `read --since` waits when no `--wait` is given.
const NO_WAIT: u32 = 0;
/// How often a history read that lost its place starts again.
const HISTORY_TRIES: u32 = 3;

type Outcome = Result<(), Failure>;

fn usage(why: impl Into<String>) -> Failure {
    Failure::new(Code::Invalid, why)
}

fn say(text: &str) -> Outcome {
    let mut out = std::io::stdout();
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            Err(Failure::new(Code::Io, format!("stdout: {e}")))
        })
}

/// Arguments, read one at a time.
struct Args(std::vec::IntoIter<OsString>);

impl Args {
    fn next_os(&mut self) -> Option<OsString> {
        self.0.next()
    }

    fn next(&mut self) -> Result<Option<String>, Failure> {
        self.next_os()
            .map(|arg| {
                arg.into_string()
                    .map_err(|arg| usage(format!("not UTF-8: {}", arg.to_string_lossy())))
            })
            .transpose()
    }

    fn value(&mut self, flag: &str) -> Result<String, Failure> {
        self.next()?
            .ok_or_else(|| usage(format!("{flag} needs a value")))
    }

    fn number<T: std::str::FromStr>(&mut self, flag: &str) -> Result<T, Failure> {
        let text = self.value(flag)?;
        text.parse()
            .map_err(|_| usage(format!("{flag}: not a number: {text}")))
    }

    fn token(&mut self, flag: &str) -> Result<Token, Failure> {
        self.value(flag)?.parse()
    }

    /// The pane an argument names: `NAME`, or `NAME@ID` for that one life
    /// of it and no later pane of the name.
    fn pane(&mut self) -> Result<Target, Failure> {
        let text = self.next()?.ok_or_else(|| usage("a pane name is needed"))?;
        let (name, id) = match text.split_once('@') {
            None => (text.as_str(), None),
            Some((name, id)) => {
                let id =
                    PaneId::parse(id).ok_or_else(|| usage(format!("bad pane id in {text}")))?;
                (name, Some(id))
            }
        };
        dir::check_name(name)?;
        Ok(Target {
            name: name.to_owned(),
            id,
        })
    }
}

/// A pane as a command names it.
struct Target {
    name: String,
    id: Option<PaneId>,
}

impl Target {
    fn connect(&self) -> Result<Client, Failure> {
        let client = Client::connect(&Session::open()?, &self.name)?;
        match self.id {
            Some(id) if id != client.hello.id => {
                let why = format!("{} is now pane {}, not {id}", self.name, client.hello.id);
                Err(Failure::new(Code::Replaced, why))
            }
            Some(_) | None => Ok(client),
        }
    }
}

fn unknown(arg: &str) -> Failure {
    usage(format!("unexpected argument: {arg}"))
}

fn odd() -> Failure {
    Failure::new(Code::Protocol, "the pane answered something else")
}

fn default_program() -> OsString {
    let shell = if cfg!(windows) { "COMSPEC" } else { "SHELL" };
    let fallback = if cfg!(windows) { "cmd.exe" } else { "sh" };
    std::env::var_os(shell)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.into())
}

fn start(mut args: Args) -> Outcome {
    let (mut name, mut labels, mut cols, mut rows) = (None, Vec::new(), 80u16, 24u16);
    let mut history = DEFAULT_HISTORY;
    let mut argv = Vec::new();
    while let Some(arg) = args.next()? {
        match arg.as_str() {
            "--name" => name = Some(args.value("--name")?),
            "--label" => labels.push(dir::parse_label(&args.value("--label")?)?),
            "--cols" => cols = args.number("--cols")?,
            "--rows" => rows = args.number("--rows")?,
            "--history" => history = args.number("--history")?,
            "--" => {
                argv.extend(args.0.by_ref());
                break;
            }
            other => return Err(unknown(other)),
        }
    }
    let name = name.ok_or_else(|| usage("start needs --name"))?;
    dir::check_name(&name)?;
    dir::check_labels(&labels)?;
    checked_size(cols, rows)?;
    if history > MAX_HISTORY {
        let why = format!("--history {history}: at most {MAX_HISTORY}");
        return Err(Failure::new(Code::Limit, why));
    }
    if argv.is_empty() {
        argv.push(default_program());
    }
    let session = Session::open()?;
    let cwd = std::env::current_dir().map_err(|e| Failure::new(Code::Io, format!("cwd: {e}")))?;
    let spec = Spec {
        root: session.root.clone(),
        name: name.clone(),
        cols,
        rows,
        history,
        cwd,
        labels,
        argv,
    };
    // Under the session's lock, so that two first starts on Windows do not
    // clean away each other's copy of this program.
    let lock = session.lock()?;
    let launch = fux2::sys::launch(&session.root, &spec.to_args());
    drop(lock);
    let launch = launch?;
    let mut go = launch.go;
    // Said before the program starts, and kept with the pane's screen.
    let word: &[u8] = match launch.bound {
        Some(why) => {
            let _ = writeln!(std::io::stderr(), "fux2: {name}: {why}");
            b"go bound\n"
        }
        None => b"go\n",
    };
    let told = go.write_all(word).and_then(|()| go.flush());
    // The line comes on a thread of its own, so that a daemon that says
    // nothing cannot hold this command past its bound.
    let (sender, receiver) = std::sync::mpsc::channel();
    let ready = launch.ready;
    let reader = std::thread::Builder::new().spawn(move || {
        let mut line = String::new();
        let got = std::io::BufReader::new(ready.take(64 << 10)).read_line(&mut line);
        let _ = sender.send(got.map(|_| line));
    });
    let unknown_fate = |why: String| {
        let text = format!("pane {name} may or may not have started: {why}");
        Failure::new(Code::Indeterminate, text)
    };
    told.map_err(|e| unknown_fate(format!("telling it to go: {e}")))?;
    reader.map_err(|e| unknown_fate(format!("a thread: {e}")))?;
    let line = receiver
        .recv_timeout(START_WAIT)
        .map_err(|_| unknown_fate(format!("no word within {START_WAIT:?}")))?
        .map_err(|e| unknown_fate(format!("reading its word: {e}")))?;
    let line = line.trim_end();
    if let Some(token) = line.strip_prefix("ok ") {
        return say(&format!("{name} {token}\n"));
    }
    let Some((code, message)) = line.strip_prefix("err ").and_then(|l| l.split_once(' ')) else {
        return Err(unknown_fate(format!("it said {line:?}")));
    };
    let code = Code::from_name(code).unwrap_or(Code::Io);
    Err(Failure::new(code, message))
}

fn ls(mut args: Args) -> Outcome {
    let (mut tests, mut long) = (Vec::new(), false);
    while let Some(arg) = args.next()? {
        match arg.as_str() {
            "--label" => tests.push(dir::parse_label(&args.value("--label")?)?),
            "--long" => long = true,
            other => return Err(unknown(other)),
        }
    }
    let session = Session::open()?;
    let mut out = String::new();
    for name in session.names()? {
        let hello = match Client::connect(&session, &name) {
            Ok(client) => client.hello,
            // Gone meanwhile, or leftovers of a pane that died.
            Err(failure) if failure.code == Code::Missing => continue,
            Err(failure) => {
                if tests.is_empty() {
                    out.push_str(&format!("{name}\tunavailable ({failure})\n"));
                }
                continue;
            }
        };
        if !dir::matches(&name, &hello.labels, &tests) {
            continue;
        }
        out.push_str(&name);
        if long {
            let labels: Vec<String> = (hello.labels.iter())
                .filter(|(key, _)| key != "name")
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            out.push_str(&format!(
                "\t{}\t{}\t{}x{}\t{}\t{}",
                hello.id,
                hello.pid,
                hello.cols,
                hello.rows,
                hello.state,
                labels.join(",")
            ));
        }
        out.push('\n');
    }
    say(&out)
}

/// CLI text as what it types: `\n` is the Enter key, as a terminal sends
/// it, so that a line is submitted on every system.
fn text_parts(text: &str) -> Result<Vec<Part>, Failure> {
    let mut parts = Vec::new();
    let mut literal: Vec<u8> = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            literal.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => {
                if !literal.is_empty() {
                    parts.push(Part::Bytes(std::mem::take(&mut literal)));
                }
                parts.push(Part::Key("Enter".to_owned()));
            }
            Some('r') => literal.push(b'\r'),
            Some('\\') => literal.push(b'\\'),
            Some('x') => {
                let digit = |c: Option<char>| c.and_then(|c| c.to_digit(16));
                let (high, low) = (digit(chars.next()), digit(chars.next()));
                let (Some(high), Some(low)) = (high, low) else {
                    return Err(usage("\\x needs two hex digits"));
                };
                let byte = high.saturating_mul(16).saturating_add(low);
                literal.push(u8::try_from(byte).unwrap_or(0));
            }
            other => {
                let shown = other.map_or("the end".to_owned(), |c| format!("{c:?}"));
                return Err(usage(format!(
                    "after \\ comes n, r, \\ or xHH, not {shown}"
                )));
            }
        }
    }
    if !literal.is_empty() {
        parts.push(Part::Bytes(literal));
    }
    Ok(parts)
}

fn send(mut args: Args) -> Outcome {
    let pane = args.pane()?;
    let (mut parts, mut expected) = (Vec::new(), None);
    while let Some(arg) = args.next()? {
        match arg.as_str() {
            "--key" => parts.push(Part::Key(args.value("--key")?)),
            "--paste" => parts.push(Part::Paste(args.value("--paste")?)),
            "--if" => expected = Some(args.token("--if")?),
            "--stdin" => {
                let most = u64::try_from(MAX_INPUT).unwrap_or(u64::MAX);
                let mut bytes = Vec::new();
                std::io::stdin()
                    .take(most.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|e| Failure::new(Code::Io, format!("stdin: {e}")))?;
                if bytes.len() > MAX_INPUT {
                    let why = format!("more than {MAX_INPUT} bytes on stdin");
                    return Err(Failure::new(Code::Limit, why));
                }
                parts.push(Part::Bytes(bytes));
            }
            text if text.starts_with("--") => return Err(unknown(text)),
            text => parts.extend(text_parts(text)?),
        }
    }
    if parts.is_empty() {
        return Err(usage("send needs TEXT, --key, --paste or --stdin"));
    }
    if parts.len() > MAX_PARTS {
        return Err(usage(format!("{} parts, at most {MAX_PARTS}", parts.len())));
    }
    let request = Request::Send { parts, expected };
    if request.encode().len() > MAX_REQUEST {
        let why = format!("a send is at most {MAX_REQUEST} bytes");
        return Err(Failure::new(Code::Limit, why));
    }
    let mut client = pane.connect()?;
    let Answer::Sent { token, .. } = client.ask(&request, ANSWER_WAIT)? else {
        return Err(odd());
    };
    say(&format!("{token}\n"))
}

fn json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_rows<'a>(rows: impl Iterator<Item = (u64, &'a Row)>, out: &mut String) {
    out.push('[');
    for (index, (number, row)) in rows.enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!("{{\"row\":{number},\"text\":"));
        json_string(&row.text(), out);
        out.push_str(",\"rendition\":");
        out.push_str(&row.rendition.to_string());
        out.push_str(",\"spans\":[");
        for (index, span) in row.spans.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("{\"sgr\":");
            json_string(&span.sgr, out);
            out.push_str(",\"cells\":[");
            for (index, cell) in span.cells.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                match cell {
                    fux2::wire::Cell::Text(text) => json_string(text, out),
                    fux2::wire::Cell::Tail => out.push_str("null"),
                }
            }
            out.push_str("]}");
        }
        out.push_str("]}");
    }
    out.push(']');
}

fn json_head(token: Token, meta: &Meta, out: &mut String) {
    out.push_str("{\"token\":");
    json_string(&token.to_string(), out);
    out.push_str(&format!(
        ",\"cols\":{},\"rows\":{},\"cursor\":{{\"row\":{},\"col\":{},\"visible\":{},\"shape\":{}}},\
         \"alternate\":{},\"history\":{{\"start\":{},\"end\":{},\"epoch\":{}}},\"state\":",
        meta.cols,
        meta.rows,
        meta.cursor_row,
        meta.cursor_col,
        meta.cursor_visible,
        meta.cursor_shape,
        meta.alternate,
        meta.history_start,
        meta.history_end,
        meta.epoch,
    ));
    json_string(&meta.state.to_string(), out);
    out.push_str(",\"limitations\":[");
    for (index, limitation) in meta.limitations.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        json_string(limitation, out);
    }
    out.push(']');
}

fn screen(client: &mut Client, since: Option<Token>) -> Result<(Token, Meta, Change), Failure> {
    let Answer::Screen {
        token,
        meta,
        change,
    } = client.ask(&Request::Screen { since }, ANSWER_WAIT)?
    else {
        return Err(odd());
    };
    Ok((token, *meta, change))
}

fn whole_rows(client: &mut Client) -> Result<(Token, Meta, Vec<Row>), Failure> {
    let (token, meta, change) = screen(client, None)?;
    let mut rows = Vec::new();
    apply(&mut rows, meta.rows, change);
    Ok((token, meta, rows))
}

/// What `read --since` waits for and how it answers.
#[derive(Default)]
struct Follow {
    since: Option<Token>,
    wait_ms: Option<u32>,
    settle_ms: Option<u32>,
    until: Option<String>,
    until_line: Option<String>,
    text: bool,
}

/// One wait: the token reached, or `None` if none came within `ms`.
fn wait_once(client: &mut Client, after: Token, ms: u32) -> Result<Option<Token>, Failure> {
    let ms = ms.min(MAX_WAIT_MS);
    let bound = Duration::from_millis(ms.into()).saturating_add(ANSWER_WAIT);
    match client.ask(&Request::Wait { after, ms }, bound) {
        Ok(Answer::Waited { token, .. }) => Ok(Some(token)),
        Err(failure) if failure.code == Code::Timeout => Ok(None),
        Err(failure) => Err(failure),
        Ok(_) => Err(odd()),
    }
}

fn millis_left(until: Instant) -> u32 {
    let left = until.saturating_duration_since(Instant::now()).as_millis();
    u32::try_from(left).unwrap_or(u32::MAX)
}

/// Waits for a change after `from`, then for the output to rest; `None` if
/// no change came by `until`.
fn next_change(
    client: &mut Client,
    from: Token,
    until: Instant,
    settle: u32,
) -> Result<Option<Token>, Failure> {
    let mut seen = from;
    loop {
        let left = millis_left(until);
        match wait_once(client, seen, left)? {
            Some(token) => {
                seen = token;
                break;
            }
            None if left <= MAX_WAIT_MS => return Ok(None),
            None => {}
        }
    }
    if settle == 0 {
        return Ok(Some(seen));
    }
    loop {
        let left = millis_left(until);
        if left == 0 {
            break;
        }
        let before = seen;
        match wait_once(client, seen, settle.min(left))? {
            // An ended pane answers at once, and will not change again.
            Some(token) if token == before => break,
            Some(token) => seen = token,
            None => break,
        }
    }
    Ok(Some(seen))
}

fn follow(client: &mut Client, since: Token, plan: &Follow) -> Outcome {
    // A token of another life of the name, or of the future, is refused
    // here, whatever else was asked.
    screen(client, Some(since))?;
    let bound = Duration::from_millis(plan.wait_ms.unwrap_or(NO_WAIT).into());
    let until = Instant::now()
        .checked_add(bound)
        .unwrap_or_else(Instant::now);
    let settle = plan.settle_ms.unwrap_or(DEFAULT_SETTLE_MS);
    let mut seen = since;
    let mut matched_rows = None;
    if plan.wait_ms.is_some() {
        loop {
            let Some(token) = next_change(client, seen, until, settle)? else {
                let why = format!("no change after {seen} within {bound:?}");
                return Err(Failure::new(Code::Timeout, why));
            };
            let (now, meta, rows) = whole_rows(client)?;
            let matched = rows.iter().map(Row::text).any(|line| {
                plan.until_line.as_ref().is_none_or(|want| *want == line)
                    && plan.until.as_ref().is_none_or(|want| line.contains(want))
            });
            if matched {
                matched_rows = Some(rows);
                break;
            }
            if plan.until.is_none() && plan.until_line.is_none() {
                break;
            }
            if meta.state != State::Running {
                let why = format!("the text never came, and the pane {}", meta.state);
                return Err(Failure::new(Code::Ended, why));
            }
            seen = if token.version > now.version {
                token
            } else {
                now
            };
            if Instant::now() >= until {
                let why = format!("the text did not come within {bound:?}");
                return Err(Failure::new(Code::Timeout, why));
            }
        }
    }
    if plan.text {
        // The screen that matched, not a later one.
        let rows = match matched_rows {
            Some(rows) => rows,
            None => whole_rows(client)?.2,
        };
        return say(&screen_text(&rows));
    }
    let (token, meta, change) = screen(client, Some(since))?;
    let mut out = String::new();
    json_head(token, &meta, &mut out);
    match &change {
        Change::Unchanged => out.push_str(",\"change\":\"unchanged\""),
        Change::Whole(rows) => {
            out.push_str(",\"change\":\"whole\",\"changed\":");
            json_rows((0u64..).zip(rows), &mut out);
        }
        Change::Patch { moved, rows } => {
            out.push_str(&format!(
                ",\"change\":\"patch\",\"moved\":{moved},\"changed\":"
            ));
            json_rows(rows.iter().map(|(y, row)| (u64::from(*y), row)), &mut out);
        }
    }
    out.push_str("}\n");
    say(&out)
}

/// One pass over a pane's primary screen, history first, by row number.
/// History rows keep their numbers; the screen comes in one answer.
fn collect_history(client: &mut Client) -> Result<String, Failure> {
    let (_, meta, _) = screen(client, None)?;
    let mut read = fux2::history::Gathered::new(meta.history_start);
    loop {
        let request = Request::History {
            epoch: meta.epoch,
            first: read.next(),
            count: MAX_HISTORY_ROWS,
        };
        let Answer::History {
            end,
            screen_end,
            rows,
            ..
        } = client.ask(&request, ANSWER_WAIT)?
        else {
            return Err(odd());
        };
        if read.take(end, screen_end, &rows)? {
            break;
        }
    }
    let mut text = read.finish();
    if meta.alternate {
        let (_, _, rows) = whole_rows(client)?;
        text.push_str(&screen_text(&rows));
    }
    Ok(text)
}

/// The pane's history and screen as text. A resize or eviction under the
/// read starts it again, a few times, and then is said.
fn history_text_of(client: &mut Client) -> Result<String, Failure> {
    let mut tries = 0;
    loop {
        match collect_history(client) {
            Err(failure)
                if matches!(failure.code, Code::Stale | Code::Gone) && tries < HISTORY_TRIES =>
            {
                tries = tries.saturating_add(1);
            }
            done => return done,
        }
    }
}

/// Shows a pane's history in a pager. With `--close-pane`, run as the
/// program of a pane made for it, it then removes that pane.
fn page(mut args: Args) -> Outcome {
    let pane = args.pane()?;
    let mut close_pane = false;
    while let Some(arg) = args.next()? {
        match arg.as_str() {
            "--close-pane" => close_pane = true,
            other => return Err(unknown(other)),
        }
    }
    let text = history_text_of(&mut pane.connect()?)?;
    let chosen = std::env::var("PAGER").ok().filter(|p| !p.trim().is_empty());
    let fallbacks: &[&str] = if cfg!(windows) {
        &["more.com"]
    } else {
        &["less", "more"]
    };
    let pagers: Vec<String> = match chosen {
        Some(pager) => vec![pager],
        None => fallbacks.iter().map(|p| (*p).to_owned()).collect(),
    };
    let mut child = None;
    let mut failure = String::new();
    for pager in &pagers {
        let mut words = pager.split_whitespace();
        let Some(program) = words.next() else {
            continue;
        };
        let started = std::process::Command::new(program)
            .args(words)
            .stdin(std::process::Stdio::piped())
            .spawn();
        match started {
            Ok(started) => {
                child = Some(started);
                break;
            }
            Err(e) => failure = format!("{program}: {e}"),
        }
    }
    let mut child = child.ok_or_else(|| Failure::new(Code::Io, failure))?;
    // A pager that quits early closes this: not a failure.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child
        .wait()
        .map_err(|e| Failure::new(Code::Io, format!("the pager: {e}")))?;
    // Only a pane started to show this one asks to go; any other caller's
    // pane is left alone.
    if close_pane
        && let Ok(own) = std::env::var("FUX2_PANE")
        && let Ok(mut client) = Client::connect(&Session::open()?, &own)
    {
        // The answer may never come: this process is what it ends.
        let _ = client.call(&Request::Kill { expected: None }, ANSWER_WAIT);
    }
    Ok(())
}

fn read(mut args: Args) -> Outcome {
    let pane = args.pane()?;
    let (mut plan, mut json, mut history) = (Follow::default(), false, None);
    let mut history_text = false;
    while let Some(arg) = args.next()? {
        match arg.as_str() {
            "--json" => json = true,
            "--text" => plan.text = true,
            "--history-text" => history_text = true,
            "--since" => plan.since = Some(args.token("--since")?),
            "--wait" => plan.wait_ms = Some(args.number("--wait")?),
            "--settle" => plan.settle_ms = Some(args.number("--settle")?),
            "--until" => plan.until = Some(args.value("--until")?),
            "--until-line" => plan.until_line = Some(args.value("--until-line")?),
            "--history" => {
                let epoch: u64 = args.number("--history EPOCH")?;
                let first: u64 = args.number("--history FIRST")?;
                let count: u64 = args.number("--history COUNT")?;
                history = Some((epoch, first, count));
            }
            other => return Err(unknown(other)),
        }
    }
    let following = plan.wait_ms.is_some()
        || plan.settle_ms.is_some()
        || plan.until.is_some()
        || plan.until_line.is_some()
        || plan.text;
    if following && plan.since.is_none() {
        return Err(usage(
            "--wait, --settle, --until and --text go with --since",
        ));
    }
    if (plan.until.is_some() || plan.until_line.is_some()) && plan.wait_ms.is_none() {
        return Err(usage("--until and --until-line need --wait"));
    }
    let mut client = pane.connect()?;
    if let Some((epoch, first, count)) = history {
        let request = Request::History {
            epoch,
            first,
            count,
        };
        let Answer::History {
            epoch,
            start,
            end,
            screen_end,
            rows,
        } = client.ask(&request, ANSWER_WAIT)?
        else {
            return Err(odd());
        };
        let mut out = format!(
            "{{\"epoch\":{epoch},\"start\":{start},\"end\":{end},\"screen_end\":{screen_end},\"rows\":"
        );
        json_rows(rows.iter().map(|r| (r.number, &r.row)), &mut out);
        out.push_str("}\n");
        return say(&out);
    }
    if history_text {
        return say(&history_text_of(&mut client)?);
    }
    if let Some(since) = plan.since {
        return follow(&mut client, since, &plan);
    }
    let (token, meta, rows) = whole_rows(&mut client)?;
    if !json {
        return say(&screen_text(&rows));
    }
    let mut out = String::new();
    json_head(token, &meta, &mut out);
    out.push_str(",\"screen\":");
    json_rows((0u64..).zip(&rows), &mut out);
    out.push_str("}\n");
    say(&out)
}

fn wait(mut args: Args) -> Outcome {
    let pane = args.pane()?;
    let (mut after, mut ms) = (None, MAX_WAIT_MS);
    while let Some(arg) = args.next()? {
        match arg.as_str() {
            "--after" => after = Some(args.token("--after")?),
            "--ms" => ms = args.number("--ms")?,
            other => return Err(unknown(other)),
        }
    }
    let after = after.ok_or_else(|| usage("wait needs --after TOKEN"))?;
    let mut client = pane.connect()?;
    let bound = Duration::from_millis(ms.into()).saturating_add(ANSWER_WAIT);
    let Answer::Waited { token, state } = client.ask(&Request::Wait { after, ms }, bound)? else {
        return Err(odd());
    };
    say(&format!("{token} {state}\n"))
}

fn expected_only(args: &mut Args) -> Result<Option<Token>, Failure> {
    let mut expected = None;
    while let Some(arg) = args.next()? {
        match arg.as_str() {
            "--if" => expected = Some(args.token("--if")?),
            other => return Err(unknown(other)),
        }
    }
    Ok(expected)
}

fn resize(mut args: Args) -> Outcome {
    let pane = args.pane()?;
    let cols = args.number("COLS")?;
    let rows = args.number("ROWS")?;
    let expected = expected_only(&mut args)?;
    let mut client = pane.connect()?;
    let request = Request::Resize {
        cols,
        rows,
        expected,
    };
    let Answer::Resized { cols, rows, .. } = client.ask(&request, ANSWER_WAIT)? else {
        return Err(odd());
    };
    say(&format!("{cols} {rows}\n"))
}

fn signal(mut args: Args) -> Outcome {
    let pane = args.pane()?;
    let signal = args.value("SIGNAL")?;
    let expected = expected_only(&mut args)?;
    let mut client = pane.connect()?;
    let request = Request::Signal {
        name: signal,
        expected,
    };
    let Answer::Signalled { delivery } = client.ask(&request, ANSWER_WAIT)? else {
        return Err(odd());
    };
    say(&format!("{delivery}\n"))
}

fn kill_one(pane: &Target, expected: Option<Token>) -> Outcome {
    let mut client = pane.connect()?;
    let Answer::Killed { complete, detail } =
        client.ask(&Request::Kill { expected }, ANSWER_WAIT)?
    else {
        return Err(odd());
    };
    if complete {
        return Ok(());
    }
    let why = format!("pane {} is gone, but: {detail}", pane.name);
    Err(Failure::new(Code::Io, why))
}

/// Kills every pane whose labels pass all the tests.
fn kill_labelled(mut args: Args, first: String) -> Outcome {
    let mut tests = vec![dir::parse_label(&first)?];
    while let Some(arg) = args.next()? {
        match arg.as_str() {
            "--label" => tests.push(dir::parse_label(&args.value("--label")?)?),
            other => return Err(unknown(other)),
        }
    }
    let session = Session::open()?;
    let mut failed = Vec::new();
    for name in session.names()? {
        let Ok(client) = Client::connect(&session, &name) else {
            continue;
        };
        if !dir::matches(&name, &client.hello.labels, &tests) {
            continue;
        }
        let pane = Target {
            name,
            id: Some(client.hello.id),
        };
        drop(client);
        if let Err(failure) = kill_one(&pane, None) {
            failed.push(failure.to_string());
        }
    }
    match failed.is_empty() {
        true => Ok(()),
        false => Err(Failure::new(Code::Io, failed.join("; "))),
    }
}

fn kill(mut args: Args) -> Outcome {
    let rest: Vec<OsString> = args.0.by_ref().collect();
    let mut args = Args(rest.clone().into_iter());
    if args.next()?.as_deref() == Some("--label") {
        let first = args.value("--label")?;
        return kill_labelled(args, first);
    }
    let mut args = Args(rest.into_iter());
    let pane = args.pane()?;
    let expected = expected_only(&mut args)?;
    kill_one(&pane, expected)
}

/// Runs the viewer installed beside this program.
fn view(args: Args) -> Outcome {
    let exe = std::env::current_exe().map_err(|e| Failure::new(Code::Io, format!("{e}")))?;
    let viewer = exe.with_file_name(format!("fux2-view{}", std::env::consts::EXE_SUFFIX));
    if !viewer.exists() {
        let why = format!("the viewer is not installed: no {}", viewer.display());
        return Err(Failure::new(Code::Missing, why));
    }
    let mut command = std::process::Command::new(&viewer);
    command.args(args.0);
    let failed = |e: std::io::Error| Failure::new(Code::Io, format!("{}: {e}", viewer.display()));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(failed(command.exec()))
    }
    #[cfg(windows)]
    {
        let status = command.status().map_err(failed)?;
        match status.success() {
            true => Ok(()),
            false => Err(Failure::new(
                Code::Io,
                format!("the viewer ended: {status}"),
            )),
        }
    }
}

fn run(mut args: Args) -> Outcome {
    let command = args
        .next()?
        .ok_or_else(|| usage(format!("no command\n\n{USAGE}")))?;
    match command.as_str() {
        "start" => start(args),
        "ls" => ls(args),
        "send" => send(args),
        "read" => read(args),
        "wait" => wait(args),
        "resize" => resize(args),
        "signal" => signal(args),
        "kill" => kill(args),
        "view" => view(args),
        "page" => page(args),
        "help" | "--help" | "-h" => say(USAGE),
        other => Err(usage(format!("no command {other}"))),
    }
}

fn main() -> ExitCode {
    let mut argv = std::env::args_os().skip(1);
    let first = argv.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("__pane")) {
        return pane::main(argv);
    }
    let args = Args(
        first
            .into_iter()
            .chain(argv)
            .collect::<Vec<_>>()
            .into_iter(),
    );
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let mut err = std::io::stderr();
            let _ = writeln!(err, "fux2: {failure}");
            ExitCode::from(failure.code.exit())
        }
    }
}
