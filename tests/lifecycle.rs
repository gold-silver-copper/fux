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
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;
use support::*;

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

/// A server started outside the test harness's own setup: no `FUX_SOCKET`,
/// no `set shell`, and the environment given.
struct Bare {
    dir: PathBuf,
    child: Option<Child>,
    env: Vec<(String, String)>,
}

impl Bare {
    fn start(name: &str, env: &[(&str, String)], remove: &[&str]) -> Result<Bare, String> {
        let dir = short_temp_dir()?.join(format!("fux-{name}-{}", std::process::id()));
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
        let mut command = Command::new(FUX);
        command
            .arg("server")
            .env_remove("FUX_SOCKET")
            .env_remove("FUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for variable in remove {
            command.env_remove(variable);
        }
        for (k, v) in &env {
            command.env(k, v);
        }
        let child = {
            let _guard = spawning()?;
            command.spawn().map_err(e)?
        };
        let bare = Bare {
            dir,
            child: Some(child),
            env,
        };
        eventually("the server to answer", || {
            Ok(bare.fux(&["ls"], remove)?.status.success())
        })?;
        Ok(bare)
    }

    fn fux(&self, args: &[&str], remove: &[&str]) -> Result<Output, String> {
        let mut command = Command::new(FUX);
        command
            .args(args)
            .env_remove("FUX_SOCKET")
            .env_remove("FUX_PANE");
        for variable in remove {
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
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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
    let base = short_temp_dir()?.join(format!("fux-default-{}", std::process::id()));
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
        assert!(server.fux(&["kill-server"], remove)?.status.success());
        eventually("the socket to go", || Ok(!socket.exists()))?;
    }
    let _ = std::fs::remove_dir_all(&base);
    Ok(())
}

/// With no `set shell`, a pane runs `$SHELL`, and with no `$SHELL`,
/// `/bin/sh`. Each server has a socket of its own (never the default, which
/// a real server may hold).
#[test]
fn a_pane_runs_shell_else_bin_sh() -> Outcome {
    for (shell, name) in [(Some("/bin/dash"), "dash"), (None, "sh")] {
        let dir = short_temp_dir()?.join(format!("fux-shell-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(e)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(e)?;
        }
        let socket = dir.join("fux.sock");
        let mut command = Command::new(FUX);
        command
            .args(["server", "--socket"])
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("HOME", &dir)
            .env("ENV", "/dev/null")
            .env_remove("FUX_PANE")
            .env_remove("SHELL");
        if let Some(shell) = shell {
            command.env("SHELL", shell);
        }
        let mut child = command.spawn().map_err(e)?;
        let ls = || {
            Command::new(FUX)
                .arg("ls")
                .env("FUX_SOCKET", &socket)
                .output()
                .map_err(e)
        };
        let shown = eventually(&format!("a pane running {name}"), || {
            Ok(String::from_utf8_lossy(&ls()?.stdout).contains(&format!("%1 {name} ")))
        });
        let _ = Command::new(FUX)
            .arg("kill-server")
            .env("FUX_SOCKET", &socket)
            .output();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        shown?;
    }
    Ok(())
}

/// A client that stops reading while a pane floods it gets nothing more
/// queued once its share is full, and when it reads again it is brought up
/// to date: its screen ends as the server composes it.
#[test]
fn a_client_that_stops_reading_catches_up_when_it_reads_again() -> Outcome {
    // Little history, so the memory measured is what waits for the client.
    let server = Server::start("set history-lines 100")?;
    let pid = server.pid().ok_or("the server's pid")?;
    let mut client = server.attach(60, 200)?;
    client.wait_for("$")?;
    // Every frame different, and the whole screen: far more paint than a
    // client may have waiting.
    server.type_line(
        "%1",
        "i=0; while :; do i=$((i+1)); printf '%0199d\\n' $i; done",
    )?;
    // Not read for three seconds.
    std::thread::sleep(Duration::from_secs(3));
    let memory = resident_mib(pid)?;
    server.ok(&["send-keys", "-t", "%1", "C-c"])?;
    server.type_line("%1", "echo caught-up")?;
    eventually("the client to show what the server composes for it", || {
        client.pump()?;
        let composed = server.ok(&["capture-client", "-c", "c1"])?;
        Ok(composed.contains("caught-up") && client.text().trim_end() == composed.trim_end())
    })?;
    // What was waiting for it stayed bounded (4 MiB, and the paint being
    // built).
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
    let server = Server::start("set history-lines 2000")?;
    let mut client = server.attach(20, 120)?;
    client.wait_for("$")?;
    let copy_all = |client: &mut Client| -> Outcome {
        server.ok(&["copy-mode", "-c", "c1"])?;
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
    server.ok(&["send-keys", "-t", "%1", "clear", "Enter"])?;
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
    let buffer = server.ok(&["show-buffer"])?;
    assert!(
        buffer.len() > 1 << 20,
        "the buffer holds it all: {} bytes",
        buffer.len()
    );
    Ok(())
}
