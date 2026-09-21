use super::*;

fn screen(s: &mut Server, f: usize) -> Result<String> {
    Ok(s.frontend(f)?.parser.screen().contents())
}
fn top_line(frame: &str) -> String {
    frame
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_owned()
}
fn dims_of_focused(s: &mut Server, v: u64) -> Result<(u64, u64)> {
    let leaf = s.relation(v, "fux::model::Focused")?;
    let pane = s
        .query("fux::model::PaneView")?
        .iter()
        .find(|r| id(r).ok() == Some(leaf))
        .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64())
        .ok_or("focused pane")?;
    let st = states(s)?
        .into_iter()
        .find(|(e, _)| *e == pane)
        .map(|(_, st)| st)
        .ok_or("state")?;
    Ok((
        st.get("rows").and_then(Value::as_u64).unwrap_or(0),
        st.get("cols").and_then(Value::as_u64).unwrap_or(0),
    ))
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(screen(s, f)?.contains("DEFAULT-SHELL"))
    })?;
    running(s)?;

    // Alternate screen: thirty lines of history, then an application enters
    // the alternate screen, paints, and leaves. The main screen and its
    // history must be back exactly as they were.
    child_command(
        s,
        f,
        "L=LI; M=MAI; A=ALTER; stty raw -echo; printf '\\033[2J\\033[H'; i=1; while [ $i -le 30 ]; do printf \"${L}NE-%02d\\r\\n\" $i; i=$((i+1)); done; printf \"${M}N-END\"; sleep 0.4; printf \"\\033[?1049h\\033[2J\\033[H${A}NATE\"; sleep 0.6; printf '\\033[?1049l'; exec cat > /dev/null",
    )?;
    s.wait("main screen painted", |s| {
        Ok(screen(s, f)?.contains("MAIN-END"))
    })?;
    let before = s.frame(v, 24, 80)?;
    // Judge the alternate screen from atomic server frames: the frontend's
    // own screen can be read mid-paint and mix two frames.
    let mut alt = String::new();
    s.wait("alternate screen shown", |s| {
        alt = s.frame(v, 24, 80)?;
        Ok(alt.contains("ALTERNATE"))
    })?;
    ensure(
        !alt.contains("MAIN-END"),
        &format!(
            "application: the alternate screen showed main-screen content: {:?}",
            alt.lines()
                .filter(|l| !l.trim().is_empty())
                .take(4)
                .collect::<Vec<_>>()
        ),
    )?;
    s.wait("main screen restored", |s| {
        let now = s.frame(v, 24, 80)?;
        Ok(now.contains("MAIN-END") && !now.contains("ALTERNATE"))
    })
    .map_err(|e| {
        format!("application: leaving the alternate screen did not restore the main screen: {e}")
    })?;
    let after = s.frame(v, 24, 80)?;
    ensure(
        after == before,
        "application: the main screen differs after an alternate-screen round trip",
    )?;
    s.control(v, json!({"kind":"scroll","order":"previous"}))?;
    let mut top = String::new();
    s.wait("history survives the alternate screen", |s| {
        top = top_line(&s.frame(v, 24, 80)?);
        Ok(top.starts_with("LINE-") && top != "LINE-09")
    })
    .map_err(|e| {
        format!("application: history after an alternate-screen round trip: top {top:?}: {e}")
    })?;
    s.send(f, b"x")?; // return to live

    // Application cursor keys: the same Up arrow delivers CSI A normally and
    // SS3 A once the application has set DECCKM.
    s.control(v, json!({"kind":"tab_new","name":"keys"}))?;
    s.wait("second tab", |s| Ok(s.query("fux::model::Tab")?.len() == 2))?;
    child_command(
        s,
        f,
        "stty raw -echo; printf '\\033[2J\\033[HNORMAL'; exec cat > normal.bin",
    )?;
    s.wait("normal-mode child", |s| {
        Ok(screen(s, f)?.starts_with("NORMAL"))
    })?;
    s.send(f, b"\x1b[A")?;
    s.wait("normal arrow delivered", |s| {
        Ok(fs::read(s.directory.join("normal.bin"))? == b"\x1b[A")
    })?;
    s.control(v, json!({"kind":"tab_new","name":"app"}))?;
    s.wait("third tab", |s| Ok(s.query("fux::model::Tab")?.len() == 3))?;
    child_command(
        s,
        f,
        "stty raw -echo; printf '\\033[?1h\\033[2J\\033[HAPP'; exec cat > app.bin",
    )?;
    s.wait("application-mode child", |s| {
        Ok(screen(s, f)?.starts_with("APP"))
    })?;
    s.send(f, b"\x1b[A")?;
    let mut got = Vec::new();
    s.wait("application arrow delivered", |s| {
        got = fs::read(s.directory.join("app.bin"))?;
        Ok(!got.is_empty())
    })?;
    ensure(
        got == b"\x1bOA",
        &format!(
            "application: DECCKM should turn Up into SS3 A, delivered {:?}",
            String::from_utf8_lossy(&got)
        ),
    )?;

    // A child that resizes its own PTY: fux owns the real size, so after the
    // next negotiation the child sees fux's size again.
    s.control(v, json!({"kind":"tab_new","name":"self"}))?;
    s.wait("fourth tab", |s| Ok(s.query("fux::model::Tab")?.len() == 4))?;
    child_command(
        s,
        f,
        "stty rows 5 cols 20; stty size > self.txt; printf '\\033[2J\\033[HSELF'; while :; do stty size > now.txt; sleep 0.1; done",
    )?;
    s.wait("child resized itself", |s| {
        Ok(fs::read_to_string(s.directory.join("self.txt")).is_ok_and(|t| t.trim() == "5 20"))
    })?;
    let negotiated = dims_of_focused(s, v)?;
    s.journal
        .record("self_resize", json!({"negotiated":negotiated}))?;
    // Record whether fux reasserts on its own; the README does not promise it
    // and a child that changes the kernel size keeps it until fux negotiates.
    std::thread::sleep(std::time::Duration::from_millis(400));
    let before_nudge = fs::read_to_string(s.directory.join("now.txt"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    s.journal.record(
        "child_size_before_negotiation",
        json!({"child_sees":before_nudge,"negotiated":negotiated}),
    )?;
    // A genuine negotiation: shrink the viewer and wait for the new size to
    // reach the child, then restore it. Each step is a real ioctl.
    s.resize(f, 23, 79)?;
    let mut seen = String::new();
    s.wait("child sees the shrunken negotiated size", |s| {
        seen = fs::read_to_string(s.directory.join("now.txt")).unwrap_or_default().trim().to_owned();
        Ok(seen == "22 79")
    })
    .map_err(|e| format!("application: after the viewer shrank, the child should see fux's size 22 79, sees {seen:?}: {e}"))?;
    s.resize(f, 24, 80)?;
    s.wait("child sees the restored negotiated size", |s| {
        seen = fs::read_to_string(s.directory.join("now.txt")).unwrap_or_default().trim().to_owned();
        Ok(seen == format!("{} {}", negotiated.0, negotiated.1))
    })
    .map_err(|e| format!("application: after the viewer was restored, the child should see fux's size {negotiated:?}, sees {seen:?}: {e}"))?;

    // 8-bit C1 control bytes in output must not corrupt the paint or the bar.
    s.control(v, json!({"kind":"tab_new","name":"c1"}))?;
    s.wait("fifth tab", |s| Ok(s.query("fux::model::Tab")?.len() == 5))?;
    child_command(
        s,
        f,
        "B=BEF; E=EN; stty raw -echo; printf \"\\033[2J\\033[H${B}ORE\\233\\062\\062m\\205AFTER\\230junk\\234${E}D\"; exec cat > /dev/null",
    )?;
    s.wait("C1 bytes did not stop painting", |s| {
        let now = screen(s, f)?;
        Ok(now.contains("BEFORE") && now.contains("END") && now.contains("c1"))
    })
    .map_err(|e| format!("application: output with 8-bit C1 bytes broke the paint: {e}"))?;
    s.healthy()?;
    Ok(())
}
