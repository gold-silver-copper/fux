//! Terminals on Windows: a pseudo console's size, and the console this
//! process runs in, as Unix has a terminal on stdin. Windows keeps no size
//! in pixels and no foreground group for a console (D-137): fux-vt's
//! in-band answers give the pixels, and the group is always none.
//!
//! - [`Raw`] sets the console for a program that reads keys and writes
//!   escape sequences, and puts back both modes and the output code page.
//! - `std::io::Stdin` and `Stdout` are [`crate::poll::Source`]s. A console
//!   can't join a completion port, but it can be waited on, so stdin is
//!   watched as a listener is: a one-shot thread-pool wait whose callback
//!   only wakes the poll. No thread of ours reads a console, so none is
//!   left behind to eat a key.
//! - A console gives records, not bytes: [`Stdin`'s read](crate::io::read)
//!   turns them into the bytes a Unix terminal would give
//!   ([`records_to_bytes`]), and a window's new size into
//!   [`Signal::Winch`], as Unix's kernel raises it.
use super::{Pixels, Size};
use crate::errno::{Errno, Result};
use crate::poll::{Events, Source, Wait, sealed::Sealed};
use crate::process::{Pid, Signal};
use crate::pty::Master;
use std::collections::VecDeque;
use std::io::{Stdin, Stdout};
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, NO_ERROR,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_UNKNOWN, GetFileType, OPEN_EXISTING,
    ReadFile, WriteFile,
};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_PROCESSED_INPUT,
    ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ENABLE_WINDOW_INPUT, FOCUS_EVENT, GetConsoleMode, GetConsoleOutputCP,
    GetConsoleScreenBufferInfo, GetNumberOfConsoleInputEvents, GetStdHandle, INPUT_RECORD,
    KEY_EVENT, LEFT_CTRL_PRESSED, RIGHT_CTRL_PRESSED, ReadConsoleInputW, STD_ERROR_HANDLE,
    STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode, SetConsoleOutputCP,
    SetStdHandle, WINDOW_BUFFER_SIZE_EVENT,
};

/// The size `master`'s pseudo console was last set to ([`set_window_size`]
/// or [`crate::pty::open`]), with no pixels: Windows keeps none.
pub fn window_size(master: &Master) -> Option<(Size, Option<Pixels>)> {
    Some((master.size(), None))
}

/// Resizes `master`'s pseudo console to `size`; `pixels` is not kept, as
/// Windows has nowhere to keep it. The program learns of it as a console
/// program does (a window-size event, and `GetConsoleScreenBufferInfo`).
/// A size over 32767 is refused with `INVAL`.
pub fn set_window_size(master: &Master, size: Size, pixels: Option<Pixels>) -> Result<()> {
    let _ = pixels;
    master.resize(size)
}

/// The foreground process group of `master`'s pseudo console: always none,
/// as a Windows console has no process groups to put in front (D-137).
pub fn foreground_group(master: &Master) -> Result<Option<Pid>> {
    let _ = master;
    Ok(None)
}

/// What the console's input mode is while [`Raw`] holds it: the terminal's
/// own bytes (VT input), and a record for each new size, as measured
/// (S4). Keys that would be signals are keys.
const RAW_INPUT: u32 = ENABLE_VIRTUAL_TERMINAL_INPUT | ENABLE_WINDOW_INPUT;
/// And what its output mode gains.
const RAW_OUTPUT: u32 =
    ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
/// UTF-8, the code page `io::write`'s bytes are read in.
const UTF8: u32 = 65001;

