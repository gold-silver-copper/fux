//! The Unix-socket transport: location, permissions, lifecycle and wire.
use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    process::Output,
    sync::{Arc, atomic::AtomicBool},
};

/// A server started with arbitrary arguments and environment, for cases the
/// ordinary fixture does not cover. Removes nothing but its own process.
struct Spawned {
    child: Child,
    log: PathBuf,
}

impl Spawned {
    fn start(
        directory: &Path,
        socket: &Path,
        adjust: impl FnOnce(&mut Command),
    ) -> Result<Self, Fail> {
        let _spawn = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
        let log = directory.join(format!(
            "server-{}.log",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let config = directory.join("fux.json");
        if !config.exists() {
            fs::write(&config, r#"{"shell":["/bin/sh"],"history_lines":100}"#)?;
        }
        let mut command = server_command(
            directory,
            &[
                "--socket".as_ref(),
                socket.as_ref(),
                "--config".as_ref(),
                config.as_ref(),
            ],
        );
        command
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log)?);
        adjust(&mut command);
        Ok(Self {
            child: command.spawn()?,
            log,
        })
    }
    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
    /// Waits for the process to exit on its own and returns its status.
    fn exited(&mut self) -> Result<std::process::ExitStatus, Fail> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("server did not exit".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn answers(socket: &Path) -> bool {
    unix_http::agent(socket, Some(Duration::from_secs(2)))
        .post(unix_http::URL)
        .send_json(json!({"jsonrpc":"2.0","id":1,"method":"rpc.discover"}))
        .is_ok()
}

fn stop(socket: &Path) -> Result<(), Fail> {
    unix_http::agent(socket, Some(Duration::from_secs(5)))
        .post(unix_http::URL)
        .send_json(
            json!({"jsonrpc":"2.0","id":1,"method":"world.trigger_event",
            "params":{"event":"fux::control::Shutdown","value":null}}),
        )?;
    Ok(())
}

/// The internet sockets `pid` holds, from `lsof`. Inspection is proved to
/// work first, by finding the server's own Unix socket, so an empty answer is
/// evidence rather than a failed tool.
fn internet_sockets(pid: u32, socket: &Path) -> Result<String, Fail> {
    let unix = Command::new("lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-U"])
        .output()?;
    let listed = String::from_utf8_lossy(&unix.stdout);
    if !listed.contains(&*socket.to_string_lossy()) {
        return Err(format!("lsof could not see the server's own socket: {listed}").into());
    }
    let inet = Command::new("lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-i"])
        .output()?;
    // lsof exits 1 when it finds nothing to list; anything else must list rows.
    if !inet.status.success() && inet.status.code() != Some(1) {
        return Err(format!("lsof -i failed: {inet:?}").into());
    }
    Ok(String::from_utf8_lossy(&inet.stdout).into_owned())
}

/// Mirrors `transport::MAX_BODY`. An integration test cannot import from the
/// binary crate, so this is a copy: change it with the constant. The test
/// either side of the limit fails loudly if the two drift apart.
fn fux_transport_max_body() -> usize {
    4 << 20
}

fn mode(path: &Path) -> Result<u32, Fail> {
    Ok(fs::symlink_metadata(path)?.permissions().mode() & 0o7777)
}

#[test]
fn the_socket_is_private_under_a_permissive_umask_and_nothing_listens_on_tcp() -> Outcome {
    let (directory, socket) = fixture()?;
    let server = Spawned::start(&directory, &socket, |command| {
        // SAFETY: umask is async-signal-safe and touches only the child.
        unsafe {
            command.pre_exec(|| {
                nix::sys::stat::umask(nix::sys::stat::Mode::empty());
                Ok(())
            });
        }
    })?;
    eventually(|| Ok(answers(&socket)))?;
    assert_eq!(mode(socket.parent().need()?)?, 0o700);
    assert_eq!(mode(&socket)?, 0o600);
    // The regression test for hunt 5's finding 002 and its whole class: the
    // server process itself holds no TCP or UDP socket, IPv4 or IPv6.
    let inet = internet_sockets(server.child.id(), &socket)?;
    assert!(
        inet.trim().is_empty(),
        "server holds internet sockets:\n{inet}"
    );
    assert!(
        server
            .log()
            .contains(&format!("fux trusted BRP unix:{}", socket.display()))
    );
    stop(&socket)?;
    drop(server);
    fs::remove_dir_all(directory)?;
    Ok(())
}

fn fux(args: &[&str], env: &[(&str, &str)]) -> Result<Output, Fail> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fux"));
    command
        .args(args)
        .env_remove("FUX_ENDPOINT")
        .env_remove("FUX_SOCKET");
    for (key, value) in env {
        command.env(key, value);
    }
    Ok(command.output()?)
}

