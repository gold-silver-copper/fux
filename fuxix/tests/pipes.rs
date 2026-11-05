//! Named pipes as a server's clients reach it (WINDOWS.md, PR 5; spike
//! S3's access, squatting and handoff probes): each one property, each
//! with a deadline, taking turns ([`turn`]) as one thread polls at a time.
//! The children are this binary's ignored tests, run with an environment
//! variable that says what to do.
#![cfg(windows)]
use fuxix::Errno;
use fuxix::io::{Stream, read, write_what_fits};
use fuxix::poll::{Events, PollFd, Source, poll};
use fuxix::socket::{Listener, Spare, User, connect, name, peer};
use std::io::{BufRead, Write};
use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

type Outcome = Result<(), String>;

fn turn() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn say(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn after(duration: Duration) -> Result<Instant, String> {
    Instant::now()
        .checked_add(duration)
        .ok_or_else(|| "a deadline".to_owned())
}

/// A pipe name for this user that no other test or run uses.
fn unique(what: &str) -> Result<String, String> {
    name(&format!("fuxix-test-{what}-{}", std::process::id())).map_err(text)
}

/// Waits until `source` has something to say (to read, its end, or an
/// error the next call gives), or fails at `deadline`.
fn ready(source: &impl Source, deadline: Instant) -> Outcome {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("nothing came in time".into());
        }
        let mut fds = [PollFd::new(source, Events::IN)];
        if poll(&mut fds, Some(left)).map_err(text)? > 0 {
            return Ok(());
        }
    }
}

/// Writes all of `bytes` to `stream`, polling for room.
fn send(stream: &Stream, bytes: &[u8], deadline: Instant) -> Outcome {
    let mut sent = 0usize;
    while let Some(rest) = bytes.get(sent..).filter(|rest| !rest.is_empty()) {
        sent = sent.saturating_add(write_what_fits(stream, rest).map_err(text)?);
        if Instant::now() > deadline {
            return Err("a write took too long".into());
        }
        let mut fds = [PollFd::new(stream, Events::OUT)];
        poll(&mut fds, Some(Duration::from_millis(50))).map_err(text)?;
    }
    Ok(())
}

/// Reads one line (without its `\n`) from `stream`; `None` at its end.
fn line(stream: &Stream, deadline: Instant) -> Result<Option<String>, String> {
    let mut got = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match read(stream, &mut byte) {
            Ok(0) if got.is_empty() => return Ok(None),
            Ok(0) => return Err("the stream ended in a line".into()),
            Ok(_) if byte == *b"\n" => return String::from_utf8(got).map(Some).map_err(text),
            Ok(_) => got.extend_from_slice(&byte),
            Err(Errno::AGAIN) => ready(stream, deadline)?,
            Err(errno) => return Err(errno.to_string()),
        }
    }
}

fn our_pid() -> Option<fuxix::process::Pid> {
    fuxix::process::Pid::from_raw(std::process::id())
}

/// A child that is killed and waited for when dropped, should a test fail.
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// This binary's ignored test `test`, with `variable` set to `value`.
fn child(test: &str, variable: &str, value: &str) -> Result<Command, String> {
    let mut command = Command::new(std::env::current_exe().map_err(text)?);
    command
        .args([test, "--exact", "--ignored", "--nocapture", "--quiet"])
        .env(variable, value)
        .stderr(Stdio::inherit());
    Ok(command)
}