/// The standard handle `which`, if it is one: a process can have none, or
/// one that names nothing (fux starts a pane's program with invalid ones).
fn standard(which: STD_HANDLE) -> Option<HANDLE> {
    // SAFETY: it takes a number and touches no memory.
    let handle = unsafe { GetStdHandle(which) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: a handle that names nothing only fails.
    let kind = unsafe { GetFileType(handle) };
    // SAFETY: it reads this thread's last error.
    (kind != FILE_TYPE_UNKNOWN || unsafe { GetLastError() } == NO_ERROR).then_some(handle)
}

/// `handle`'s console mode, if it is a console.
fn mode(handle: HANDLE) -> Option<u32> {
    let mut mode = 0u32;
    // SAFETY: `mode` is writable; a handle that is no console only fails.
    (unsafe { GetConsoleMode(handle, &mut mode) } != 0).then_some(mode)
}

/// The console's own input or output, opened by `name` (`CONIN$`,
/// `CONOUT$`): fails where this process has no console.
fn device(name: &str) -> Result<OwnedHandle> {
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: `name` is NUL-ended; not inherited.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(Errno::last());
    }
    // SAFETY: `raw` was just opened, and nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Opens the console for each standard handle this process has none of,
/// as a program in a pseudo console may be started (`crate::pty`), and
/// std's stdio would read nothing and write nowhere. One it was given,
/// a file or a pipe among them, stays. With no console, nothing changes.
pub fn open_stdio() {
    for (which, name) in [
        (STD_INPUT_HANDLE, "CONIN$"),
        (STD_OUTPUT_HANDLE, "CONOUT$"),
        (STD_ERROR_HANDLE, "CONOUT$"),
    ] {
        if standard(which).is_none()
            && let Ok(console) = device(name)
        {
            // SAFETY: the handle is open; from now on it is the process's,
            // which std looks up at each use and never closes.
            if unsafe { SetStdHandle(which, console.as_raw_handle()) } != 0 {
                let _ = console.into_raw_handle();
            }
        }
    }
}

/// The rows and columns of the console's window, which a program reading
/// its keys draws in: its window, not its buffer, which has the scrollback's
/// rows too. Windows says no pixels.
pub fn stdin_size() -> Option<(Size, Option<Pixels>)> {
    let output = device("CONOUT$").ok()?;
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: the handle is open, and `info` is writable.
    if unsafe { GetConsoleScreenBufferInfo(output.as_raw_handle(), &mut info) } == 0 {
        return None;
    }
    let window = info.srWindow;
    let span = |from: i16, to: i16| u16::try_from(to.saturating_sub(from).saturating_add(1)).ok();
    let size = Size::new(
        span(window.Top, window.Bottom)?,
        span(window.Left, window.Right)?,
    )?;
    Some((size, None))
}

/// The console's input mode, output mode and output code page, as [`Raw`]
/// saves them: what a program must find as it was once another has left.
pub fn console_state() -> Option<(u32, u32, u32)> {
    let (input, output) = (device("CONIN$").ok()?, device("CONOUT$").ok()?);
    // SAFETY: it takes nothing and touches no memory.
    let page = unsafe { GetConsoleOutputCP() };
    Some((
        mode(input.as_raw_handle())?,
        mode(output.as_raw_handle())?,
        page,
    ))
}

/// Whether stdin and stdout are this process's console, both: not a file
/// or a pipe. Not `IsTerminal`, which says yes of a Unix-style terminal's
/// pipes that no console answers on (Git Bash's mintty).
pub fn on_one_terminal() -> bool {
    let console = |which| standard(which).and_then(mode).is_some();
    console(STD_INPUT_HANDLE) && console(STD_OUTPUT_HANDLE)
}

/// The console set for keys and escape sequences until dropped, then as
/// it was, however that comes: a return, an error, a panic, or the console
/// closing, logging off or shutting down, which [`restore_held`] answers
/// from the control handler this sets. Nothing in the process can act on
/// `TerminateProcess`: a client killed so leaves the console as it was set.
///
/// As on Unix, a program that writes something to its terminal as it
/// leaves sets its own panic hook after entering, to run first
/// ([`leaving`]).
pub struct Raw<F: AsHandle> {
    _terminal: F,
    id: u64,
}

/// What is put back: the modes and code page saved, and the console's two
/// handles, opened for it, which nothing else closes.
struct Held {
    id: u64,
    input: OwnedHandle,
    output: OwnedHandle,
    input_mode: u32,
    output_mode: u32,
    code_page: u32,
}

impl Held {
    /// Puts back all it saved, each whatever became of the others.
    fn restore(&self) {
        // SAFETY: the handle is open, this entry's own; no memory is touched.
        unsafe { SetConsoleMode(self.input.as_raw_handle(), self.input_mode) };
        // SAFETY: as above.
        unsafe { SetConsoleMode(self.output.as_raw_handle(), self.output_mode) };
        // SAFETY: it takes a number and touches no memory.
        unsafe { SetConsoleOutputCP(self.code_page) };
    }
}

/// Each live [`Raw`], in the order entered. Held while one is restored:
/// the restorer that came second waits till the console is back, rather
/// than find the list empty and go on (a close handler's return ends the
/// process).
static HELD: Mutex<Vec<Held>> = Mutex::new(Vec::new());

fn held() -> MutexGuard<'static, Vec<Held>> {
    HELD.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Puts back every console a live [`Raw`] holds, the latest first; each
/// once, however many ways out run. From a panic hook or a control
/// handler, on any thread: no output is written and nothing waits on one.
pub(crate) fn restore_held() {
    let mut held = held();
    while let Some(entry) = held.pop() {
        entry.restore();
    }
}

impl<F: AsHandle> Raw<F> {
    /// Raw now, the terminal's keys as bytes and no key a signal.
    pub fn enter(terminal: F) -> Result<Raw<F>> {
        Raw::enter_with(terminal, RAW_INPUT)
    }

    /// As [`Raw::enter`], with Ctrl-C and Ctrl-Break still the console's
    /// signals, for a program that takes them as a Unix one takes SIGINT.
    pub fn enter_keeping_signals(terminal: F) -> Result<Raw<F>> {
        Raw::enter_with(terminal, RAW_INPUT | ENABLE_PROCESSED_INPUT)
    }

    fn enter_with(terminal: F, input_mode: u32) -> Result<Raw<F>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        mode(terminal.as_handle().as_raw_handle()).ok_or_else(Errno::last)?;
        let (input, output) = (device("CONIN$")?, device("CONOUT$")?);
        let saved_input = mode(input.as_raw_handle()).ok_or_else(Errno::last)?;
        let saved_output = mode(output.as_raw_handle()).ok_or_else(Errno::last)?;
        // SAFETY: it takes nothing and touches no memory.
        let code_page = unsafe { GetConsoleOutputCP() };
        crate::signal::handle_console(false)?;
        static HOOKED: std::sync::Once = std::sync::Once::new();
        HOOKED.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_held();
                previous(info);
            }));
        });
        let id = NEXT.fetch_add(1, Ordering::SeqCst);
        let (to_input, to_output) = (input.as_raw_handle(), output.as_raw_handle());
        // Registered before anything changes, so that a close or a panic
        // from here on, and a failure below, put back whatever was.
        held().push(Held {
            id,
            input,
            output,
            input_mode: saved_input,
            output_mode: saved_output,
            code_page,
        });
        let entered = Raw {
            _terminal: terminal,
            id,
        };
        // Under the lock, so that no restore closes these handles meanwhile
        // (the entry goes only with `entered`, or with one).
        let list = held();
        // SAFETY: the handle is open while its entry is held; a console
        // that closed only fails.
        let input_set = unsafe { SetConsoleMode(to_input, input_mode) } != 0;
        // SAFETY: as above.
        let output_set =
            input_set && unsafe { SetConsoleMode(to_output, saved_output | RAW_OUTPUT) } != 0;
        // SAFETY: it takes a number and touches no memory.
        let set = output_set && unsafe { SetConsoleOutputCP(UTF8) } != 0;
        let errno = (!set).then(Errno::last);
        drop(list);
        // A failure drops `entered`, which puts back what was changed.
        errno.map_or(Ok(entered), Err)
    }
}