#[test]
fn retired_and_invalid_transport_settings_fail_naming_the_change() -> Outcome {
    let (directory, socket) = fixture()?;
    let path = socket.to_string_lossy().into_owned();
    let long = format!("/tmp/{}", "x".repeat(200));
    /// Arguments, environment, and the text stderr must contain.
    type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a str);
    let cases: [Case; 11] = [
        (
            &["rpc", "rpc.discover"],
            &[("FUX_ENDPOINT", "http://127.0.0.1:15702")],
            "FUX_ENDPOINT is no longer supported",
        ),
        (
            &["rpc", "rpc.discover"],
            &[("FUX_ENDPOINT", "x"), ("FUX_SOCKET", &path)],
            "FUX_ENDPOINT is no longer supported",
        ),
        (
            &["server"],
            &[("FUX_ENDPOINT", "http://127.0.0.1:15702")],
            "FUX_ENDPOINT is no longer supported",
        ),
        (
            &["rpc", "rpc.discover"],
            &[("FUX_SOCKET", "http://127.0.0.1:15702")],
            "looks like a URL",
        ),
        (
            &["rpc", "rpc.discover"],
            &[("FUX_SOCKET", "")],
            "FUX_SOCKET is empty",
        ),
        (
            &["rpc", "rpc.discover"],
            &[("FUX_SOCKET", "relative.sock")],
            "absolute path",
        ),
        (&["stop"], &[("FUX_SOCKET", &long)], "-byte limit"),
        (&["server", "--port", "15702"], &[], "--port was removed"),
        (
            &["server", "--address", "127.0.0.1"],
            &[],
            "--address was removed",
        ),
        (
            &["server", "--socket", "http://127.0.0.1:15702"],
            &[],
            "looks like a URL",
        ),
        (
            &["attach"],
            &[("FUX_SOCKET", &path)],
            "no fux server socket",
        ),
    ];
    for (args, env, needle) in cases {
        let output = fux(args, env)?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?} {env:?} succeeded");
        assert!(stderr.contains(needle), "{args:?} {env:?}: {stderr}");
    }
    // Help stays available whatever the environment says.
    let help = fux(&["help"], &[("FUX_ENDPOINT", "http://127.0.0.1:15702")])?;
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--socket PATH"));
    // Without XDG_RUNTIME_DIR, TMPDIR decides; an empty one is an error.
    let empty = Command::new(env!("CARGO_BIN_EXE_fux"))
        .args(["rpc", "rpc.discover"])
        .env_remove("FUX_SOCKET")
        .env_remove("FUX_ENDPOINT")
        .env_remove("XDG_RUNTIME_DIR")
        .env("TMPDIR", "")
        .output()?;
    assert!(String::from_utf8_lossy(&empty.stderr).contains("TMPDIR is set but empty"));
    // A location that is unsafe or occupied is refused and left as it was.
    let open = directory.join("open");
    fs::create_dir(&open)?;
    fs::set_permissions(&open, fs::Permissions::from_mode(0o755))?;
    let occupied = directory.join("private");
    fs::create_dir(&occupied)?;
    fs::set_permissions(&occupied, fs::Permissions::from_mode(0o700))?;
    fs::write(occupied.join("fux.sock"), "keep")?;
    // A symbolic link at the socket path, and one in place of its directory.
    std::os::unix::fs::symlink(occupied.join("fux.sock"), occupied.join("link.sock"))?;
    std::os::unix::fs::symlink(&occupied, directory.join("linked"))?;
    for (target, needle) in [
        (open.join("fux.sock"), "mode 0700"),
        (occupied.join("fux.sock"), "not a socket"),
        (occupied.join("link.sock"), "not a socket"),
        (directory.join("linked").join("fux.sock"), "symbolic link"),
    ] {
        let target = target.to_string_lossy().into_owned();
        let output = fux(&["server", "--socket", &target], &[])?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success() && stderr.contains(needle),
            "{stderr}"
        );
    }
    assert_eq!(mode(&open)?, 0o755);
    assert!(fs::read_dir(&open)?.next().is_none());
    assert_eq!(fs::read_to_string(occupied.join("fux.sock"))?, "keep");
    assert!(
        fs::symlink_metadata(occupied.join("link.sock"))?
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::symlink_metadata(directory.join("linked"))?
            .file_type()
            .is_symlink()
    );
    let names: Vec<_> = fs::read_dir(&occupied)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names.len(), 2, "{names:?}");
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn a_killed_servers_socket_is_recovered_and_a_live_one_is_not_taken() -> Outcome {
    let (directory, socket) = fixture()?;
    let mut first = Spawned::start(&directory, &socket, |_| {})?;
    eventually(|| Ok(answers(&socket)))?;
    first.child.kill()?;
    first.child.wait()?;
    // SIGKILL runs no cleanup: the socket file is left behind.
    assert!(fs::symlink_metadata(&socket).is_ok());
    let second = Spawned::start(&directory, &socket, |_| {})?;
    eventually(|| Ok(answers(&socket)))?;
    // A second server for a live socket refuses and leaves the first serving.
    let mut third = Spawned::start(&directory, &socket, |_| {})?;
    assert!(!third.exited()?.success());
    assert!(third.log().contains("already using"), "{}", third.log());
    assert!(answers(&socket));
    stop(&socket)?;
    drop(second);
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn servers_racing_for_one_socket_leave_exactly_one_running() -> Outcome {
    let (directory, socket) = fixture()?;
    let mut servers = (0..3)
        .map(|_| Spawned::start(&directory, &socket, |_| {}))
        .collect::<Result<Vec<_>, _>>()?;
    eventually(|| Ok(answers(&socket)))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut running = servers.len();
    while running > 1 && Instant::now() < deadline {
        running = 0;
        for server in &mut servers {
            if server.child.try_wait()?.is_none() {
                running += 1;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(running, 1);
    for server in &mut servers {
        if let Some(status) = server.child.try_wait()? {
            assert!(!status.success());
            assert!(server.log().contains("already using"), "{}", server.log());
        }
    }
    assert!(answers(&socket));
    stop(&socket)?;
    drop(servers);
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn shutdown_removes_its_own_socket_but_not_a_replacement() -> Outcome {
    let (directory, socket) = fixture()?;
    let mut server = Spawned::start(&directory, &socket, |_| {})?;
    eventually(|| Ok(answers(&socket)))?;
    stop(&socket)?;
    assert!(server.exited()?.success());
    assert!(fs::symlink_metadata(&socket).is_err());

    let mut server = Spawned::start(&directory, &socket, |_| {})?;
    eventually(|| Ok(answers(&socket)))?;
    // Replace the file under a running server; its cleanup must spare it.
    let connection = UnixStream::connect(&socket)?;
    fs::remove_file(&socket)?;
    let replacement = std::os::unix::net::UnixListener::bind(&socket)?;
    let mut connection = connection;
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"world.trigger_event","params":{"event":"fux::control::Shutdown","value":null}}"#;
    write!(
        connection,
        "POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    assert!(server.exited()?.success());
    assert!(fs::symlink_metadata(&socket)?.file_type().is_socket_file());
    drop(replacement);
    fs::remove_dir_all(directory)?;
    Ok(())
}

trait SocketFile {
    fn is_socket_file(&self) -> bool;
}
impl SocketFile for fs::FileType {
    fn is_socket_file(&self) -> bool {
        std::os::unix::fs::FileTypeExt::is_socket(self)
    }
}

/// Sends raw HTTP and reads the whole response.
fn exchange(socket: &Path, request: &str) -> Result<String, Fail> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn post(socket: &Path, body: &str) -> Result<String, Fail> {
    exchange(
        socket,
        &format!(
            "POST / HTTP/1.1\r\nHost: fux\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

#[test]
fn the_wire_is_unchanged_over_the_socket() -> Outcome {
    let server = Server::start()?;
    let viewer = server.attach()?;
    let single = post(
        &server.socket,
        r#"{"jsonrpc":"2.0","id":7,"method":"fux.frame","params":{"viewer":0}}"#,
    )?;
    assert!(
        single.contains("content-type: application/json"),
        "{single}"
    );
    assert!(
        single.contains(r#""id":7"#) && single.contains(r#""error""#),
        "{single}"
    );
    let batch = post(
        &server.socket,
        &format!(
            r#"[{{"jsonrpc":"2.0","id":1,"method":"rpc.discover"}},{{"jsonrpc":"2.0","id":2,"method":"fux.frame+watch","params":{{"viewer":{viewer}}}}}]"#
        ),
    )?;
    assert!(
        batch.contains("Streaming can not be used in batch requests"),
        "{batch}"
    );
    assert!(batch.contains(r#""id":1,"result""#), "{batch}");
    let garbage = post(&server.socket, "not json")?;
    assert!(garbage.contains("-32600"), "{garbage}");
    let unnamed = post(&server.socket, r#"{"jsonrpc":"2.0","id":3}"#)?;
    assert!(
        unnamed.contains(r#""id":3"#) && unnamed.contains("-32600"),
        "{unnamed}"
    );
    // Without the stock HTTP plugin there is no TCP server to advertise.
    let discover = server.rpc("rpc.discover", Value::Null)?;
    assert!(
        discover.get("servers").is_none_or(Value::is_null),
        "{discover}"
    );
    // The command-line client ignores proxy settings: it never leaves the socket.
    let socket = server.socket.to_string_lossy().into_owned();
    let output = fux(
        &["rpc", "rpc.discover"],
        &[
            ("FUX_SOCKET", &socket),
            ("HTTP_PROXY", "http://127.0.0.1:9"),
            ("http_proxy", "http://127.0.0.1:9"),
            ("ALL_PROXY", "socks5://127.0.0.1:9"),
        ],
    )?;
    assert!(output.status.success(), "{output:?}");
    Ok(())
}

/// Opens a watch for `viewer` and returns the stream and a reader of events.
fn watch(socket: &Path, viewer: u64) -> Result<(UnixStream, BufReader<UnixStream>), Fail> {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":9,"method":"fux.frame+watch","params":{{"viewer":{viewer}}}}}"#
    );
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    let reader = BufReader::new(stream.try_clone()?);
    Ok((stream, reader))
}

fn next_event(reader: &mut BufReader<UnixStream>) -> Result<Value, Fail> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err("watch closed".into());
        }
        if let Some(data) = line.strip_prefix("data: ") {
            return Ok(serde_json::from_str(data.trim())?);
        }
    }
}

fn viewer_exists(server: &Server, viewer: u64) -> Result<bool, String> {
    Ok(server
        .query("fux::model::Viewer")?
        .rows()
        .any(|row| row.at("entity") == viewer))
}

#[test]
fn watches_stream_over_the_socket_and_closing_one_detaches_its_viewer() -> Outcome {
    let server = Server::start()?;
    // Busy: frames are flowing when the connection closes.
    let busy = server.attach()?;
    let (stream, mut reader) = watch(&server.socket, busy)?;
    let mut headers = String::new();
    while reader.read_line(&mut headers)? > 2 && !headers.ends_with("\r\n\r\n") {}
    assert!(
        headers.contains("content-type: text/event-stream"),
        "{headers}"
    );
    let first = next_event(&mut reader)?;
    assert_eq!(first.at("id"), 9);
    assert!(first.at("result").at("paint").is_string());
    server.split(busy, "horizontal", Some("exec yes stream"))?;
    next_event(&mut reader)?;
    drop(reader);
    drop(stream);
    eventually(|| Ok(!viewer_exists(&server, busy)?))?;

    // Idle: on a fresh server nothing repaints this viewer after its first
    // frame, so detaching must not wait for another paint.
    drop(server);
    let server = Server::start()?;
    let idle = server.attach()?;
    let (stream, mut reader) = watch(&server.socket, idle)?;
    next_event(&mut reader)?;
    thread::sleep(Duration::from_millis(200));
    drop(reader);
    drop(stream);
    eventually(|| Ok(!viewer_exists(&server, idle)?))?;
    Ok(())
}

/// Hunt 6 finding 003. A `fux.frame+watch` request names the viewer to stream,
/// and fux used to despawn whatever that id named when the connection closed:
/// a tab, a process entity and its child, or one of Bevy's resource entities,
/// which ended the server. The detach applies to viewers only.
fn entities_with(server: &Server, component: &str) -> Result<Vec<u64>, String> {
    Ok(server
        .query(component)?
        .rows()
        .filter_map(|row| row.at("entity").as_u64())
        .collect())
}

/// Opens a watch for `id`, lets the request be dispatched, then closes it.
fn watch_then_close(server: &Server, id: u64) -> Result<(), Fail> {
    let (stream, mut reader) = watch(&server.socket, id)?;
    let mut line = String::new();
    // Read whatever arrives, if anything: an id that is not a viewer is
    // answered with an error rather than a stream.
    let _ = reader.read_line(&mut line);
    drop(reader);
    drop(stream);
    thread::sleep(Duration::from_millis(400));
    Ok(())
}

#[test]
fn a_closed_watch_detaches_only_a_viewer() -> Outcome {
    let server = Server::start()?;
    let viewer = server.attach()?;
    server.tab_new(viewer, Some("victim"))?;
    server.split(viewer, "horizontal", Some("exec sleep 600"))?;
    eventually(|| Ok(entities_with(&server, "fux::model::ProcessState")?.len() >= 2))?;

    let tabs = entities_with(&server, "fux::model::Tab")?;
    let victim_tab = tabs.iter().copied().max().need()?;
    let processes = server.query("fux::model::ProcessState")?;
    let (process_entity, pid) = processes
        .rows()
        .find_map(|row| {
            let pid = row
                .at("components")
                .at("fux::model::ProcessState")
                .at("status")
                .at("pid")
                .as_i64()?;
            Some((row.at("entity").as_u64()?, pid as i32))
        })
        .need()?;
    assert!(alive(pid));

    // A tab, by the streaming route and by the refused-batch route.
    watch_then_close(&server, victim_tab)?;
    let batch = post(
        &server.socket,
        &format!(
            r#"[{{"jsonrpc":"2.0","id":1,"method":"fux.frame+watch","params":{{"viewer":{victim_tab}}}}}]"#
        ),
    )?;
    assert!(
        batch.contains("Streaming can not be used in batch requests"),
        "{batch}"
    );
    thread::sleep(Duration::from_millis(400));
    assert!(
        entities_with(&server, "fux::model::Tab")?.contains(&victim_tab),
        "a closed watch despawned a tab"
    );

    // A process entity: its child must keep running.
    watch_then_close(&server, process_entity)?;
    assert!(alive(pid), "a closed watch killed a child process");
    assert!(entities_with(&server, "fux::model::ProcessState")?.contains(&process_entity));

    // Bevy's resource entities sit at the top of the id space, because
    // `Entity::to_bits` complements the index. Despawning one aborted the server.
    for bits in [0xFFFF_FFFF_u64, 0xFFFF_FFFE, 0xFFFF_FFFD] {
        watch_then_close(&server, bits)?;
        assert!(
            server.rpc("rpc.discover", Value::Null).is_ok(),
            "watching entity bits {bits:#x} ended the server"
        );
    }

    // A workspace, and the viewer's own focused pane view.
    let workspace = server.workspace_of(viewer)?;
    let focused = server.focused(viewer)?.as_u64().need()?;
    for id in [workspace, focused] {
        watch_then_close(&server, id)?;
    }
    assert!(entities_with(&server, "fux::model::Workspace")?.contains(&workspace));
    assert!(entities_with(&server, "fux::model::PaneView")?.contains(&focused));

    // Everything the viewer needs is still there, and it still paints.
    assert_viewer_consistent(&server, viewer)?;
    assert!(!server.screen(viewer)?.is_empty());
    Ok(())
}

/// The documented behaviour is unchanged: closing a watch on a real viewer
/// detaches that viewer, whether or not frames were flowing.
#[test]
fn a_closed_watch_still_detaches_a_real_viewer() -> Outcome {
    let server = Server::start()?;
    let busy = server.attach()?;
    let (stream, mut reader) = watch(&server.socket, busy)?;
    let mut headers = String::new();
    while reader.read_line(&mut headers)? > 2 && !headers.ends_with("\r\n\r\n") {}
    next_event(&mut reader)?;
    server.split(busy, "horizontal", Some("exec yes stream"))?;
    next_event(&mut reader)?;
    drop(reader);
    drop(stream);
    eventually(|| Ok(!viewer_exists(&server, busy)?))?;

    // A refused batch, by contrast, must leave the viewer attached: a refused
    // request may not change the world.
    let idle = server.attach()?;
    let batch = post(
        &server.socket,
        &format!(
            r#"[{{"jsonrpc":"2.0","id":1,"method":"fux.frame+watch","params":{{"viewer":{idle}}}}}]"#
        ),
    )?;
    assert!(
        batch.contains("Streaming can not be used in batch requests"),
        "{batch}"
    );
    thread::sleep(Duration::from_millis(500));
    assert!(
        viewer_exists(&server, idle)?,
        "a refused batch detached the viewer it named"
    );
    Ok(())
}

/// Hunt 6 findings 004 and 005. Both are `bevy_remote` defects that fux reaches
/// because it serves the stock registry unfiltered, and both ended the server
/// with one accepted request. fux replaces the two methods with guards that
/// answer for the named entity and then hand the request to the stock handler;
/// this pins that, against unmodified bevy.
#[test]
fn stock_methods_refuse_ids_that_used_to_end_the_server() -> Outcome {
    let server = Server::start()?;
    let viewer = server.attach()?;

    // 005: `world.mutate_components` reached `World::entity_mut`, which panics
    // on an entity that is not alive. A despawned id is the ordinary way in.
    let spawned = server
        .rpc(
            "world.spawn_entity",
            json!({"components":{"bevy_ecs::name::Name":"probe"}}),
        )?
        .at("entity")
        .as_u64()
        .need()?;
    server.rpc("world.despawn_entity", json!({ "entity": spawned }))?;
    let never = 12_345_u64;
    for id in [spawned, never] {
        let error = server
            .rpc(
                "world.mutate_components",
                json!({"entity":id,"component":"bevy_ecs::name::Name","path":"","value":"x"}),
            )
            .err()
            .unwrap_or_default();
        assert!(!error.is_empty(), "mutating entity {id} was not refused");
        assert!(
            server.rpc("rpc.discover", Value::Null).is_ok(),
            "mutating entity {id} ended the server"
        );
    }

    // 004: `world.despawn_entity` would despawn one of Bevy's resource
    // entities, and the next command flush panicked inside the ECS.
    for bits in [0xFFFF_FFFF_u64, 0xFFFF_FFFE, 0xFFFF_FFFD] {
        let error = server
            .rpc("world.despawn_entity", json!({ "entity": bits }))
            .err()
            .unwrap_or_default();
        assert!(
            !error.is_empty(),
            "despawning entity bits {bits:#x} was not refused"
        );
        assert!(
            server.rpc("rpc.discover", Value::Null).is_ok(),
            "despawning entity bits {bits:#x} ended the server"
        );
    }

    // Ordinary use of both methods is untouched.
    let live = server
        .rpc(
            "world.spawn_entity",
            json!({"components":{"bevy_ecs::name::Name":"live"}}),
        )?
        .at("entity")
        .as_u64()
        .need()?;
    server.rpc(
        "world.mutate_components",
        json!({"entity":live,"component":"bevy_ecs::name::Name","path":"","value":"renamed"}),
    )?;
    assert_eq!(
        components(&server, live, &["bevy_ecs::name::Name"])?.at("bevy_ecs::name::Name"),
        json!("renamed")
    );
    server.rpc("world.despawn_entity", json!({ "entity": live }))?;
    assert_viewer_consistent(&server, viewer)?;
    assert!(!server.screen(viewer)?.is_empty());
    Ok(())
}

/// Hunt 6 finding 006. `accept` failing with EMFILE is not like a peer that
/// aborted: retrying cannot free a descriptor. fux used to warn and retry
/// twenty times a second for as long as the pressure lasted, while clients
/// waited in silence on a backlog that never drained.
#[test]
fn descriptor_pressure_is_reported_bounded_and_recovers() -> Outcome {
    let (directory, socket) = fixture()?;
    let server = Spawned::start(&directory, &socket, |command| {
        // SAFETY: setrlimit is async-signal-safe and touches only the child.
        unsafe {
            command.pre_exec(|| {
                let limit = nix::libc::rlimit {
                    rlim_cur: 64,
                    rlim_max: 64,
                };
                if nix::libc::setrlimit(nix::libc::RLIMIT_NOFILE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    })?;
    eventually(|| Ok(answers(&socket)))?;

    // One client holds connections until the server runs out of descriptors.
    let mut held = Vec::new();
    for _ in 0..400 {
        match UnixStream::connect(&socket) {
            Ok(mut stream) => {
                let _ = stream.write_all(b"GET /hold HTTP/1.1\r\nHost: fux\r\n");
                held.push(stream);
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
    assert!(held.len() > 50, "only held {} connections", held.len());

    // Every attempt must fail promptly rather than hang.
    let mut slowest = Duration::ZERO;
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        let started = Instant::now();
        let _ = answers(&socket);
        slowest = slowest.max(started.elapsed());
        thread::sleep(Duration::from_millis(200));
    }
    assert!(
        slowest < Duration::from_secs(5),
        "a client waited {slowest:?} on a listener that could not answer"
    );

    // The condition is reported, and the log is bounded rather than a flood.
    let log = server.log();
    assert!(
        log.contains("out of descriptors"),
        "descriptor pressure was never reported: {log}"
    );
    let lines = log.matches("BRP socket accept").count();
    assert!(lines <= 40, "the accept loop logged {lines} lines, a flood");

    // Releasing the pressure brings the server back on its own.
    drop(held);
    eventually(|| Ok(answers(&socket)))?;
    assert!(
        server.log().contains("accept recovered"),
        "recovery was not reported"
    );
    stop(&socket)?;
    drop(server);
    fs::remove_dir_all(directory)?;
    Ok(())
}

/// Hunt 6 finding 007. The body was read whole with no bound, so one
/// connection could make the server hold whatever it sent (256 MB in grew it
/// by 580 MB), and a batch amplified on the way out: 478 KiB holding 10 000
/// requests was answered with 12.49 MB.
#[test]
fn request_bodies_and_batches_are_bounded() -> Outcome {
    let server = Server::start()?;
    let viewer = server.attach()?;

    // A body over the limit is refused by its size, with a typed error naming
    // the limit, and the server is untouched.
    let over = fux_transport_max_body() + 1;
    let head = br#"{"jsonrpc":"2.0","id":1,"method":""#;
    let tail = br#"","params":null}"#;
    let filler = over - head.len() - tail.len();
    let mut stream = UnixStream::connect(&server.socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    write!(
        stream,
        "POST / HTTP/1.1\r\nHost: fux\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        over
    )?;
    stream.write_all(head)?;
    let chunk = vec![b'a'; 1 << 16];
    let mut sent = 0;
    while sent < filler {
        let n = chunk.len().min(filler - sent);
        if stream.write_all(chunk.get(..n).need()?).is_err() {
            break;
        }
        sent += n;
    }
    let _ = stream.write_all(tail);
    let mut reply = String::new();
    let _ = stream.read_to_string(&mut reply);
    assert!(
        reply.contains("byte limit"),
        "an oversized body was not refused by its size: {reply:.200}"
    );
    assert!(server.rpc("rpc.discover", Value::Null).is_ok());

    // A body just under the limit is still accepted, so the bound is a limit
    // and not a smaller accident.
    let under = fux_transport_max_body() - 1024;
    let method = "a".repeat(under - head.len() - tail.len());
    let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":null}}"#);
    let reply = post(&server.socket, &body)?;
    assert!(
        reply.contains("-32601"),
        "a body under the limit was not answered on its merits: {reply:.200}"
    );

    // A batch over the count cap is refused, naming the cap.
    let one = r#"{"jsonrpc":"2.0","id":1,"method":"rpc.discover"}"#;
    let huge = format!(
        "[{}]",
        std::iter::repeat_n(one, 10_000)
            .collect::<Vec<_>>()
            .join(",")
    );
    let reply = post(&server.socket, &huge)?;
    assert!(
        reply.contains("limit of"),
        "a 10000-request batch was not refused: {reply:.200}"
    );
    assert!(
        reply.len() < 4096,
        "the refusal itself was large: {}",
        reply.len()
    );

    // A batch at the harness's largest legitimate size still works.
    let ordinary = format!(
        "[{}]",
        std::iter::repeat_n(one, 1000).collect::<Vec<_>>().join(",")
    );
    let reply = post(&server.socket, &ordinary)?;
    assert!(
        reply.contains("\"result\""),
        "a 1000-request batch was refused"
    );

    // The reply is bounded even when every request answers with a lot.
    let schema = r#"{"jsonrpc":"2.0","id":1,"method":"registry.schema"}"#;
    let fat = format!(
        "[{}]",
        std::iter::repeat_n(schema, 1000)
            .collect::<Vec<_>>()
            .join(",")
    );
    let reply = post(&server.socket, &fat)?;
    assert!(
        reply.len() < 32 << 20,
        "a batch reply reached {} bytes",
        reply.len()
    );
    assert!(
        reply.contains("byte limit"),
        "the reply budget was never reported: {:.200}",
        &reply[reply.len().saturating_sub(400)..]
    );

    assert_viewer_consistent(&server, viewer)?;
    assert!(!server.screen(viewer)?.is_empty());
    Ok(())
}

/// A peer that completes `fux.attach` and then streams an event that never
/// ends. It is deliberately not fux: `FUX_SOCKET` names a socket, and any
/// same-user process can create one, so the frontend has to defend itself
/// against a server that is wedged, buggy or replaced.
struct EndlessPeer {
    directory: PathBuf,
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    sent: Arc<AtomicU64>,
}

impl EndlessPeer {
    fn start(terminated: bool) -> Result<Self, Fail> {
        let directory = std::env::temp_dir().join(format!(
            "fux-peer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory)?;
        let inner = directory.join("s");
        fs::create_dir(&inner)?;
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o700))?;
        let socket = inner.join("fux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        let stop = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU64::new(0));
        let (flag, counter) = (Arc::clone(&stop), Arc::clone(&sent));
        thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(stream) = stream else { return };
                // One thread per connection, and every connection serves
                // requests until it is closed: the frontend pools them.
                let (flag, counter) = (Arc::clone(&flag), Arc::clone(&counter));
                thread::spawn(move || serve_peer(stream, terminated, &flag, &counter));
            }
        });
        Ok(Self {
            directory,
            socket,
            stop,
            sent,
        })
    }
}

impl Drop for EndlessPeer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket);
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Answers every request on one connection, and streams once asked to watch.
fn serve_peer(
    mut stream: std::os::unix::net::UnixStream,
    terminated: bool,
    flag: &Arc<AtomicBool>,
    counter: &Arc<AtomicU64>,
) {
    loop {
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(0) | Err(_) => return,
                Ok(_) => request.extend_from_slice(&byte),
            }
        }
        let length = String::from_utf8_lossy(&request)
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        let mut body = vec![0_u8; length];
        if stream.read_exact(&mut body).is_err() {
            return;
        }
        if !String::from_utf8_lossy(&body).contains("fux.frame+watch") {
            // Anything else: a result the frontend can parse, connection kept.
            let reply = br#"{"jsonrpc":"2.0","id":1,"result":{"viewer":4294967295}}"#;
            if write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                reply.len()
            )
            .is_err()
                || stream.write_all(reply).is_err()
            {
                return;
            }
            continue;
        }
        if stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                  transfer-encoding: chunked\r\n\r\n",
            )
            .is_err()
        {
            return;
        }
        let blob = vec![b'a'; 1 << 20];
        while !flag.load(Ordering::Relaxed) {
            // Terminated: ordinary frames. Otherwise: one endless line.
            let payload: Vec<u8> = if terminated {
                format!(
                    "data: {{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":\
                     {{\"paint\":\"{}\",\"detach\":false}}}}\n\n",
                    "x".repeat(4096)
                )
                .into_bytes()
            } else {
                blob.clone()
            };
            if write!(stream, "{:x}\r\n", payload.len()).is_err()
                || stream.write_all(&payload).is_err()
                || stream.write_all(b"\r\n").is_err()
            {
                return;
            }
            counter.fetch_add(payload.len() as u64, Ordering::Relaxed);
        }
        return;
    }
}

/// Hunt 6 finding 008. `fux attach` read the watch stream with
/// `read_line` into a `String`, so a peer that never sent a newline was
/// buffered without bound: 5.3 GB in sixty seconds, climbing at 95 MB/s.
#[test]
fn an_endless_event_ends_the_attachment_and_restores_the_terminal() -> Outcome {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let peer = EndlessPeer::start(false)?;
    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut reader = pair.master.try_clone_reader()?;
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fux"));
    command.arg("attach");
    command.env("FUX_SOCKET", &peer.socket);
    command.env("TERM", "xterm-256color");
    command.env_remove("FUX_ENDPOINT");
    let mut child = {
        let _spawn = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
        pair.slave.spawn_command(command)?
    };
    drop(pair.slave);
    let collected = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 8192];
        while let Ok(n) = reader.read(&mut chunk) {
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(chunk.get(..n).unwrap_or_default());
        }
        bytes
    });

    // It must end on its own, from the bound rather than from a signal.
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err("the frontend never stopped reading an endless event".into());
        }
        thread::sleep(Duration::from_millis(100));
    };
    assert!(!status.success(), "the frontend exited successfully");

    // The outer terminal is left as it was found: the real PTY's termios is
    // cooked again, and the frontend wrote the resets on its way out.
    let master = pair.master;
    let fd = master.as_raw_fd().need()?;
    // SAFETY: `fd` is the live PTY master owned by `master`, and `attributes`
    // is a plain C struct that tcgetattr fills.
    let cooked = unsafe {
        let mut attributes: nix::libc::termios = std::mem::zeroed();
        assert_eq!(
            nix::libc::tcgetattr(fd, &mut attributes),
            0,
            "could not read the outer terminal's attributes"
        );
        attributes.c_lflag & (nix::libc::ICANON | nix::libc::ECHO)
    };
    assert_eq!(
        cooked,
        nix::libc::ICANON | nix::libc::ECHO,
        "the outer terminal was left in raw mode"
    );
    drop(master);
    let written = collected.join().map_err(|_| "capture thread panicked")?;
    let text = String::from_utf8_lossy(&written);
    for reset in ["\x1b[?1049l", "\x1b[?1003l", "\x1b[?2004l", "\x1b[?25h"] {
        assert!(
            text.contains(reset),
            "the frontend did not write {reset:?} on its way out"
        );
    }
    assert!(
        text.contains("without ending it"),
        "the frontend did not name the bound it hit: {:.400}",
        &text[text.len().saturating_sub(600)..]
    );
    // The peer had to send more than a legitimate frame before this happened.
    assert!(peer.sent.load(Ordering::Relaxed) > 16 << 20);
    Ok(())
}

/// The control for the case above: the same peer sending the same volume as
/// ordinary newline-terminated frames must keep working, so the bound is a
/// bound on one unending event and not on how much a server may send.
#[test]
fn a_stream_of_ordinary_frames_is_not_bounded_away() -> Outcome {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let peer = EndlessPeer::start(true)?;
    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut reader = pair.master.try_clone_reader()?;
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fux"));
    command.arg("attach");
    command.env("FUX_SOCKET", &peer.socket);
    command.env("TERM", "xterm-256color");
    command.env_remove("FUX_ENDPOINT");
    let mut child = {
        let _spawn = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
        pair.slave.spawn_command(command)?
    };
    drop(pair.slave);
    let drain = thread::spawn(move || {
        let mut chunk = [0_u8; 1 << 16];
        while let Ok(n) = reader.read(&mut chunk) {
            if n == 0 {
                break;
            }
        }
    });

    // Well past the volume that ends the unbounded case, the frontend is still
    // attached and painting.
    let deadline = Instant::now() + Duration::from_secs(60);
    while peer.sent.load(Ordering::Relaxed) < (96 << 20) {
        if let Some(status) = child.try_wait()? {
            return Err(format!("the frontend exited early with {status:?}").into());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let sent = peer.sent.load(Ordering::Relaxed);
    assert!(
        sent > 64 << 20,
        "the peer only sent {sent} bytes, less than the bound being tested"
    );
    assert!(
        child.try_wait()?.is_none(),
        "the frontend stopped on a stream of ordinary frames"
    );
    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    let _ = drain.join();
    Ok(())
}
