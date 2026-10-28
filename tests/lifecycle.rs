//! What README promises about a server's life that the other tests leave
//! out: stopping on a signal, the default socket, the shell a pane runs
//! when none is set, a client that stops reading, and the clipboard's cap.
//!
//! The peer check (README, "Security": only the server's own user may
//! connect) needs a second user, so it is verified by hand: `sudo -u
//! nobody fux ls` against a running server is refused.
mod support;
use fuxix::process::Signal;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use support::*;

/// Types a line into a pane once its shell shows a prompt, answering at
/// once.
fn type_now(server: &Server, pane: &str, line: &str) -> Outcome {
    eventually(&format!("a prompt in {pane}"), || {
        Ok(server.ok(&["str", pane, "pane", "capture"])?.contains('$'))
    })?;
    server.ok(&[
        "str",
        pane,
        "pane",
        "str",
        line,
        "text",
        "str",
        "Enter",
        "key",
        "send-keys",
    ])?;
    Ok(())
}

/// SIGTERM, SIGINT and SIGHUP each stop a server: every attached client is
/// told why, and the socket is removed.
#[test]
fn a_signal_stops_the_server_telling_clients_why_and_removing_its_socket() -> Outcome {
    for sig in [Signal::Term, Signal::Int, Signal::Hup] {
        let mut server = Server::start("")?;
        let pid = server.pid().ok_or("the server's pid")?;
        let mut client = server.attach(10, 40)?;
        client.wait_for("$")?;
        signal(i32::try_from(pid).map_err(e)?, sig);
        let reason = client.wait_exit()?;
        assert!(
            reason.contains("stopped by a signal"),
            "{sig:?}: the client was told {reason:?}"
        );
        let status = server.wait_exit()?;
        assert!(status.success(), "{sig:?}: the server exited {status}");
        assert!(
            !server.socket.exists(),
            "{sig:?}: the socket was left at {}",
            server.socket.display()
        );
    }
    Ok(())
}

/// A server started outside the test harness's own setup, by `fux start`:
/// no `FUX_SOCKET`, no `set shell`, and the environment given.
struct Bare {
    dir: PathBuf,
    env: Vec<(String, String)>,
    remove: Vec<String>,
}