impl<F: AsHandle> Drop for Raw<F> {
    fn drop(&mut self) {
        held().retain(|entry| {
            let mine = entry.id == self.id;
            if mine {
                entry.restore();
            }
            !mine
        });
    }
}

/// How long a panic's last output may hold up putting the console back.
const LEAVING: Duration = Duration::from_millis(250);

/// Runs `leave`, what a program writes to its terminal as a panic ends it,
/// before the panic hook puts the console back: the modes must still be
/// on for it to mean anything. A console that stopped reading would hold it
/// for ever, so it runs on a thread of its own, waited for [`LEAVING`] at
/// most: restoring never waits on output.
pub fn leaving(leave: fn()) {
    let (done, finished) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("fuxix-leaving".into())
        .spawn(move || {
            leave();
            let _ = done.send(());
        });
    if started.is_ok() {
        let _ = finished.recv_timeout(LEAVING);
    }
}

/// How many console records a read takes at most at once, and how many
/// bytes a read holds before it stops taking more: a big paste comes in
/// slices, and the rest waits in the console.
const RECORDS: usize = 64;
const HOLD: usize = 64 * 1024;

/// What console records left: a high surrogate waiting for its low one,
/// and whether the window's size changed.
#[derive(Default)]
pub(crate) struct Pending {
    high: Option<u16>,
    resized: bool,
}

