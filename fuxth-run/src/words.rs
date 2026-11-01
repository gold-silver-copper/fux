//! The runner's words: the terminal's, files', and the process's, each
//! checking its own values as it runs. A file word that can fail as the
//! OS refuses gives an ior: 0, or a failure that holds the OS's message,
//! for `throw` or a test.
use crate::input::{self, Input, Stop};
use fuxth::{AppValue, Args, Code, Done, Error, Failure, Host, List, Thrown, Value, Vocabulary};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The codes of the runner's own failures; its others are the language's
/// (`interrupt`, `end_of_input`, `unsupported`), which carry Forth's
/// numbers.
const UNSET_VAR: &str = "unset_var";
pub const READ: &str = "read";
pub const IO: &str = "io";
pub const OUTPUT: &str = "output";

/// A file `create-file` opened, by its place in the runner's table.
#[derive(Clone, Debug, PartialEq)]
pub struct File(usize);

impl AppValue for File {
    fn kind(&self) -> &'static str {
        "a file"
    }
}

/// What a word waits for the runner to do with the machine.
pub enum Request {
    /// The REPL's next line, after the stack and `ok`.
    Line,
    /// Note that the word took effect.
    Acted,
    /// End the program, with this status.
    Exit(u8),
    /// End the runner, as this signal asks.
    End(fuxix::process::Signal),
}

/// Expands `Variant "name" "effect",` to the enum `Word` and `WORDS`, in
/// the order the vocabulary adds them, so a word's id is its place.
macro_rules! words {
    ($($variant:ident $name:literal $effect:literal,)*) => {
        #[derive(Clone, Copy)]
        enum Word {
            $($variant,)*
        }

        const WORDS: &[(Word, &str, &str)] = &[$((Word::$variant, $name, $effect),)*];
    };
}

words! {
    ReadKey "read-key" "-- k",
    KeyReady "key-ready" "-- flag",
    Accept "accept" "n -- s",
    CursorAt "cursor-at" "col row --",
    ClearScreen "clear-screen" "--",
    ScreenSize "screen-size" "-- rows cols",
    ReadFile "read-file" "path -- s",
    CreateFile "create-file" "path -- file ior",
    WriteFile "write-file" "s file -- ior",
    WriteLine "write-line" "s file -- ior",
    CloseFile "close-file" "file -- ior",
    RenameFile "rename-file" "from to -- ior",
    FileMode "file-mode" "path -- n ior",
    SetFileMode "set-file-mode" "n path -- ior",
    Getenv "getenv" "name -- s",
    Arguments "args" "-- list",
    Exit "exit" "n --",
    // Last, as only the REPL has it.
    PromptLine "prompt-line" "-- s",
}

/// The runner's words, the constants among them (`key-up`, `ctrl-mask`),
/// and `evaluate`; `prompt-line` only for the REPL.
pub fn vocabulary(repl: bool) -> Result<Vocabulary, Error> {
    let mut vocabulary = Vocabulary::default();
    let words = WORDS
        .iter()
        .filter(|(word, ..)| repl || !matches!(word, Word::PromptLine));
    let names = words.map(|&(_, name, effect)| (name, effect));
    let constants = input::CONSTANTS.iter().map(|&(name, _)| (name, "-- n"));
    for (name, effect) in names.chain(constants) {
        vocabulary.add(name, effect)?;
    }
    for code in [UNSET_VAR, READ, IO, OUTPUT] {
        vocabulary.raises(Code::Host(code))?;
    }
    vocabulary.evaluate();
    Ok(vocabulary)
}

/// What runs the runner's words.
pub struct Runner {
    pub input: Input,
    args: Vec<String>,
    /// Each file `create-file` opened, until `close-file`.
    files: Vec<Option<std::io::BufWriter<std::fs::File>>>,
    /// The words before the constants.
    words: usize,
    /// stdout's reader went: the program ends quietly.
    closed: bool,
}

impl Runner {
    pub fn new(args: Vec<String>, input: Input, repl: bool) -> Self {
        let words = WORDS.len().saturating_sub(usize::from(!repl));
        Self {
            input,
            args,
            files: Vec::new(),
            words,
            closed: false,
        }
    }

    pub fn closed(&self) -> bool {
        self.closed
    }

