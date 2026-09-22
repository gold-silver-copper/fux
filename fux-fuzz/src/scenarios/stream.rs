use super::*;

fn screen(s: &mut Server, f: usize) -> Result<String> {
    Ok(s.frontend(f)?.parser.screen().contents())
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(screen(s, f)?.contains("DEFAULT-SHELL"))
    })?;
    let shell = *running(s)?.first().ok_or("no shell")?;
    // A hot pane whose lines carry a counter, so any lost or reordered paint
    // shows up as a non-monotonic screen.
    s.control(v, json!({"kind":"split","axis":"vertical","program":"i=0; while :; do i=$((i+1)); printf 'HOT-%06d\\n' $i; done"}))?;
    s.wait("hot output flowing", |s| Ok(screen(s, f)?.contains("HOT-")))?;
    let hot = running(s)?
        .into_iter()
        .find(|p| *p != shell)
        .ok_or("hot pid")?;

    // Stop reading the frontend's outer PTY for three seconds while output
    // is hot, keeping the server busy with requests meanwhile.
    s.frontend(f)?.paused = true;
    s.journal.record("pause_outer_pty", json!({"seconds":3}))?;
    let started = std::time::Instant::now();
    let mut requests = 0;
    while started.elapsed() < std::time::Duration::from_secs(3) {
        s.rpc("fux.frame", json!({"viewer":v}))?;
        requests += 1;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    ensure(
        alive(hot) && alive(shell),
        "application: a paused frontend killed a process",
    )?;
    let resumed = std::time::Instant::now();
    s.frontend(f)?.paused = false;
    s.journal.record(
        "resume_outer_pty",
        json!({"requests_while_paused":requests}),
    )?;

    // Once reading resumes the frontend must catch up: its screen shows a
    // recent counter, and the server is still healthy.
    let mut counter = 0u64;
    s.wait("frontend caught up after the pause", |s| {
        let now = screen(s, f)?;
        counter = now
            .lines()
            .filter_map(|l| l.trim().strip_prefix("HOT-")?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        Ok(counter > 0)
    })?;
    let catch_up_ms = resumed.elapsed().as_millis();
    let first_seen = counter;
    s.wait("counter keeps advancing after the pause", |s| {
        let now = screen(s, f)?;
        let latest = now
            .lines()
            .filter_map(|l| l.trim().strip_prefix("HOT-")?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        Ok(latest > first_seen)
    })
    .map_err(|e| format!("application: after resuming reads the frontend's screen stopped advancing at HOT-{first_seen}: {e}"))?;

    let advancing_ms = resumed.elapsed().as_millis();
    // Stop the hot process; once output ends, what the frontend shows must
    // equal a fresh server-side frame rendered the same way.
    let convergence_started = std::time::Instant::now();
    s.control(v, json!({"kind":"terminate"}))?;
    s.wait("hot process terminated", |_| Ok(!alive(hot)))?;
    let mut server_view = String::new();
    let mut front_view = String::new();
    s.wait("frontend converges with the server frame", |s| {
        server_view = s.frame(v, 24, 80)?;
        front_view = screen(s, f)?;
        Ok(server_view == front_view && server_view.contains("[exit:"))
    })
    .map_err(|e| {
        let diff = server_view
            .lines()
            .zip(front_view.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("line {i}: server {a:?} frontend {b:?}"))
            .unwrap_or_else(|| "line counts differ".into());
        format!("application: after output stopped the frontend did not converge with the server frame: {diff}: {e}")
    })?;
    let timings = json!({"catch_up_ms":catch_up_ms,"advancing_ms":advancing_ms,
        "convergence_ms":convergence_started.elapsed().as_millis(),
        "first_counter":first_seen,"final_frame_equal":server_view == front_view});
    s.journal.record("stream_timings", timings.clone())?;
    println!("STREAM-TIMES {timings}");
    ensure(
        alive(shell),
        "application: the shell died during the stream test",
    )?;
    Ok(())
}