/// Appends the character `unit` ends, if it does, to `out`: UTF-16 units
/// to UTF-8, a surrogate alone as U+FFFD.
fn push_unit(pending: &mut Pending, unit: u16, out: &mut VecDeque<u8>) {
    let mut scalar = |units: &[u16]| {
        for decoded in char::decode_utf16(units.iter().copied()) {
            let c = decoded.unwrap_or(char::REPLACEMENT_CHARACTER);
            out.extend(c.encode_utf8(&mut [0; 4]).as_bytes());
        }
    };
    match (pending.high.take(), unit) {
        (Some(high), 0xDC00..=0xDFFF) => scalar(&[high, unit]),
        (high, _) => {
            if let Some(high) = high {
                scalar(&[high]);
            }
            match unit {
                0xD800..=0xDBFF => pending.high = Some(unit),
                unit => scalar(&[unit]),
            }
        }
    }
}

/// The virtual-key codes [`records_to_bytes`] looks at: Space and 2, each
/// NUL with Ctrl (a terminal's NUL comes as Ctrl+Shift+2, measured on CI).
const VK_SPACE: u16 = 0x20;
const VK_2: u16 = 0x32;
const VK_MENU: u16 = 0x12;

/// `records`, as the bytes a Unix terminal would have given, appended to
/// `out`. With VT input on, a key's bytes come as the characters of
/// key-down records, however many times its repeat count says; in-box
/// conhost gives an emoji on `VK_MENU` key-up records, as Alt+Numpad does
/// (S4). A key down with no character is a modifier or the like and gives
/// nothing (that emoji's Alt+Numpad records are such), but for Ctrl+Space
/// and Ctrl+2, which are NUL. Focus becomes the report the terminal would
/// have sent, and a new size is left in `pending` for the caller.
pub(crate) fn records_to_bytes(
    pending: &mut Pending,
    records: &[INPUT_RECORD],
    out: &mut VecDeque<u8>,
) {
    for record in records {
        match u32::from(record.EventType) {
            KEY_EVENT => {
                // SAFETY: `EventType` says this union member is the one.
                let key = unsafe { record.Event.KeyEvent };
                // SAFETY: a `u16` either way; both members are plain.
                let unit = unsafe { key.uChar.UnicodeChar };
                let ctrl = key.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;
                let down = key.bKeyDown != 0;
                if down && unit == 0 && ctrl && [VK_SPACE, VK_2].contains(&key.wVirtualKeyCode) {
                    for _ in 0..key.wRepeatCount.max(1) {
                        out.push_back(0);
                    }
                } else if down && unit != 0 {
                    // A surrogate's repeat would split the pair.
                    let surrogate = (0xD800..=0xDFFF).contains(&unit);
                    let times = if surrogate {
                        1
                    } else {
                        key.wRepeatCount.max(1)
                    };
                    for _ in 0..times {
                        push_unit(pending, unit, out);
                    }
                } else if !down && unit != 0 && key.wVirtualKeyCode == VK_MENU {
                    push_unit(pending, unit, out);
                }
            }
            WINDOW_BUFFER_SIZE_EVENT => pending.resized = true,
            FOCUS_EVENT => {
                // SAFETY: `EventType` says this union member is the one.
                let focus = unsafe { record.Event.FocusEvent };
                let report: &[u8] = match focus.bSetFocus != 0 {
                    true => b"\x1b[I",
                    false => b"\x1b[O",
                };
                out.extend(report);
            }
            // Mouse reports come as bytes with VT input; menus are noise.
            _ => {}
        }
    }
}