/// A client of the same user connects: both ends read the other as this
/// user and this process, and bytes go both ways.
#[test]
fn the_same_user_connects_and_talks_both_ways() -> Outcome {
    let _turn = turn();
    let deadline = after(Duration::from_secs(20))?;
    let name = unique("same")?;
    let mut listener = Listener::bind(&name).map_err(text)?;
    assert_eq!(listener.accept().err(), Some(Errno::AGAIN));
    let client = connect(&name).map_err(text)?;
    ready(&listener, deadline)?;
    let served = listener.accept().map_err(text)?;
    let theirs = peer(&served).map_err(text)?;
    assert!(theirs.user.is_current(), "{:?}", theirs.user);
    assert_eq!(theirs.pid, our_pid());
    let served = served.stream().map_err(text)?;
    let ours = peer(&client).map_err(text)?;
    assert_eq!((ours.user, ours.pid), (theirs.user, our_pid()));
    send(&client, b"ping\n", deadline)?;
    assert_eq!(line(&served, deadline)?.as_deref(), Some("ping"));
    send(&served, b"pong\n", deadline)?;
    assert_eq!(line(&client, deadline)?.as_deref(), Some("pong"));
    // The listener made its next instance first: another client gets in.
    let second = connect(&name).map_err(text)?;
    ready(&listener, deadline)?;
    let _second = (second, listener.accept().map_err(text)?);
    say(&format!(
        "PIPES same-user peer={} pid={:?} both ways ok",
        theirs.user, theirs.pid
    ));
    Ok(())
}

const SQUAT: &str = "FUXIX_TEST_SQUAT";

/// The child of [`a_name_taken_first_is_told_apart`]: takes the name
/// first, with the default security and no wish to be first, says so,
/// and holds it till its stdin closes.
#[test]
#[ignore = "the child of a_name_taken_first_is_told_apart"]
fn squatter_holds_a_name() -> Outcome {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::CreateNamedPipeW;
    let Some(name) = std::env::var_os(SQUAT) else {
        return Ok(());
    };
    let wide: Vec<u16> = name.to_string_lossy().encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is NUL-ended; the default security, not inherited.
    let raw = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            0,
            255,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: `raw` was just made, and nothing else owns it.
    let _held =
        unsafe { <OwnedHandle as std::os::windows::io::FromRawHandle>::from_raw_handle(raw) };
    writeln!(std::io::stdout(), "held").map_err(text)?;
    std::io::stdout().flush().map_err(text)?;
    let _ = std::io::stdin().lines().count();
    Ok(())
}

/// Another process holds the name first: binding it fails with
/// `ADDRINUSE`, never joining that process's pipe.
#[test]
fn a_name_taken_first_is_told_apart() -> Outcome {
    let name = unique("squat")?;
    let mut command = child("squatter_holds_a_name", SQUAT, &name)?;
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut squatter = Child(command.spawn().map_err(text)?);
    // Kept open till it exits: its test harness still writes there.
    let mut out = std::io::BufReader::new(squatter.0.stdout.take().ok_or("its stdout")?);
    let mut said = String::new();
    while said.trim() != "held" {
        said.clear();
        if out.read_line(&mut said).map_err(text)? == 0 {
            return Err("the squatter never held the name".into());
        }
    }
    let bound = Listener::bind(&name).map(drop);
    assert_eq!(bound, Err(Errno::ADDRINUSE));
    say(&format!("PIPES squat bind={bound:?}"));
    drop(squatter.0.stdin.take());
    // Once it is gone, the name is free again.
    let status = squatter.0.wait().map_err(text)?;
    drop(out);
    assert!(status.success(), "{status:?}");
    Listener::bind(&name).map_err(text)?;
    Ok(())
}

const SPARE: &str = "FUXIX_TEST_SPARE";

/// The child of [`a_child_given_a_spare_takes_its_client`]: takes a client
/// on the spare it was given as its stdin, reads it as its parent's user,
/// and answers each line with its own pid.
#[test]
#[ignore = "the child of a_child_given_a_spare_takes_its_client"]
fn child_takes_a_client_on_its_spare() -> Outcome {
    if std::env::var_os(SPARE).is_none() {
        return Ok(());
    }
    let _turn = turn();
    let deadline = after(Duration::from_secs(20))?;
    let handle = std::io::stdin().as_handle().try_clone_to_owned();
    let mut spare = Spare::try_from(handle.map_err(text)?).map_err(text)?;
    ready(&spare, deadline)?;
    let connection = spare.accept().map_err(text)?;
    if !peer(&connection).map_err(text)?.user.is_current() {
        return Err("a stranger".into());
    }
    let stream = connection.stream().map_err(text)?;
    while let Some(line) = line(&stream, deadline)? {
        let answer = format!(
            "{} from {}\n",
            line.replace("ping", "pong"),
            std::process::id()
        );
        send(&stream, answer.as_bytes(), deadline)?;
    }
    Ok(())
}