    /// The REPL's prompt: the stack, the deepest first, then `ok`, or
    /// `compiled` while a definition is open; then the next line, with
    /// any line after it that a quote left open goes on to. Ctrl-C drops
    /// the line and prompts again. None at the end of input.
    pub fn prompt(&mut self, stack: &[Value<File>]) -> Result<Option<String>, Stop> {
        let mut said = String::new();
        let mut open = false;
        for value in stack {
            match value {
                Value::Mark(_) => open = true,
                Value::Int(_)
                | Value::Str(_)
                | Value::List(_)
                | Value::App(_)
                | Value::Xt(_)
                | Value::Failure(_)
                | Value::Addr(_)
                | Value::Bool(_) => {
                    value.write_text(&mut said)?;
                    said.push(' ');
                }
            }
        }
        said.push_str(if open { "compiled\n" } else { "ok\n" });
        loop {
            self.out(&said)?;
            match self.entry() {
                Err(Stop::Failed(failure)) if failure.code == Code::Interrupt => self.out("\n")?,
                entry => return entry,
            }
        }
    }

    /// The REPL's next line, and those a quote left open goes on to.
    fn entry(&mut self) -> Result<Option<String>, Stop> {
        let mut text = String::new();
        loop {
            let Some(line) = self.input.line(usize::MAX)? else {
                return Ok(None);
            };
            text.push_str(&line);
            match fuxth::token::source("stdin", &text) {
                Err(error) if error.code == Code::OpenQuote => text.push('\n'),
                Ok(_) | Err(_) => return Ok(Some(text)),
            }
        }
    }

    /// Writes `text` to stdout now: once a key word made the runner catch
    /// the ending signals, a piece at a time, each once stdout takes it
    /// without waiting.
    fn out(&mut self, text: &str) -> Result<(), Failure> {
        let mut rest = text.as_bytes();
        while !rest.is_empty() {
            self.input.writable()?;
            let most = if self.input.watching() {
                input::PIPE_WRITE
            } else {
                rest.len()
            };
            let (piece, after) = rest.split_at_checked(most).unwrap_or((rest, &[]));
            let mut out = std::io::stdout().lock();
            let wrote = out.write_all(piece).and_then(|()| out.flush());
            wrote.map_err(|e| {
                self.closed |= e.kind() == std::io::ErrorKind::BrokenPipe;
                output_failed(&e)
            })?;
            rest = after;
        }
        Ok(())
    }

    /// The file `file` names, open.
    fn file(&mut self, file: &File) -> Option<&mut std::io::BufWriter<std::fs::File>> {
        self.files.get_mut(file.0)?.as_mut()
    }
}

impl Host for Runner {
    type Value = File;
    type Request = Request;

    fn call(
        &mut self,
        word: usize,
        args: Args<'_, File>,
        out: &mut Vec<Value<File>>,
    ) -> Result<Done<Request>, Failure> {
        match self.word(word, args, out) {
            Ok(done) => Ok(done),
            Err(Stop::Failed(failure)) => Err(failure),
            Err(Stop::Signal(signal)) => Ok(Done::Later(Request::End(signal))),
        }
    }

    fn write(&mut self, text: &str) -> Result<(), Failure> {
        self.out(text)
    }

    /// The file `path` names, relative to the file naming it, else to
    /// where fuxth started: its canonical path, or, for one that has none
    /// (`<(…)`'s `/dev/fd/63`), the path itself.
    fn resolve(&mut self, path: &str, from: Option<&Path>) -> Result<PathBuf, String> {
        let base = from.and_then(Path::parent).map(Path::to_owned);
        let full = match base {
            Some(base) => base.join(path),
            None => PathBuf::from(path),
        };
        identity(&full).map_err(|e| e.to_string())
    }

    /// The file's text. Stopped by Ctrl-C, `include` fails `interrupt`, which
    /// a `try` may catch; by an ending signal, it fails, and the signal is
    /// raised at the next check.
    fn read(&mut self, path: &Path) -> Result<String, Failure> {
        let unread = |why| Failure::new(Code::Require, why);
        match self.input.file(path) {
            Ok(read) => read.map_err(unread),
            Err(Stop::Failed(failure)) => Err(failure),
            Err(Stop::Signal(signal)) => {
                self.input.defer(Stop::Signal(signal));
                Err(unread(format!("SIG{}", signal.name())))
            }
        }
    }
}