/// What stdin holds for a read, for the whole process, as stdin is.
#[derive(Default)]
struct Input {
    /// Bytes read and not yet taken.
    bytes: VecDeque<u8>,
    pending: Pending,
    /// The poll's one-shot wait on the console.
    wait: Option<Wait>,
    /// A pipe's or a file's reader, once a poll asked for one.
    piped: Option<Receiver<Vec<u8>>>,
    /// That reader ended.
    ended: bool,
}

static INPUT: Mutex<Option<Input>> = Mutex::new(None);

/// A wait on the console fired since the last was made: the console stays
/// signalled while it holds records, so each wait is good for one wake.
static FIRED: AtomicBool = AtomicBool::new(false);

/// What the thread pool runs as the console holds records.
unsafe extern "system" fn console_ready(_: *mut core::ffi::c_void, _: bool) {
    FIRED.store(true, Ordering::SeqCst);
    // A wake that can't be posted has nobody to tell.
    let _ = crate::poll::wake();
}

fn with_input<T>(act: impl FnOnce(&mut Input) -> T) -> T {
    let mut input = INPUT.lock().unwrap_or_else(PoisonError::into_inner);
    act(input.get_or_insert_with(Input::default))
}

/// stdin, if it is a console.
fn console_input() -> Option<HANDLE> {
    standard(STD_INPUT_HANDLE).filter(|handle| mode(*handle).is_some())
}

/// How many records the console holds; none if it has gone.
fn queued(console: HANDLE) -> Option<u32> {
    let mut count = 0u32;
    // SAFETY: `count` is writable; a console gone only fails.
    (unsafe { GetNumberOfConsoleInputEvents(console, &mut count) } != 0).then_some(count)
}

/// Reads stdin, a pipe or a file, on a thread of its own, into a bounded
/// queue a poll can look at: `ReadFile` on it can't be waited on, and a
/// thread blocked in it can't be told Ctrl-C came.
fn read_piped(stdin: HANDLE) -> Option<Receiver<Vec<u8>>> {
    let (send, receive) = std::sync::mpsc::sync_channel(4);
    let stdin = stdin as usize;
    let reader = move || {
        loop {
            let mut buffer = vec![0u8; 16 * 1024];
            let room = u32::try_from(buffer.len()).unwrap_or(0);
            let mut got = 0u32;
            // SAFETY: stdin is open for the process's life, and the buffer
            // is writable for `room` bytes; a plain handle, so it waits.
            let done = unsafe {
                ReadFile(
                    stdin as HANDLE,
                    buffer.as_mut_ptr(),
                    room,
                    &mut got,
                    std::ptr::null_mut(),
                )
            };
            if done == 0 || got == 0 {
                break;
            }
            buffer.truncate(usize::try_from(got).unwrap_or(0));
            if send.send(buffer).is_err() {
                return;
            }
            let _ = crate::poll::wake();
        }
        drop(send);
        let _ = crate::poll::wake();
    };
    let thread = std::thread::Builder::new().name("fuxix-stdin".into());
    thread.spawn(reader).ok().map(|_| receive)
}

impl Input {
    /// What a pipe's reader brought, moved in.
    fn take_piped(&mut self) {
        let Some(piped) = &self.piped else {
            return;
        };
        match piped.try_recv() {
            Ok(bytes) => self.bytes.extend(bytes),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.ended = true,
        }
    }