/// A child given the listener's spare instance takes the client that
/// comes and talks to it, while the client still reads the pipe's server
/// as the parent, who made the instance.
#[test]
fn a_child_given_a_spare_takes_its_client() -> Outcome {
    let _turn = turn();
    let deadline = after(Duration::from_secs(25))?;
    let name = unique("spare")?;
    let mut listener = Listener::bind(&name).map_err(text)?;
    let spare = OwnedHandle::try_from(listener.spare().map_err(text)?).map_err(text)?;
    let mut command = child("child_takes_a_client_on_its_spare", SPARE, "1")?;
    command.stdin(Stdio::from(spare)).stdout(Stdio::null());
    let mut worker = Child(command.spawn().map_err(text)?);
    drop(command);
    // Only the child's instance is left to connect to.
    drop(listener);
    let client = connect(&name).map_err(text)?;
    let server = peer(&client).map_err(text)?;
    assert_eq!(server.pid, our_pid(), "the server is the parent");
    for i in 0..3 {
        send(&client, format!("ping {i}\n").as_bytes(), deadline)?;
        let want = format!("pong {i} from {}", worker.0.id());
        assert_eq!(line(&client, deadline)?, Some(want));
    }
    drop(client);
    let status = worker.0.wait().map_err(text)?;
    assert!(status.success(), "{status:?}");
    say(&format!(
        "PIPES handoff spare worker={} server_pid_seen={:?} ok",
        worker.0.id(),
        server.pid
    ));
    Ok(())
}

/// What [`another_user_is_refused`] needs: a standard user's name and
/// password, and a folder that user may read and write, which CI makes.
const OTHER_USER: &str = "FUXIX_OTHER_USER";
const OTHER_PASSWORD: &str = "FUXIX_OTHER_PASSWORD";
const OTHER_DIR: &str = "FUXIX_OTHER_DIR";

/// The other user's side of [`another_user_is_refused`], run as that user
/// from a copy of this binary in the shared folder: reads the pipe's name
/// there, tries to connect and to read alone, and writes what happened.
#[test]
#[ignore = "the other user's side of another_user_is_refused"]
fn another_users_side() -> Outcome {
    use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
    let exe = std::env::current_exe().map_err(text)?;
    let dir = exe.parent().ok_or("its folder")?;
    let Ok(name) = std::fs::read_to_string(dir.join("pipe-name")) else {
        return Ok(());
    };
    let user = User::current().map_err(text)?;
    let connected = connect(&name).map(drop);
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is NUL-ended; opened for reading, not shared.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    let read = match raw == INVALID_HANDLE_VALUE {
        true => Err(std::io::Error::last_os_error().raw_os_error()),
        false => {
            // SAFETY: `raw` was just opened, and nothing else owns it.
            drop(unsafe {
                <OwnedHandle as std::os::windows::io::FromRawHandle>::from_raw_handle(raw)
            });
            Ok("opened")
        }
    };
    let report = format!("user={user} connect={connected:?} read_only={read:?}\n");
    std::fs::write(dir.join("other-result"), report).map_err(text)?;
    Ok(())
}