impl Runner {
    /// Runs the runner's word `word` on `args`.
    fn word(
        &mut self,
        word: usize,
        mut args: Args<'_, File>,
        out: &mut Vec<Value<File>>,
    ) -> Result<Done<Request>, Stop> {
        let Some(&(word, name, _)) = WORDS.get(word).filter(|_| word < self.words) else {
            let constant = word
                .checked_sub(self.words)
                .and_then(|i| input::CONSTANTS.get(i));
            if let Some(&(_, n)) = constant {
                out.push(Value::Int(n));
            }
            return Ok(Done::Now);
        };
        let mut next = || {
            args.next()
                .ok_or_else(|| Failure::new(Code::StackUnderflow, name))
        };
        let ior = |done: std::io::Result<()>, what: &str| match done {
            Ok(()) => Value::Int(0),
            Err(e) => failure(format!("{name}: {what}: {e}")),
        };
        match word {
            Word::ReadKey => out.push(Value::Int(self.input.key()?)),
            Word::KeyReady => out.push(Value::Bool(self.input.key_ready()?)),
            Word::Accept => {
                let most = usize::try_from(int(next()?, name)?);
                let why = "accept takes how many characters at most, 0 or more";
                let most = most.map_err(|_| Failure::new(Code::Range, why))?;
                out.push(Value::str(self.input.accept(most)?));
            }
            Word::CursorAt => {
                let (col, row) = (int(next()?, name)?, int(next()?, name)?);
                if col < 0 || row < 0 {
                    let why = "cursor-at takes a column and a row from 0";
                    return Err(Failure::new(Code::Range, why).into());
                }
                let (row, col) = (row.saturating_add(1), col.saturating_add(1));
                self.out(&format!("\x1b[{row};{col}H"))?;
            }
            Word::ClearScreen => self.out("\x1b[H\x1b[2J")?,
            Word::ScreenSize => {
                let (rows, cols) = self.input.screen_size().ok_or_else(|| {
                    let why = "no terminal on stdin, and LINES and COLUMNS are not both set";
                    Failure::new(Code::Unsupported, why)
                })?;
                out.extend([Value::Int(rows), Value::Int(cols)]);
            }
            Word::ReadFile => {
                let path = string(next()?, name)?;
                let text = (self.input.file(Path::new(&**path))?)
                    .map_err(|why| Failure::new(Code::Host(READ), format!("{why} ({path})")))?;
                out.push(Value::str(text));
            }
            Word::CreateFile => {
                let path = string(next()?, name)?;
                let (file, ior) = match std::fs::File::create_new(&**path) {
                    Ok(created) => {
                        self.files.push(Some(std::io::BufWriter::new(created)));
                        let id = self.files.len().saturating_sub(1);
                        (Value::App(File(id)), Value::Int(0))
                    }
                    Err(e) => (Value::Int(0), ior(Err(e), &path)),
                };
                out.push(file);
                return Ok(acted(out, ior));
            }
            Word::WriteFile | Word::WriteLine => {
                let (text, file) = (string(next()?, name)?, app(next()?, name)?);
                let open = self.file(&file).ok_or_else(|| not_open(name))?;
                let end = if matches!(word, Word::WriteLine) {
                    "\n"
                } else {
                    ""
                };
                let wrote =
                    (open.write_all(text.as_bytes())).and_then(|()| open.write_all(end.as_bytes()));
                return Ok(acted(out, ior(wrote, "the file")));
            }
            Word::CloseFile => {
                let file = app(next()?, name)?;
                let open = self.files.get_mut(file.0).and_then(Option::take);
                let open = open.ok_or_else(|| not_open(name))?;
                let closed = open.into_inner().map(drop).map_err(|e| e.into_error());
                return Ok(acted(out, ior(closed, "the file")));
            }
            Word::RenameFile => {
                let (from, to) = (string(next()?, name)?, string(next()?, name)?);
                let renamed = std::fs::rename(&**from, &**to);
                return Ok(acted(out, ior(renamed, &format!("{from} to {to}"))));
            }
            Word::FileMode => {
                let path = string(next()?, name)?;
                match std::fs::metadata(&**path) {
                    Ok(meta) => {
                        out.extend([Value::Int(i64::from(meta.mode() & 0o7777)), Value::Int(0)])
                    }
                    Err(e) => out.extend([Value::Int(0), ior(Err(e), &path)]),
                }
            }
            Word::SetFileMode => {
                let (mode, path) = (int(next()?, name)?, string(next()?, name)?);
                let why = "set-file-mode takes a mode from 0 to 4095 (0o7777)";
                let mode = u32::try_from(mode).ok().filter(|mode| *mode <= 0o7777);
                let mode = mode.ok_or_else(|| Failure::new(Code::Range, why))?;
                let set = std::fs::set_permissions(&**path, std::fs::Permissions::from_mode(mode));
                return Ok(acted(out, ior(set, &path)));
            }
            Word::Getenv => {
                let name = string(next()?, name)?;
                let value = match std::env::var(&**name) {
                    Ok(value) => value,
                    Err(std::env::VarError::NotPresent) => {
                        let why = format!("{name} is not set");
                        return Err(Failure::new(Code::Host(UNSET_VAR), why).into());
                    }
                    Err(std::env::VarError::NotUnicode(_)) => {
                        let why = format!("{name}: its value is not text");
                        return Err(Failure::new(Code::Host(READ), why).into());
                    }
                };
                out.push(Value::str(value));
            }
            Word::Arguments => {
                let args = self.args.iter().map(|a| Value::str(a.as_str())).collect();
                out.push(Value::List(Rc::new(List::new(args))));
            }
            Word::Exit => {
                let status = u8::try_from(int(next()?, name)?)
                    .map_err(|_| Failure::new(Code::Range, "exit takes a status from 0 to 255"))?;
                return Ok(Done::Later(Request::Exit(status)));
            }
            Word::PromptLine => return Ok(Done::Later(Request::Line)),
        }
        Ok(Done::Now)
    }
}