    /// What the console holds, up to a bound, turned into bytes. Whether it
    /// is still there.
    fn take_records(&mut self, console: HANDLE) -> bool {
        while self.bytes.len() < HOLD {
            let Some(count) = queued(console) else {
                return false;
            };
            if count == 0 {
                break;
            }
            let mut records = [INPUT_RECORD::default(); RECORDS];
            let room = u32::try_from(RECORDS).unwrap_or(1).min(count);
            let mut got = 0u32;
            // SAFETY: `records` is writable for `room` records. Only this
            // process's readers take them, and only under `INPUT`'s lock,
            // so the `count` queued are there and it doesn't wait.
            if unsafe { ReadConsoleInputW(console, records.as_mut_ptr(), room, &mut got) } == 0 {
                return false;
            }
            let got = usize::try_from(got).unwrap_or(0);
            let taken = records.get(..got).unwrap_or_default();
            records_to_bytes(&mut self.pending, taken, &mut self.bytes);
        }
        if std::mem::take(&mut self.pending.resized) {
            // A wake that can't be posted has a broken port: the poll fails.
            let _ = crate::signal::tell(Signal::Winch);
        }
        true
    }

    /// Gives what it holds to `buffer`.
    fn give(&mut self, buffer: &mut [u8]) -> usize {
        let mut count = 0usize;
        for to in buffer.iter_mut() {
            let Some(from) = self.bytes.pop_front() else {
                break;
            };
            *to = from;
            count = count.saturating_add(1);
        }
        count
    }
}

impl Sealed for Stdin {
    /// Watches the console for records, or starts stdin's reader. Bytes
    /// already held need no wake: `ready` says so.
    fn arm(&self, interest: Events) {
        if !interest.intersects(Events::IN | Events::HUP) {
            return;
        }
        with_input(|input| {
            let Some(console) = console_input() else {
                if input.piped.is_none()
                    && let Some(stdin) = standard(STD_INPUT_HANDLE)
                {
                    input.piped = read_piped(stdin);
                }
                return;
            };
            if input.wait.is_none() || FIRED.swap(false, Ordering::SeqCst) {
                input.wait = None;
                input.wait = Wait::once(console, console_ready).ok();
            }
        });
    }

    /// What it holds counts, not only what the console does, or the tail
    /// of a paste would wait for the next key.
    fn ready(&self, interest: Events) -> Events {
        if !interest.intersects(Events::IN) {
            return Events::NONE;
        }
        with_input(|input| {
            input.take_piped();
            let queued = console_input().is_some_and(|console| queued(console) != Some(0));
            match !input.bytes.is_empty() || input.ended || queued {
                true => Events::IN,
                false => Events::NONE,
            }
        })
    }

    /// Never waits: what was typed or piped, `AGAIN` if records brought no
    /// bytes (a key's release, a new size) or nothing has come, and 0 once
    /// the console or the pipe has ended.
    fn read(&self, buffer: &mut [u8]) -> Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        with_input(|input| {
            input.take_piped();
            let open = match console_input() {
                Some(console) if input.bytes.is_empty() => input.take_records(console),
                Some(_) => true,
                None => !input.ended,
            };
            match (input.give(buffer), open) {
                (0, false) => Ok(0),
                (0, true) => Err(Errno::AGAIN),
                (count, _) => Ok(count),
            }
        })
    }

    fn write(&self, _: &[u8]) -> Result<usize> {
        Err(Errno::BADF)
    }
}

impl Source for Stdin {}

impl Sealed for Stdout {
    fn arm(&self, _: Events) {}

    /// Writes wait, so it is always writable; a pipe's reader gone is
    /// asked of the pipe at each look (`crate::io::STDOUT_LOOK`).
    fn ready(&self, interest: Events) -> Events {
        let mut ready = Events::NONE;
        if interest.intersects(Events::OUT) {
            ready |= Events::OUT;
        }
        if interest.intersects(Events::HUP | Events::ERR) && crate::io::stdout_reader_gone() {
            ready |= Events::ERR | Events::HUP;
        }
        ready
    }

    fn read(&self, _: &mut [u8]) -> Result<usize> {
        Err(Errno::BADF)
    }

