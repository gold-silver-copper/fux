//! The `fux2` command.
use fux2::client::{ANSWER_WAIT, Client};
use fux2::dir::{self, Session};
use fux2::layout::{self, Amount, Axis, Dir, Live, Op, PaneRef, Rect, Target};
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
  fux2 layout show
  fux2 layout split PANE NEW [--rows] [--before]      fux2 layout remove PANE
  fux2 layout replace OLD NEW                          fux2 layout swap PANE OTHER
  fux2 layout move PANE left|right|up|down|earlier|later
  fux2 layout move PANE --tab TAB|new                  fux2 layout move PANE --workspace WS|new
  fux2 layout border PANE left|right|up|down N%|CELLS  fux2 layout zoom PANE
  fux2 layout title PANE TEXT
  fux2 layout tab new [--after TAB | --workspace WS] [--title T] [--match K=V]..
  fux2 layout tab rename|close|move|match TAB ..       fux2 layout workspace new|rename|close|move ..
      tab close and workspace close take --end-panes: the panes they held are ended
  fux2 layout reset
      every layout edit takes [--if VERSION] [--area COLSxROWS]
  fux2 view

TEXT understands \\n (the Enter key), \\r, \\\\ and \\xHH. See the README for the rest.
";

/// How long `start` waits to hear from its pane.
const START_WAIT: Duration = Duration::from_secs(10);
/// How long output may rest before `read --wait` calls it settled.
const DEFAULT_SETTLE_MS: u32 = 100;
/// How long `read --since` waits when no `--wait` is given.
const NO_WAIT: u32 = 0;
/// How often a matching read waits for two reads to see one screen.
const PAIR_TRIES: u32 = 3;
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
        Target::parse(&text)
    }
}

/// Connects to the pane `target` names: refused if it names one life of a
/// pane and the name has passed to another.
fn connect(target: &Target) -> Result<Client, Failure> {
    let client = Client::connect(&Session::open()?, &target.name)?;
    match target.id {
        Some(id) if id != client.hello.id => {
            let why = format!("{} is now pane {}, not {id}", target.name, client.hello.id);
            Err(Failure::new(Code::Replaced, why))
        }
        Some(_) | None => Ok(client),
    }
}

fn unknown(arg: &str) -> Failure {
    usage(format!("unexpected argument: {arg}"))
}

fn odd() -> Failure {
    Failure::new(Code::Protocol, "the pane answered something else")
}