impl Runner {
    /// The program file `file`: its one identity, and its text.
    pub fn source(&mut self, file: &str) -> Result<(PathBuf, String), Stop> {
        let failed = |why: String| Failure::new(Code::Host(READ), format!("{why} ({file})"));
        let path = identity(Path::new(file)).map_err(|e| failed(e.to_string()))?;
        let text = self.input.file(&path)?.map_err(failed)?;
        Ok((path, text))
    }
}

/// The one identity of the file at `path`: its canonical path, or the path
/// itself for a file that has none (a pipe).
fn identity(path: &Path) -> std::io::Result<PathBuf> {
    path.canonicalize().or_else(|error| match path.metadata() {
        Ok(_) => Ok(path.to_owned()),
        Err(_) => Err(error),
    })
}

fn output_failed(error: &std::io::Error) -> Failure {
    Failure::new(Code::Host(OUTPUT), format!("writing the output: {error}"))
}

/// A failure as a value, for an ior.
fn failure(message: String) -> Value<File> {
    Value::Failure(Rc::new(Thrown {
        error: Error::new(Code::Host(IO), message, None),
        number: None,
        carried: None,
        actions: Vec::new(),
        calls: Vec::new(),
    }))
}

/// `ior` pushed: a word that took effect, as its ior says, waits for the
/// runner to note it.
fn acted(out: &mut Vec<Value<File>>, ior: Value<File>) -> Done<Request> {
    let took = matches!(ior, Value::Int(0));
    out.push(ior);
    match took {
        true => Done::Later(Request::Acted),
        false => Done::Now,
    }
}

fn not_open(name: &str) -> Failure {
    Failure::new(Code::Host(IO), format!("{name}: the file is closed"))
}

fn refused(name: &str, wants: &str, value: &Value<File>) -> Failure {
    Failure::new(
        Code::Type,
        format!("{name} takes {wants}, not {}", value.kind()),
    )
}

fn int(value: Value<File>, name: &str) -> Result<i64, Failure> {
    match value {
        Value::Int(n) => Ok(n),
        other @ (Value::Str(_)
        | Value::List(_)
        | Value::App(_)
        | Value::Xt(_)
        | Value::Mark(_)
        | Value::Failure(_)
        | Value::Addr(_)
        | Value::Bool(_)) => Err(refused(name, "an integer", &other)),
    }
}

fn string(value: Value<File>, name: &str) -> Result<Rc<fuxth::Text>, Failure> {
    match value {
        Value::Str(text) => Ok(text),
        other @ (Value::Int(_)
        | Value::List(_)
        | Value::App(_)
        | Value::Xt(_)
        | Value::Mark(_)
        | Value::Failure(_)
        | Value::Addr(_)
        | Value::Bool(_)) => Err(refused(name, "a string", &other)),
    }
}

fn app(value: Value<File>, name: &str) -> Result<File, Failure> {
    match value {
        Value::App(file) => Ok(file),
        other @ (Value::Int(_)
        | Value::Str(_)
        | Value::List(_)
        | Value::Xt(_)
        | Value::Mark(_)
        | Value::Failure(_)
        | Value::Addr(_)
        | Value::Bool(_)) => Err(refused(name, "a file", &other)),
    }
}
