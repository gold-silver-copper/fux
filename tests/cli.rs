//! Milestone 5: the whole CLI table, `--json`, targets and their errors, and
//! a `-- CMD` typed into a new shell.
mod support;
use support::*;

/// Waits until `%N`'s screen contains `needle`.
fn shows(server: &Server, pane: &str, needle: &str) -> Outcome {
    eventually(&format!("{needle:?} in {pane}"), || {
        Ok(server.ok(&["capture-pane", "-t", pane])?.contains(needle))
    })
}

#[test]
fn a_typed_command_runs_in_the_new_shell_and_leaves_its_prompt() -> Outcome {
    let server = Server::start("")?;
    // Arguments with spaces and quotes arrive intact.
    let pane = server.ok(&[
        "split", "-h", "-t", "%1", "--", "printf", "[%s]\\n", "a b", "it's", "$HOME",
    ])?;
    assert_eq!(pane.trim(), "%2");
    shows(&server, "%2", "[a b]")?;
    shows(&server, "%2", "[it's]")?;
    shows(&server, "%2", "[$HOME]")?;
    // The command ended; the shell is still there and takes more input.
    server.ok(&[
        "send-keys",
        "-t",
        "%2",
        "echo",
        "Space",
        "still-here",
        "Enter",
    ])?;
    shows(&server, "%2", "\nstill-here")?;
    assert!(
        server.ok(&["ls"])?.contains("%2 printf"),
        "the pane is named after its command"
    );
    // new-tab and new-workspace type theirs too.
    server.ok(&["new-tab", "-t", "+1", "--", "echo", "tab-cmd"])?;
    shows(&server, "%3", "\ntab-cmd")?;
    server.ok(&["new-workspace", "--", "echo", "ws-cmd"])?;
    shows(&server, "%4", "\nws-cmd")?;
    // A control character would act as a key in the shell: refused, and no
    // pane is made.
    let refused = server.fux(&["split", "-h", "-t", "%1", "--", "echo", "a\nb"])?;
    assert_eq!(refused.status, 1);
    assert!(
        refused.stderr.contains("control character"),
        "{}",
        refused.stderr
    );
    assert!(!server.ok(&["ls"])?.contains("%5"));
    Ok(())
}

#[test]
fn targets_come_from_t_or_fux_pane_and_are_never_guessed() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["split", "-h", "-t", "%1"])?;
    // Without -t or FUX_PANE a command fails, naming -t.
    for args in [
        &["kill-pane"][..],
        &["kill-tab"],
        &["kill-workspace"],
        &["new-tab"],
        &["split", "-h"],
    ] {
        let out = server.fux(args)?;
        assert_eq!(out.status, 1, "{args:?}");
        assert!(out.stderr.contains("-t"), "{args:?}: {}", out.stderr);
    }
    // FUX_PANE targets the caller's pane, its tab and its workspace.
    let out = server.fux_env(&["split", "-v"], &[("FUX_PANE", "%2")])?;
    assert_eq!(out.status, 0, "{}", out.stderr);
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("%3"));
    let out = server.fux_env(&["new-tab"], &[("FUX_PANE", "%2")])?;
    assert_eq!(out.stdout.trim(), "@2");
    // Wrong kinds and missing targets.
    assert_eq!(server.fux(&["kill-tab", "-t", "%1"])?.status, 2);
    assert_eq!(server.fux(&["kill-pane", "-t", "@1"])?.status, 2);
    let gone = server.fux(&["kill-tab", "-t", "@9"])?;
    assert_eq!(
        (gone.status, gone.stderr.trim()),
        (1, "fux: no tab @9".trim_start_matches("fux: "))
    );
    let named = server.fux(&["kill-workspace", "-t", "nope"])?;
    assert!(
        named.stderr.contains("no workspace named \"nope\""),
        "{}",
        named.stderr
    );
    Ok(())
}