fn default_program() -> OsString {
    std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "sh".into())
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
    let launch = fux2::sys::launch(&session.root, &spec.to_args())?;
    let mut go = launch.go;
    let told = go.write_all(b"go\n").and_then(|()| go.flush());
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
    let mut client = connect(&pane)?;
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
    // What changed is asked of the last screen this wait looked at, so the
    // pane still remembers it; a screen it has forgotten is no evidence.
    let mut base = since;
    let mut matched_rows = None;
    if plan.wait_ms.is_some() {
        loop {
            let Some(token) = next_change(client, seen, until, settle)? else {
                let why = format!("no change after {seen} within {bound:?}");
                return Err(Failure::new(Code::Timeout, why));
            };
            // Only rows that changed since the token count: a line already on
            // the screen, from an earlier command, is no answer to this one.
            // Output between the two reads of a pair is no match yet.
            let Some((now, meta, rows, changed)) = rows_and_changes(client, base)? else {
                seen = token;
                if Instant::now() >= until {
                    let why = format!("the text did not come within {bound:?}");
                    return Err(Failure::new(Code::Timeout, why));
                }
                continue;
            };
            base = now;
            let matched = changed.iter().map(Row::text).any(|line| {
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

/// The screen, and the rows of it that changed since `since`, from one
/// observation: tried again until no output came between the two reads.
type Changes = Option<(Token, Meta, Vec<Row>, Vec<Row>)>;

fn rows_and_changes(client: &mut Client, since: Token) -> Result<Changes, Failure> {
    for _ in 0..PAIR_TRIES {
        let (whole, meta, rows) = whole_rows(client)?;
        let (token, _, change) = screen(client, Some(since))?;
        if token != whole {
            continue;
        }
        let changed = match change {
            // Forgotten, or a resize: nothing can be told apart as new.
            Change::Unchanged | Change::Whole(_) => Vec::new(),
            Change::Patch { rows, .. } => rows.into_iter().map(|(_, row)| row).collect(),
        };
        return Ok(Some((whole, meta, rows, changed)));
    }
    Ok(None)
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
    let shown = show_pager(&pane);
    if !close_pane {
        return shown;
    }
    // Run in a pane made for it: a failure is said there, and stays until
    // read, before the pane goes.
    if let Err(failure) = &shown {
        let mut err = std::io::stderr();
        let _ = writeln!(err, "fux2: {failure}\n\npress Enter to close");
        let _ = std::io::stdin().take(1024).read(&mut [0; 1024]);
    }
    // This one life of the pane, never a later one of the same name.
    let own = std::env::var("FUX2_PANE").ok();
    let id = std::env::var("FUX2_PANE_ID")
        .ok()
        .and_then(|id| PaneId::parse(&id));
    let own = own.zip(id).map(|(name, id)| Target { name, id: Some(id) });
    // Its tile goes with it: it was made for this and nothing else.
    if let Some(own) = &own
        && let Ok(session) = Session::open()
        && let Ok(live) = live_panes(&session)
    {
        let _ = layout::edit(
            &session,
            None,
            &live,
            layout::NOMINAL,
            &Op::Remove(own.clone()),
        );
    }
    if let Some(own) = &own
        && let Ok(mut client) = connect(own)
    {
        // The answer may never come: this process is what it ends.
        let _ = client.call(&Request::Kill { expected: None }, ANSWER_WAIT);
    }
    shown
}

/// The pane's history in `$PAGER`, else a pager of the system's.
fn show_pager(pane: &Target) -> Outcome {
    let text = history_text_of(&mut connect(pane)?)?;
    let chosen = std::env::var("PAGER").ok().filter(|p| !p.trim().is_empty());
    let fallbacks: &[&str] = &["less", "more"];
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
    let status = child
        .wait()
        .map_err(|e| Failure::new(Code::Io, format!("the pager: {e}")))?;
    match status.success() {
        true => Ok(()),
        false => Err(Failure::new(Code::Io, format!("the pager ended: {status}"))),
    }
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
            "--wait, --settle, --until, --until-line and --text go with --since",
        ));
    }
    if (plan.until.is_some() || plan.until_line.is_some()) && plan.wait_ms.is_none() {
        return Err(usage("--until and --until-line need --wait"));
    }
    let mut client = connect(&pane)?;
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
    let mut client = connect(&pane)?;
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
    let mut client = connect(&pane)?;
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
    let mut client = connect(&pane)?;
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
    let mut client = connect(pane)?;
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

/// How long the layout waits, in all, for panes to say who they are.
const LIVE_WAIT: Duration = Duration::from_secs(2);

/// Every pane that answers within [`LIVE_WAIT`], with its labels, as the
/// layout routes them. They are asked at once, each on its own thread, so
/// one that stalls holds up nobody: it is left out, as one that is gone.
fn live_panes(session: &Session) -> Result<Vec<Live>, Failure> {
    let names = session.names()?;
    let (sender, answers) = std::sync::mpsc::channel();
    let mut asked = 0usize;
    for name in names {
        let (session, sender) = (session.clone(), sender.clone());
        let spawned = std::thread::Builder::new().spawn(move || {
            let live = Client::connect(&session, &name).ok().map(|client| Live {
                pane: PaneRef {
                    name,
                    id: client.hello.id,
                },
                labels: client.hello.labels,
            });
            let _ = sender.send(live);
        });
        if spawned.is_ok() {
            asked = asked.saturating_add(1);
        }
    }
    let now = Instant::now();
    let until = now.checked_add(LIVE_WAIT).unwrap_or(now);
    let mut live = Vec::new();
    for _ in 0..asked {
        let left = until.saturating_duration_since(Instant::now());
        match answers.recv_timeout(left) {
            Ok(answer) => live.extend(answer),
            Err(_) => break,
        }
    }
    live.sort_by(|a: &Live, b: &Live| a.pane.name.cmp(&b.pane.name));
    Ok(live)
}

/// A layout command's words after its name: what is left in order, and
/// the options it takes, each `(flag, takes a value)`. A word that is no
/// option of this command stays a word, so a title may start with `--`.
struct Words {
    rest: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Words {
    fn read(words: Vec<String>, options: &[(&str, bool)]) -> Result<Words, Failure> {
        let mut rest = Vec::new();
        let mut flags = Vec::new();
        let mut words = words.into_iter();
        while let Some(word) = words.next() {
            match options.iter().find(|(flag, _)| *flag == word) {
                Some((flag, true)) => {
                    let value = words
                        .next()
                        .ok_or_else(|| usage(format!("{flag} needs a value")))?;
                    flags.push((word, Some(value)));
                }
                Some((_, false)) => flags.push((word, None)),
                None => rest.push(word),
            }
        }
        Ok(Words { rest, flags })
    }

    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == flag)
    }

    /// The value of `flag`, given at most once.
    fn value(&self, flag: &str) -> Result<Option<String>, Failure> {
        let mut values = self.flags.iter().filter(|(f, _)| f == flag);
        let first = values.next().and_then(|(_, v)| v.clone());
        match values.next() {
            Some(_) => Err(usage(format!("{flag} is given twice"))),
            None => Ok(first),
        }
    }

    fn tests(&self) -> Result<Vec<(String, String)>, Failure> {
        (self.flags.iter())
            .filter(|(f, _)| f == "--match")
            .filter_map(|(_, v)| v.as_deref())
            .map(dir::parse_label)
            .collect()
    }
}

fn later(word: &str) -> Result<bool, Failure> {
    match word {
        "later" => Ok(true),
        "earlier" => Ok(false),
        other => Err(usage(format!("{other}: earlier or later"))),
    }
}

/// Reads a layout operation from its words, and whether it ends the panes
/// of what it closes.
fn layout_op(words: Vec<String>) -> Result<(Op, bool), Failure> {
    let pane = |text: &str| Target::parse(text);
    let owned = |text: &str| text.to_owned();
    let head: Vec<&str> = words.iter().take(2).map(String::as_str).collect();
    let (skip, options): (usize, &[(&str, bool)]) = match head.as_slice() {
        ["split", ..] => (
            1,
            &[("--rows", false), ("--columns", false), ("--before", false)],
        ),
        ["move", ..] => (1, &[("--tab", true), ("--workspace", true)]),
        ["tab", "new"] => (
            2,
            &[
                ("--after", true),
                ("--workspace", true),
                ("--title", true),
                ("--match", true),
            ],
        ),
        ["tab", "close"] | ["workspace", "close"] => (2, &[("--end-panes", false)]),
        ["workspace", "new"] => (2, &[("--title", true)]),
        ["tab" | "workspace", _] => (2, &[]),
        _ => (1, &[]),
    };
    let named: Vec<String> = words.iter().take(skip).cloned().collect();
    let words = Words::read(words.into_iter().skip(skip).collect(), options)?;
    let named: Vec<&str> = named.iter().map(String::as_str).collect();
    let rest: Vec<&str> = words.rest.iter().map(String::as_str).collect();
    let op = match (named.as_slice(), rest.as_slice()) {
        (["split"], [target, new]) => Op::Split {
            target: pane(target)?,
            new: pane(new)?,
            axis: match (words.has("--rows"), words.has("--columns")) {
                (true, true) => return Err(usage("split takes --rows or --columns, not both")),
                (true, false) => Axis::Rows,
                (false, _) => Axis::Columns,
            },
            before: words.has("--before"),
        },
        (["remove"], [target]) => Op::Remove(pane(target)?),
        (["replace"], [old, new]) => Op::Replace {
            old: pane(old)?,
            new: pane(new)?,
        },
        (["swap"], [a, b]) => Op::Swap(pane(a)?, pane(b)?),
        (["move"], [target]) => match (words.value("--tab")?, words.value("--workspace")?) {
            (Some(tab), None) => Op::ToTab {
                pane: pane(target)?,
                tab: (tab != "new").then_some(tab),
            },
            (None, Some(space)) => Op::ToWorkspace {
                pane: pane(target)?,
                workspace: (space != "new").then_some(space),
            },
            _ => return Err(usage("move PANE needs a direction, --tab or --workspace")),
        },
        (["move"], [target, way]) if words.flags.is_empty() => match *way {
            "earlier" | "later" => Op::Reorder {
                pane: pane(target)?,
                later: later(way)?,
            },
            _ => Op::Step {
                pane: pane(target)?,
                dir: Dir::parse(way)?,
            },
        },
        (["border"], [target, way, amount]) => Op::Border {
            pane: pane(target)?,
            dir: Dir::parse(way)?,
            amount: Amount::parse(amount)?,
        },
        (["zoom"], [target]) => Op::Zoom(pane(target)?),
        (["title"], [target, text]) => Op::Title {
            pane: pane(target)?,
            title: owned(text),
        },
        (["tab", "new"], []) => Op::NewTab {
            after: words.value("--after")?,
            workspace: words.value("--workspace")?,
            title: words.value("--title")?,
            tests: words.tests()?,
        },
        (["tab", "rename"], [tab, text]) => Op::RenameTab {
            tab: owned(tab),
            title: owned(text),
        },
        (["tab", "close"], [tab]) => Op::CloseTab(owned(tab)),
        (["tab", "move"], [tab, way]) => Op::MoveTab {
            tab: owned(tab),
            later: later(way)?,
        },
        (["tab", "match"], [tab, labels @ ..]) => Op::MatchTab {
            tab: owned(tab),
            tests: labels
                .iter()
                .map(|l| dir::parse_label(l))
                .collect::<Result<_, _>>()?,
        },
        (["workspace", "new"], []) => Op::NewWorkspace {
            title: words.value("--title")?,
        },
        (["workspace", "rename"], [space, text]) => Op::RenameWorkspace {
            workspace: owned(space),
            title: owned(text),
        },
        (["workspace", "close"], [space]) => Op::CloseWorkspace(owned(space)),
        (["workspace", "move"], [space, way]) => Op::MoveWorkspace {
            workspace: owned(space),
            later: later(way)?,
        },
        (["reset"], []) => Op::Reset,
        _ => {
            let all = [named.join(" "), rest.join(" ")].join(" ");
            return Err(usage(format!(
                "not a layout command: {}\n\n{USAGE}",
                all.trim()
            )));
        }
    };
    Ok((op, words.has("--end-panes")))
}

/// Ends each pane a close let go of: one already gone or replaced is no
/// failure; any other is said, every one of them.
fn end_panes(panes: &[PaneRef]) -> Outcome {
    let mut failed = Vec::new();
    for pane in panes {
        let target = Target {
            name: pane.name.clone(),
            id: Some(pane.id),
        };
        match kill_one(&target, None) {
            Ok(()) => {}
            Err(f) if matches!(f.code, Code::Missing | Code::Replaced | Code::Gone) => {}
            Err(f) => failed.push(format!("{}: {f}", pane.name)),
        }
    }
    match failed.is_empty() {
        true => Ok(()),
        false => Err(Failure::new(Code::Io, failed.join("; "))),
    }
}

/// Reads or changes the stored layout.
fn layout(mut args: Args) -> Outcome {
    let mut words = Vec::new();
    while let Some(word) = args.next()? {
        words.push(word);
    }
    // `--if` and `--area` belong to every edit; they are taken first.
    let mut all = Words::read(words, &[("--if", true), ("--area", true)])?;
    let expected = all
        .value("--if")?
        .map(|v| {
            v.parse::<u64>()
                .map_err(|_| usage(format!("--if: not a version: {v}")))
        })
        .transpose()?;
    let area = match all.value("--area")? {
        Some(text) => Rect::parse_area(&text)?,
        None => layout::NOMINAL,
    };
    let words = std::mem::take(&mut all.rest);
    let session = Session::open()?;
    if words.first().map(String::as_str) == Some("show") {
        if words.len() > 1 || !all.flags.is_empty() {
            return Err(usage("show takes nothing more"));
        }
        let live = live_panes(&session)?;
        let stored = layout::read(&session)?;
        return say(&stored.shown(&live).to_string());
    }
    let (op, ending) = layout_op(words)?;
    // Panes are asked before the lock is taken: a slow pane holds no
    // other writer.
    let live = live_panes(&session)?;
    let edited = layout::edit(&session, expected, &live, area, &op)?;
    say(&edited.to_string())?;
    if ending {
        end_panes(&edited.let_go)?;
        let names: Vec<String> = edited
            .let_go
            .iter()
            .map(|p| format!("ended {p}\n"))
            .collect();
        say(&names.concat())?;
    }
    Ok(())
}

/// Runs the viewer installed beside this program.
fn view(args: Args) -> Outcome {
    let exe = std::env::current_exe().map_err(|e| Failure::new(Code::Io, format!("{e}")))?;
    let viewer = exe.with_file_name("fux2-view");
    if !viewer.exists() {
        let why = format!("the viewer is not installed: no {}", viewer.display());
        return Err(Failure::new(Code::Missing, why));
    }
    let mut command = std::process::Command::new(&viewer);
    command.args(args.0);
    let failed = |e: std::io::Error| Failure::new(Code::Io, format!("{}: {e}", viewer.display()));
    use std::os::unix::process::CommandExt;
    Err(failed(command.exec()))
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
        "layout" => layout(args),
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
