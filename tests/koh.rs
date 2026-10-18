//! koh's client driven in a fux pane, against a `koh serve --local` on
//! loopback: a remote pager, prompt, slow command and REPL answer as local
//! ones do, from the state koh reports (`OSC 133 ; S`) and its relayed
//! marks. It needs a koh binary: `FUX_KOH=path/to/koh cargo test --test koh
//! -- --ignored` (CI builds koh at a pinned commit).
mod support;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use support::*;

/// A `koh serve`, stopped by its own pid when dropped.
struct Serve(Child);

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `koh ARGS` as the user whose home is `home`.
fn koh(bin: &str, home: &Path) -> Command {
    let mut command = Command::new(bin);
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("SHELL", "/bin/bash")
        .env_remove("FUX_PANE")
        .env_remove("FUX_SERVER")
        .env_remove("FUX_SOCKET")
        .env_remove("FUX_MARK_NONCE");
    command
}

/// A `koh serve --local` for the client whose home is `local`, serving
/// bash in `remote`; and its endpoint id and port, from what it prints
/// (on stderr).
fn serve(bin: &str, local: &Path, remote: &Path) -> Result<(Serve, String, String), String> {
    let id = koh(bin, local).arg("id").output().map_err(e)?;
    let client = String::from_utf8_lossy(&id.stdout).trim().to_owned();
    let said = remote.join("serve.out");
    let out = std::fs::File::create(&said).map_err(e)?;
    let errors = out.try_clone().map_err(e)?;
    let child = koh(bin, remote)
        .args(["serve", "--local", "--allow", &client, "--shell", "bash"])
        .current_dir(remote)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(errors)
        .spawn()
        .map_err(e)?;
    let serve = Serve(child);
    let deadline = after(PATIENCE);
    loop {
        let text = std::fs::read_to_string(&said).unwrap_or_default();
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.split_once(name)?.1.split_once(':'))
                .map(|(_, v)| v.trim().to_owned())
        };
        let port = field("udp port").and_then(|p| Some(p.split_whitespace().next()?.to_owned()));
        if let (Some(id), Some(port)) = (field("endpoint id"), port) {
            return Ok((serve, id, port));
        }
        if Instant::now() > deadline {
            return Err(format!("koh serve did not say its id: {text}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `args` against `server`, its answer, which must come within `limit`.
fn within(server: &Server, limit: u64, args: &[&str]) -> Result<String, String> {
    let began = Instant::now();
    let answer = server.ok(args)?;
    let took = began.elapsed();
    if took > Duration::from_secs(limit) {
        return Err(format!("{args:?} took {took:?}: {answer}"));
    }
    Ok(answer)
}

/// A clock on the alternate screen: a frame each 0.3 s, polling its
/// terminal between them; a key shows, `q` quits, the terminal's modes put
/// back (else the shell keeps them for the next command).
const CLOCK: &str = "import os, select, termios, tty\n\
    old = termios.tcgetattr(0); tty.setraw(0); os.write(1, b'\\x1b[?1049h'); n = 0; got = b'-'\n\
    while got != b'q':\n\
    \x20   n += 1\n\
    \x20   os.write(1, b'\\x1b[H\\x1b[2Jtick %d\\r\\ngot %s' % (n, got))\n\
    \x20   if select.select([0], [], [], 0.3)[0]: got = os.read(0, 1)\n\
    os.write(1, b'\\x1b[?1049l'); termios.tcsetattr(0, termios.TCSADRAIN, old)\n";

#[test]
#[ignore = "needs koh: FUX_KOH=path/to/koh cargo test --test koh -- --ignored"]
fn a_remote_shell_through_koh_answers_as_a_local_one() -> Outcome {
    let bin = std::env::var("FUX_KOH").map_err(|_| "FUX_KOH names no koh binary")?;
    let server = Server::start("")?;
    let (local, remote) = (server.dir.join("koh-local"), server.dir.join("koh-remote"));
    std::fs::create_dir_all(&local).map_err(e)?;
    std::fs::create_dir_all(&remote).map_err(e)?;
    std::fs::write(remote.join(".bashrc"), "PS1='remote> '\n").map_err(e)?;
    std::fs::write(remote.join("n.txt"), "one\ntwo\nthree\n").map_err(e)?;
    let (_serve, id, port) = serve(&bin, &local, &remote)?;
    let connect = format!("{bin} connect {id} --direct 127.0.0.1:{port}");
    let home = local.to_string_lossy().into_owned();
    let config = local.join(".config").to_string_lossy().into_owned();
    let env = |k, v| ["str", k, "str", v, "env"];
    let started = server.ok(&[
        &["opts"][..],
        &env("HOME", &home),
        &env("XDG_CONFIG_HOME", &config),
        &["str", &connect, "command", "run"],
    ]
    .concat())?;
    let pane = started
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let p = ["str", pane.as_str(), "pane"];
    let line =
        |text: &str, limit| within(&server, limit, &[&p[..], &["str", text, "line"]].concat());
    let press = |key: &str| within(&server, 5, &[&p[..], &["str", key, "press"]].concat());
    // The remote prompt first: what was typed before it would be lost.
    server.ok(&[&p[..], &["opts", "str", "remote>", "wait-text", "wait"]].concat())?;

    let less = line("less n.txt", 5)?;
    assert!(less.starts_with(&format!("{pane} reading")), "{less}");
    assert!(less.contains("three"), "{less}");
    let down = press("Down")?;
    assert!(down.starts_with(&format!("{pane} reading")), "{down}");
    let quit = press("q")?;
    assert!(
        quit.starts_with(&format!("{pane} reading · done 0")),
        "{quit}"
    );

    let asked = line("read -p \"name? \" n; echo hi $n", 5)?;
    assert!(asked.starts_with(&format!("{pane} reading")), "{asked}");
    assert!(asked.contains("name?"), "{asked}");
    let hi = line("bob", 5)?;
    assert!(hi.starts_with(&format!("{pane} reading · done 0")), "{hi}");
    assert!(hi.contains("hi bob\n"), "{hi}");

    let slow = line("sleep 2; echo slow; (exit 3)", 15)?;
    assert!(
        slow.starts_with(&format!("{pane} reading · done 3")),
        "{slow}"
    );
    let output = server.ok(&[&p[..], &["opts", "last-output", "capture"]].concat())?;
    assert_eq!(output.lines().nth(1), Some("slow"), "{output}");

    // A remote full-screen program that redraws on its own (`top`-like):
    // its keys are answered once read and drawn after, not at the limit.
    std::fs::write(remote.join("clock.py"), CLOCK).map_err(e)?;
    let clock = line("python3 clock.py", 10)?;
    assert!(clock.contains("redraws on its own"), "{clock}");
    let x = within(&server, 5, &[&p[..], &["str", "x", "type"]].concat())?;
    assert!(x.contains("redraws on its own"), "{x}");
    assert!(x.contains("\ngot x"), "{x}");
    let quit = within(&server, 5, &[&p[..], &["str", "q", "type"]].concat())?;
    assert!(quit.contains("done 0"), "{quit}");

    let repl = line("python3 -i", 10)?;
    assert!(repl.contains(">>>"), "{repl}");
    assert!(repl.contains("command running"), "{repl}");
    let two = line("1+1", 10)?;
    assert!(two.contains("\n2\n"), "{two}");
    let out = line("exit()", 10)?;
    assert!(out.contains("done 0"), "{out}");
    Ok(())
}