    /// `WriteFile`, on the thread that asks, taking no lock of std's, and
    /// waiting as a blocking descriptor does. On a console its bytes are
    /// read in the output code page, UTF-8 under [`Raw`], not as std's
    /// `WriteConsoleW` reads them, which fails on a paint split inside a
    /// character.
    fn write(&self, bytes: &[u8]) -> Result<usize> {
        let stdout = standard(STD_OUTPUT_HANDLE).ok_or(Errno::BADF)?;
        let count = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let mut put = 0u32;
        // SAFETY: stdout is open; `bytes` is readable for `count` bytes,
        // and the call is plain, done as it returns.
        let done = unsafe {
            WriteFile(
                stdout,
                bytes.as_ptr(),
                count,
                &mut put,
                std::ptr::null_mut(),
            )
        };
        match done {
            0 => Err(Errno::last()),
            _ => Ok(usize::try_from(put).unwrap_or(0)),
        }
    }
}

impl Source for Stdout {}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Console::{
        FOCUS_EVENT_RECORD, INPUT_RECORD_0, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0,
        WINDOW_BUFFER_SIZE_RECORD,
    };

    /// A key record: down or up, its virtual key, character, repeat count
    /// and modifiers.
    fn key(down: bool, vk: u16, unit: u16, repeat: u16, state: u32) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: u16::try_from(KEY_EVENT).unwrap_or(0),
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: i32::from(down),
                    wRepeatCount: repeat,
                    wVirtualKeyCode: vk,
                    wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit },
                    dwControlKeyState: state,
                },
            },
        }
    }

    fn bytes(pending: &mut Pending, records: &[INPUT_RECORD]) -> Vec<u8> {
        let mut out = VecDeque::new();
        records_to_bytes(pending, records, &mut out);
        out.into()
    }

    /// In-box conhost's emoji, as S4 measured it: Alt+Numpad key-downs of
    /// no character, the units on Alt's key-ups, here split over two
    /// reads; a key's repeat count; Ctrl+Space, which alone of the keys
    /// with no character is NUL; a surrogate alone; a size and focus.
    #[test]
    fn records_become_a_terminals_bytes() {
        let mut pending = Pending::default();
        let digits = [(18, 0), (102, 0), (99, 0)].map(|(vk, unit)| key(true, vk, unit, 1, 0));
        assert!(bytes(&mut pending, &digits).is_empty());
        assert!(bytes(&mut pending, &[key(false, VK_MENU, 0xD83D, 1, 0)]).is_empty());
        assert_eq!(
            bytes(
                &mut pending,
                &[key(false, 0x10, 0, 1, 0), key(false, VK_MENU, 0xDE00, 1, 0)]
            ),
            "😀".as_bytes()
        );
        assert_eq!(bytes(&mut pending, &[key(true, 0x41, 97, 3, 0)]), b"aaa");
        assert_eq!(bytes(&mut pending, &[key(false, 0x41, 97, 1, 0)]), b"");
        let space = key(true, VK_SPACE, 0, 1, LEFT_CTRL_PRESSED);
        assert_eq!(bytes(&mut pending, &[space]), [0]);
        let at = key(true, VK_2, 0, 1, LEFT_CTRL_PRESSED | 0x10);
        assert_eq!(bytes(&mut pending, &[at]), [0]);
        assert_eq!(bytes(&mut pending, &[key(true, VK_SPACE, 0, 1, 0)]), b"");
        assert_eq!(
            bytes(
                &mut pending,
                &[key(true, 0x41, 0xD83D, 1, 0), key(true, 0x41, 97, 1, 0)]
            ),
            "\u{FFFD}a".as_bytes()
        );
        let resize = INPUT_RECORD {
            EventType: u16::try_from(WINDOW_BUFFER_SIZE_EVENT).unwrap_or(0),
            Event: INPUT_RECORD_0 {
                WindowBufferSizeEvent: WINDOW_BUFFER_SIZE_RECORD::default(),
            },
        };
        assert_eq!(bytes(&mut pending, &[resize]), b"");
        assert!(pending.resized, "and no bytes: a read of it says AGAIN");
        let focus = INPUT_RECORD {
            EventType: u16::try_from(FOCUS_EVENT).unwrap_or(0),
            Event: INPUT_RECORD_0 {
                FocusEvent: FOCUS_EVENT_RECORD { bSetFocus: 1 },
            },
        };
        assert_eq!(bytes(&mut pending, &[focus]), b"\x1b[I");
    }
}