#[test]
fn rename_move_swap_reorder_and_kill() -> Outcome {
    let server = Server::start("")?;
    server.ok(&["rename", "-t", "%1", "editor"])?;
    server.ok(&["rename", "-t", "@1", "code"])?;
    server.ok(&["rename", "-t", "+1", "work"])?;
    server.ok(&["rename", "-t", "work", "work2"])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.contains("+1 work2") && ls.contains("@1 code") && ls.contains("%1 editor"),
        "{ls}"
    );
    server.ok(&["new-workspace", "-n", "other"])?;
    let clash = server.fux(&["rename", "-t", "other", "work2"])?;
    assert!(
        clash.stderr.contains("another workspace"),
        "{}",
        clash.stderr
    );
    assert_eq!(server.fux(&["rename", "-t", "%1", ""])?.status, 1);
    // Swap across workspaces.
    server.ok(&["swap-pane", "-t", "%1", "%2"])?;
    let ls = server.ok(&["ls"])?;
    let at = |needle: &str| ls.find(needle).unwrap_or(usize::MAX);
    assert!(at("%2") < at("+2") && at("%1") > at("+2"), "{ls}");
    // Move to a tab, a workspace, new ones.
    server.ok(&["split", "-h", "-t", "%2"])?;
    server.ok(&["move-pane", "-t", "%3", "--to", "+2"])?;
    server.ok(&["move-pane", "-t", "%3", "--to", "new-tab"])?;
    let ls = server.ok(&["ls"])?;
    assert!(ls.contains("@3 tab-2"), "{ls}");
    // Reorder tabs and workspaces.
    server.ok(&["reorder", "tab", "-t", "@3", "--previous"])?;
    let ls = server.ok(&["ls"])?;
    assert!(
        ls.find("@3").unwrap_or(0) < ls.find("@2").unwrap_or(0),
        "{ls}"
    );
    server.ok(&["reorder", "-t", "+2", "--previous"])?;
    assert!(server.ok(&["ls"])?.starts_with("+2 other"));
    let end = server.fux(&["reorder", "-t", "+2", "--previous"])?;
    assert!(end.stderr.contains("already at that end"), "{}", end.stderr);
    // Kill a pane (its tab, now empty of panes, goes with it), a workspace,
    // and a tab (the workspace's last, so the workspace goes too).
    server.ok(&["kill-pane", "-t", "%3"])?;
    assert!(!server.ok(&["ls"])?.contains("@3 "));
    server.ok(&["kill-workspace", "-t", "work2"])?;
    let ls = server.ok(&["ls"])?;
    assert!(!ls.contains("work2") && ls.contains("other"), "{ls}");
    server.ok(&["new-workspace", "-n", "third"])?;
    server.ok(&["kill-tab", "-t", "@2"])?;
    let ls = server.ok(&["ls"])?;
    assert!(!ls.contains("other") && ls.contains("third"), "{ls}");
    Ok(())
}

#[test]
fn send_keys_terminate_and_buffers() -> Outcome {
    let server = Server::start("")?;
    // send-keys types at once; after the prompt, so the output is a line
    // of its own however slowly the shell starts.
    shows(&server, "%1", "$")?;
    server.ok(&["send-keys", "-t", "%1", "-l", "echo lit", "eral"])?;
    server.ok(&["send-keys", "-t", "%1", "Enter"])?;
    shows(&server, "%1", "\nliteral")?;
    // terminate stops what runs in the foreground, never the shell.
    let idle = server.fux(&["terminate", "-t", "%1"])?;
    assert!(
        idle.stderr.contains("nothing is running"),
        "{}",
        idle.stderr
    );
    server.ok(&[
        "send-keys",
        "-t",
        "%1",
        "sleep 600; echo after-sleep",
        "Enter",
    ])?;
    eventually("sleep in the foreground", || {
        Ok(server.fux(&["terminate", "-t", "%1"])?.status == 0)
    })?;
    shows(&server, "%1", "after-sleep")?;
    assert!(server.ok(&["ls"])?.contains("%1"), "the shell survives");
    // No buffers yet.
    assert_eq!(server.ok(&["list-buffers"])?, "");
    assert_eq!(server.fux(&["show-buffer"])?.status, 1);
    Ok(())
}

#[test]
fn ls_json_and_list_keys_have_their_documented_shapes() -> Outcome {
    let server = Server::start("")?;
    let mut client = server.attach(10, 50)?;
    client.wait_for("$")?;
    let json = server.ok(&["ls", "--json"])?;
    for field in [
        "{\"workspaces\":[{\"id\":\"+1\",\"name\":\"main\",\"tabs\":[{\"id\":\"@1\",\"name\":\"main\",\"panes\":[{\"id\":\"%1\",\"name\":\"sh\",\"title\":\"\",\"rows\":9,\"cols\":50,\"pid\":",
        "\"clients\":[{\"id\":\"c1\",\"rows\":10,\"cols\":50,\"workspace\":\"+1\",\"tab\":\"@1\",\"pane\":\"%1\",\"zoom\":false}]}",
    ] {
        assert!(json.contains(field), "{field}\nin\n{json}");
    }
    let keys = server.ok(&["list-keys"])?;
    for name in ["Enter", "BTab", "BSpace", "F12", "PageDown"] {
        assert!(keys.contains(name), "{name}");
    }
    assert!(keys.contains("split -h"));
    let help = server.fux(&["help"])?;
    assert_eq!(help.status, 0);
    assert!(help.stdout.contains("capture-pane"));
    Ok(())
}