/// A standard second user can't open the pipe, to read and write or to
/// read alone: the owner-only descriptor refuses it with `ACCES`, and the
/// server sees no client.
#[test]
fn another_user_is_refused() -> Outcome {
    use windows_sys::Win32::Foundation::{STILL_ACTIVE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CreateProcessWithLogonW, GetExitCodeProcess, LOGON_WITH_PROFILE,
        PROCESS_INFORMATION, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
    };
    let (Some(user), Some(password), Some(dir)) = (
        std::env::var_os(OTHER_USER),
        std::env::var_os(OTHER_PASSWORD),
        std::env::var_os(OTHER_DIR),
    ) else {
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return Err(format!(
                "CI must set {OTHER_USER}, {OTHER_PASSWORD} and {OTHER_DIR}"
            ));
        }
        say("PIPES other-user skipped: no second user here");
        return Ok(());
    };
    let _turn = turn();
    let dir = std::path::PathBuf::from(dir);
    let name = unique("other")?;
    let mut listener = Listener::bind(&name).map_err(text)?;
    let exe = dir.join(format!("fuxix-pipes-{}.exe", std::process::id()));
    std::fs::copy(std::env::current_exe().map_err(text)?, &exe).map_err(text)?;
    let _ = std::fs::remove_file(dir.join("other-result"));
    std::fs::write(dir.join("pipe-name"), &name).map_err(text)?;
    let wide = |s: &std::ffi::OsStr| -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        s.encode_wide().chain([0]).collect()
    };
    let line = format!(
        "\"{}\" another_users_side --exact --ignored --quiet",
        exe.display()
    );
    let mut line = wide(line.as_ref());
    let (user, domain, password, cwd) = (
        wide(&user),
        wide(".".as_ref()),
        wide(&password),
        wide(dir.as_os_str()),
    );
    let startup = STARTUPINFOW {
        cb: u32::try_from(size_of::<STARTUPINFOW>()).map_err(text)?,
        ..STARTUPINFOW::default()
    };
    let mut started = PROCESS_INFORMATION::default();
    // SAFETY: every string is NUL-ended and alive for the call, `line`
    // writable; `startup` is read and `started` written.
    let made = unsafe {
        CreateProcessWithLogonW(
            user.as_ptr(),
            domain.as_ptr(),
            password.as_ptr(),
            LOGON_WITH_PROFILE,
            std::ptr::null(),
            line.as_mut_ptr(),
            CREATE_NO_WINDOW,
            std::ptr::null(),
            cwd.as_ptr(),
            &startup,
            &mut started,
        )
    };
    if made == 0 {
        return Err(format!(
            "running as the other user: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: just made for us, and nothing else owns it.
    let process = unsafe {
        <OwnedHandle as std::os::windows::io::FromRawHandle>::from_raw_handle(started.hProcess)
    };
    // SAFETY: as above.
    let _thread = unsafe {
        <OwnedHandle as std::os::windows::io::FromRawHandle>::from_raw_handle(started.hThread)
    };
    // SAFETY: waits on the process handle we own, at most 25 s.
    let waited = unsafe { WaitForSingleObject(process.as_raw_handle(), 25_000) };
    let mut code = STILL_ACTIVE.cast_unsigned();
    // SAFETY: the handle is ours; `code` is written once.
    unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) };
    if waited != WAIT_OBJECT_0 {
        // SAFETY: ends the process we started, on its own handle.
        unsafe { TerminateProcess(process.as_raw_handle(), 1) };
        return Err("the other user's side took over 25 s".into());
    }
    let report = std::fs::read_to_string(dir.join("other-result")).map_err(text)?;
    let _ = std::fs::remove_file(&exe);
    say(&format!("PIPES other-user exit={code} {}", report.trim()));
    assert_eq!(code, 0, "the other user's side failed: {report}");
    assert!(!report.contains(&User::current().map_err(text)?.to_string()));
    assert!(report.contains("connect=Err(Errno(5:"), "{report}");
    assert!(report.contains("read_only=Err(Some(5))"), "{report}");
    assert_eq!(listener.accept().err(), Some(Errno::AGAIN));
    Ok(())
}