impl Bare {
    fn start(name: &str, env: &[(&str, String)], remove: &[&str]) -> Result<Bare, String> {
        let dir = fuxix::file::scratch()
            .map_err(e)?
            .join(format!("fux-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(e)?;
        let mut env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        env.extend([
            ("HOME".to_owned(), dir.display().to_string()),
            ("ENV".to_owned(), "/dev/null".to_owned()),
            ("PS1".to_owned(), "$ ".to_owned()),
        ]);
        let bare = Bare {
            dir,
            env,
            remove: remove.iter().map(|v| (*v).to_owned()).collect(),
        };
        let started = {
            let _guard = spawning()?;
            bare.fux(&["start"])?
        };
        if !started.status.success() {
            return Err(String::from_utf8_lossy(&started.stderr).into_owned());
        }
        Ok(bare)
    }

    fn fux(&self, args: &[&str]) -> Result<Output, String> {
        let mut command = Command::new(FUX);
        command
            .args(args)
            .env_remove("FUX_SOCKET")
            .env_remove("FUX_PANE")
            .stdin(Stdio::null());
        for variable in &self.remove {
            command.env_remove(variable);
        }
        for (k, v) in &self.env {
            command.env(k, v);
        }
        command.output().map_err(e)
    }
}

impl Drop for Bare {
    fn drop(&mut self) {
        let _ = self.fux(&["force-kill-server"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn mode_of(path: &Path) -> Result<u32, String> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path).map_err(e)?.permissions().mode() & 0o777)
}

/// The environment set, the variables removed, and where the socket goes.
type Case<'a> = (&'a [(&'a str, String)], &'a [&'a str], &'a Path);

/// With no FUX_SOCKET, the socket is `$XDG_RUNTIME_DIR/fux/server.sock`,
/// else `$TMPDIR/fux/server.sock`; the `fux` directory is made private.
#[test]
fn the_default_socket_is_under_xdg_runtime_dir_else_tmpdir() -> Outcome {
    let base = fuxix::file::scratch()
        .map_err(e)?
        .join(format!("fux-default-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for sub in ["run", "tmp"] {
        std::fs::create_dir_all(base.join(sub)).map_err(e)?;
    }
    let run = base.join("run");
    let tmp = base.join("tmp");
    let cases: [Case<'_>; 2] = [
        (
            &[
                ("XDG_RUNTIME_DIR", run.display().to_string()),
                ("TMPDIR", tmp.display().to_string()),
            ],
            &[],
            &run,
        ),
        (
            &[("TMPDIR", tmp.display().to_string())],
            &["XDG_RUNTIME_DIR"],
            &tmp,
        ),
    ];
    for (env, remove, expected) in cases {
        let server = Bare::start("default-socket", env, remove)?;
        let socket = expected.join("fux").join("server.sock");
        assert!(socket.exists(), "no socket at {}", socket.display());
        assert_eq!(
            mode_of(&expected.join("fux"))?,
            0o700,
            "{}",
            expected.display()
        );
        assert!(server.fux(&["kill-server"])?.status.success());
        eventually("the socket to go", || Ok(!socket.exists()))?;
    }
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

/// The login shell `/etc/passwd` gives this user, if it gives one.
fn login_shell() -> Option<String> {
    let euid = fuxix::process::geteuid().to_string();
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.get(2) == Some(&euid.as_str()))
            .then(|| fields.get(6).map(|s| s.trim().to_owned()))
            .flatten()
            .filter(|s| !s.is_empty())
    })
}

/// With no `set shell`, a pane runs `$SHELL`; with no `$SHELL`, the user's
/// login shell from `/etc/passwd`, and with none there, `/bin/sh`. Each
/// server has a socket of its own (never the default, which a real server
/// may hold).
#[test]
fn a_pane_runs_shell_else_the_login_shell_else_bin_sh() -> Outcome {
    let unset = login_shell().unwrap_or_else(|| "/bin/sh".into());
    for (shell, expected) in [(Some("/bin/bash"), "/bin/bash"), (None, unset.as_str())] {
        let name = Path::new(expected)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("the login shell's name")?;
        let dir = fuxix::file::scratch()
            .map_err(e)?
            .join(format!("fux-shell-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(e)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
        }
        let socket = dir.join("fux.sock");
        let mut command = Command::new(FUX);
        command
            .arg(fux::SERVE)
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("HOME", &dir)
            .env("ENV", "/dev/null")
            .env("PS1", "$ ")
            .env_remove("FUX_PANE")
            .env_remove("SHELL");
        if let Some(shell) = shell {
            command.env("SHELL", shell);
        }
        let mut child = command.spawn().map_err(e)?;
        let capture = || {
            Command::new(FUX)
                .args(["%1", "capture"])
                .env("FUX_SOCKET", &socket)
                .output()
                .map_err(e)
        };
        // A server starts with nothing in it: its first shell, once it
        // answers.
        eventually("the first shell made", || {
            let made = Command::new(FUX)
                .args(["opts", "80", "24", "size", "new-workspace"])
                .env("FUX_SOCKET", &socket)
                .env_remove("FUX_PANE")
                .output()
                .map_err(e)?;
            Ok(made.status.success())
        })?;
        // Ask the shell for its argv[0]; OS process names may resolve
        // an executable's symlink, and macOS does not report its argv.
        let shown = (|| {
            eventually("the shell's prompt", || {
                let observed = capture()?;
                Ok(observed.status.success()
                    && String::from_utf8_lossy(&observed.stdout).contains('$'))
            })?;
            let typed = Command::new(FUX)
                .args([
                    "%1",
                    "str",
                    r#"printf '\nFUX-SHELL:%s\n' "$0""#,
                    "text",
                    "str",
                    "Enter",
                    "key",
                    "send-keys",
                ])
                .env("FUX_SOCKET", &socket)
                .output()
                .map_err(e)?;
            if !typed.status.success() {
                return Err(String::from_utf8_lossy(&typed.stderr).into_owned());
            }
            let expected = format!("FUX-SHELL:{expected}");
            eventually(&format!("a pane running {name}"), || {
                let observed = capture()?;
                Ok(observed.status.success()
                    && String::from_utf8_lossy(&observed.stdout)
                        .lines()
                        .any(|line| line == expected))
            })
        })();
        let _ = Command::new(FUX)
            .args(["force-kill-server"])
            .env("FUX_SOCKET", &socket)
            .output();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        shown?;
    }
    Ok(())
}

/// A client that stops reading while a pane floods it gets no frame past
/// the one in flight, and when it reads again it is brought up to date: its
/// screen ends as the server composes it.
#[test]
fn a_client_that_stops_reading_catches_up_when_it_reads_again() -> Outcome {
    // Little history, so the memory measured is what waits for the client.
    let server = Server::start("str history-bytes 160000 set")?;
    let pid = server.pid().ok_or("the server's pid")?;
    let mut client = server.attach(60, 200)?;
    client.wait_for("$")?;
    // Every frame different, and the whole screen: far more paint than a
    // client may have waiting.
    type_now(
        &server,
        "%1",
        "i=0; while :; do i=$((i+1)); printf '%0199d\\n' $i; done",
    )?;
    // Not read for three seconds.
    std::thread::sleep(Duration::from_secs(3));
    let memory = resident_mib(pid)?;
    server.ok(&["str", "%1", "pane", "str", "C-c", "press"])?;
    server.type_line("%1", "echo caught-up")?;
    eventually("the client to show what the server composes for it", || {
        client.pump()?;
        let composed = server.ok(&["str", "c1", "client", "capture-client"])?;
        Ok(composed.contains("caught-up") && client.text().trim_end() == composed.trim_end())
    })?;
    // What was waiting for it stayed bounded: one frame.
    assert!(
        memory < 64.0,
        "{memory:.0} MiB resident while it did not read"
    );
    Ok(())
}

/// A copy goes to the client's clipboard as OSC 52 unless it is over 1 MiB
/// encoded; then only to the paste buffer, saying so.
#[test]
fn the_clipboard_takes_a_copy_up_to_a_mebibyte() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(20, 120)?;
    client.wait_for("$")?;
    let copy_all = |client: &mut Client| -> Outcome {
        server.ok(&["str", "c1", "client", "copy-mode"])?;
        // Its keys are copy mode's once the client is told it is in it.
        client.wait("copy mode", |t| {
            t.lines().last().is_some_and(|b| b.contains("COPY"))
        })?;
        client.keys("tszy")?;
        Ok(())
    };
    // Small: on the clipboard.
    server.type_line("%1", "echo small-copy")?;
    client.wait("the line", |t| t.lines().any(|l| l == "small-copy"))?;
    copy_all(&mut client)?;
    client.wait("the copy", |t| {
        t.lines().last().is_some_and(|b| b.contains("copied"))
    })?;
    let painted = String::from_utf8_lossy(&client.painted).into_owned();
    assert!(painted.contains("\x1b]52;c;"), "an OSC 52 write");
    let before = client.painted.len();
    // Large: 1200 lines of 50 family emoji, 25 bytes each, about 2 MiB
    // encoded, in fewer cells than a copy may take.
    server.type_line("%1", "clear")?;
    server.type_line(
        "%1",
        "i=0; while [ $i -lt 1200 ]; do i=$((i+1)); printf '\\360\\237\\221\\250\\342\\200\\215\\360\\237\\221\\251\\342\\200\\215\\360\\237\\221\\247\\342\\200\\215\\360\\237\\221\\246%.0s' $(seq 50); echo; done; echo done-printing",
    )?;
    client.wait("the output", |t| t.lines().any(|l| l == "done-printing"))?;
    copy_all(&mut client)?;
    client.wait("the copy", |t| {
        t.lines()
            .last()
            .is_some_and(|b| b.contains("too large for the clipboard"))
    })?;
    let after =
        String::from_utf8_lossy(client.painted.get(before..).unwrap_or_default()).into_owned();
    assert!(!after.contains("\x1b]52;"), "no OSC 52 write for it");
    let buffer = server.ok(&["newest", "buffer"])?;
    assert!(
        buffer.len() > 1 << 20,
        "the buffer holds it all: {} bytes",
        buffer.len()
    );
    Ok(())
}