/// A shell setup that writes during its startup and then discards pending
/// input (as some line editors and plugins do) must not lose a typed
/// command: fux types it only once the output has gone quiet. The stand-in
/// startup is one Python process -- so no process start opens a gap in its
/// output -- that prints a line every 5 ms for about 200 ms and flushes the
/// terminal's pending input after the tenth line, some 50 ms in; then sh
/// starts.
///
/// That tests something only if the stand-in's first line comes before
/// fux's deadline and its lines up to the flush come within fux's quiet
/// time of each other. A slow machine cannot always manage it (under
/// emulated amd64 its gaps were 50 to 85 ms), and then fux types in a gap,
/// as designed; so the stand-in reports its timing, and an attempt that
/// missed the premise is run again, up to five times.
#[test]
fn a_typed_command_survives_a_startup_that_writes_then_discards_input() -> Outcome {
    let mut missed = Vec::new();
    for _ in 0..5 {
        match typed_command_after_a_noisy_startup()? {
            Ok(()) => return Ok(()),
            Err(why) => missed.push(why),
        }
    }
    Err(format!(
        "in five attempts the stand-in startup never wrote fast enough to test anything: {missed:?}"
    ))
}

/// One attempt: `Err` inside if the premise did not hold, and nothing was
/// tested.
fn typed_command_after_a_noisy_startup() -> Result<Result<(), String>, String> {
    let dir = std::env::temp_dir().canonicalize().map_err(e)?;
    let script = dir.join(format!("fux-noisy-startup-{}.py", std::process::id()));
    let timing = dir.join(format!("fux-noisy-timing-{}", std::process::id()));
    let _ = std::fs::remove_file(&timing);
    std::fs::write(
        &script,
        format!(
            "import termios, time\n\
             first = time.time()\n\
             last = time.monotonic()\n\
             gap = 0.0\n\
             for i in range(40):\n\
             \x20   print('starting-%d' % i, flush=True)\n\
             \x20   now = time.monotonic()\n\
             \x20   if i <= 10:\n\
             \x20       gap = max(gap, now - last)\n\
             \x20   last = now\n\
             \x20   time.sleep(0.005)\n\
             \x20   if i == 10:\n\
             \x20       termios.tcflush(0, termios.TCIFLUSH)\n\
             open('{}', 'w').write('%f %f' % (first, gap))\n",
            timing.display()
        ),
    )
    .map_err(e)?;
    let shell = format!(
        "set shell /bin/sh -c \"python3 {}; exec /bin/sh\"",
        script.display()
    );
    let server = Server::start(&shell)?;
    let split_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(e)?
        .as_secs_f64();
    server.ok(&["split", "-h", "-t", "%1", "--", "echo", "typed-after-quiet"])?;
    // The command ran: its output is on a line of its own, or after a
    // prompt drawn before it; which, the premise does not decide.
    let ran = |screen: &str| {
        screen
            .lines()
            .any(|l| l.ends_with("typed-after-quiet") && !l.contains("echo"))
    };
    let result = eventually("the typed command's output", || {
        Ok(ran(&server.ok(&["capture-pane", "-t", "%2"])?))
    });
    let screen = server.ok(&["capture-pane", "-t", "%2"])?;
    let _ = std::fs::remove_file(&script);
    let measured = std::fs::read_to_string(&timing).unwrap_or_default();
    let _ = std::fs::remove_file(&timing);
    let mut words = measured.split_whitespace().map(str::parse::<f64>);
    let (Some(Ok(first)), Some(Ok(gap))) = (words.next(), words.next()) else {
        return Err(format!(
            "the stand-in reported no timing; %2 shows:\n{screen}"
        ));
    };
    // With a margin: the deadline counts from a little before the split.
    let late = first - split_at;
    let deadline = fux::session::TYPE_WAIT.as_secs_f64() * 0.8;
    let quiet = fux::pane::QUIET.as_secs_f64() * 0.8;
    if late > deadline || gap > quiet {
        return Ok(Err(format!(
            "first line {:.0} ms after the split, largest gap {:.0} ms",
            late * 1000.0,
            gap * 1000.0
        )));
    }
    if result.is_err() {
        return Err(format!("the typed command was lost; %2 shows:\n{screen}"));
    }
    assert!(
        screen.contains("starting-39"),
        "the startup ran whole: {screen}"
    );
    Ok(Ok(()))
}
