mod adversarial;
mod api_misuse;
mod chrome;
mod churn;
mod clipqueue;
mod concurrent;
mod config;
mod copy;
mod history;
mod hostile;
mod identity;
mod invariant;
mod keys;
mod layout;
mod limits;
mod memory;
mod mouse;
mod mouse_edge;
mod nav;
mod overlay;
mod paste;
mod process;
mod race;
mod raw;
mod reorder;
mod repair;
mod resize_cmd;
mod ron;
mod scale;
mod scene;
mod scene_fidelity;
mod scene_fuzz;
mod scene_map;
mod scene_refs;
mod selection;
mod signal;
mod soak;
mod stream;
mod tabless;
mod terminal_edge;
mod transport;
mod walk;
mod zoom;

use crate::{
    Result, ensure,
    runtime::{Server, alive, component, id},
    trace::{Action, Config, Shutdown},
};
use serde_json::{Value, json};
use std::fs;

const STATE: &str = "fux::model::ProcessState";
const VIEWER: &str = "fux::model::Viewer";
const LAUNCH: &str = "fux::model::Launch";

pub fn execute(server: &mut Server, action: &Action) -> Result<()> {
    if matches!(
        action,
        Action::Shutdown {
            mode: Shutdown::Initializing
        }
    ) {
        // Observe the pre-app.run announcement, but do not wait for BRP
        // readiness. Scheduling decides which initialization phase is interrupted.
        server.announced()?;
        server.signal_shutdown()?;
        return server.wait_server_exit(true);
    }
    server.ready()?;
    match action {
        Action::Startup { config } => startup(server, *config),
        Action::Resize { sizes, token } => resize(server, sizes, token),
        Action::Paste {
            text,
            repeats,
            bracketed,
            chunk_bytes,
        } => paste::run(server, text, *repeats, *bracketed, *chunk_bytes),
        Action::Signal { signal } => signal::run(server, *signal),
        Action::Keys { sequences } => keys::run(server, sequences),
        Action::Mouse { mode, sgr, split } => mouse::run(server, *mode, *sgr, *split),
        Action::Copy { reload } => copy::run(server, *reload),
        Action::History => history::run(server),
        Action::Zoom => zoom::run(server),
        Action::Layout => layout::run(server),
        Action::Process => process::run(server),
        Action::Nav => nav::run(server),
        Action::Scene => scene::run(server),
        Action::Config => config::run(server),
        Action::Overlay => overlay::run(server),
        Action::Limits => limits::run(server),
        Action::Chrome => chrome::run(server),
        Action::Selection => selection::run(server),
        Action::Race => race::run(server),
        Action::Memory => memory::run(server),
        Action::Reorder => reorder::run(server),
        Action::SceneMap => scene_map::run(server),
        Action::MouseEdge => mouse_edge::run(server),
        Action::ClipQueue => clipqueue::run(server),
        Action::ResizeCmd => resize_cmd::run(server),
        Action::ApiMisuse => api_misuse::run(server),
        Action::SceneFidelity => scene_fidelity::run(server),
        Action::Tabless => tabless::run(server),
        Action::Churn => churn::run(server),
        Action::SceneRefs => scene_refs::run(server),
        Action::Soak { cycles } => soak::run(server, *cycles),
        Action::Repair => repair::run(server),
        Action::TerminalEdge => terminal_edge::run(server),
        Action::Stream => stream::run(server),
        Action::Walk { seed, steps } => walk::run(server, *seed, steps),
        Action::Scale => scale::run(server),
        Action::Adversarial { seed } => adversarial::run(server, *seed),
        Action::Concurrent { seed, steps } => concurrent::run(server, *seed, steps),
        Action::Raw { seed, steps } => raw::run(server, *seed, steps),
        Action::SceneFuzz { seed, cases } => scene_fuzz::run(server, *seed, cases),
        Action::Hostile => hostile::run(server),
        Action::Transport => transport::run(server),
        Action::Identity => identity::run(server),
        Action::Shutdown {
            mode: Shutdown::Lifecycle,
        } => lifecycle(server),
        Action::Shutdown {
            mode: Shutdown::Output,
        } => output_shutdown(server),
        Action::Shutdown {
            mode: Shutdown::Initializing,
        } => Err("initialization dispatch error".into()),
    }
}

fn startup(s: &mut Server, config: Config) -> Result<()> {
    let f = s.attach(24, 80)?; // first action after readiness, no config-settling sleep
    let viewer = s.frontend(f)?.viewer;
    let launches = s.query(LAUNCH)?;
    let first = component(launches.first().ok_or("no first process recipe")?, LAUNCH)?;
    s.wait("first shell output", |s| {
        let frame = s.frame(viewer, 24, 80)?;
        Ok(frame.contains("DEFAULT-SHELL") || frame.contains("CONFIGURED-SHELL"))
    })?;
    let frame = s.frame(viewer, 24, 80)?;
    s.journal.record(
        "initial_launch_observed",
        json!({"recipe":first,"frame":frame,"config":config}),
    )?;
    let (name, marker) = match config {
        Config::Valid => ("configured-shell", "CONFIGURED-SHELL"),
        Config::Missing | Config::Malformed | Config::Clipboard => {
            ("default-shell", "DEFAULT-SHELL")
        }
    };
    let expected = json!([s.directory.join(name)]);
    ensure(
        first.get("argv") == Some(&expected) && frame.contains(marker),
        &format!(
            "startup contract: {config:?} config should launch {name} first; observed recipe={first}, frame={frame:?}. The first pane must use settled initial configuration."
        ),
    )
}

fn states(s: &mut Server) -> Result<Vec<(u64, Value)>> {
    s.query(STATE)?
        .iter()
        .map(|r| Ok((id(r)?, component(r, STATE)?)))
        .collect()
}
fn pid(state: &Value) -> Result<i32> {
    state
        .pointer("/status/pid")
        .and_then(Value::as_i64)
        .and_then(|p| i32::try_from(p).ok())
        .ok_or("process is not running".into())
}
fn running(s: &mut Server) -> Result<Vec<i32>> {
    s.wait("process running", |s| {
        Ok(states(s)?
            .iter()
            .all(|(_, state)| state.pointer("/status/kind") == Some(&json!("running"))))
    })?;
    states(s)?.iter().map(|(_, state)| pid(state)).collect()
}
fn dims(s: &mut Server, viewer: u64, rows: u16, cols: u16) -> Result<bool> {
    for row in s.query(VIEWER)? {
        if id(&row)? == viewer {
            let value = component(&row, VIEWER)?;
            return Ok(
                value.get("rows") == Some(&json!(rows)) && value.get("cols") == Some(&json!(cols))
            );
        }
    }
    Err("viewer disappeared while resizing".into())
}
fn process_size(s: &mut Server, rows: u16, cols: u16) -> Result<bool> {
    let states = states(s)?;
    Ok(!states.is_empty()
        && states.iter().all(|(_, state)| {
            state.get("rows") == Some(&json!(rows)) && state.get("cols") == Some(&json!(cols))
        }))
}
fn child_command(s: &mut Server, f: usize, command: &str) -> Result<()> {
    s.send(f, format!("{command}\r").as_bytes())
}
fn detach(s: &mut Server, f: usize) -> Result<()> {
    let viewer = s.frontend(f)?.viewer;
    s.frontend(f)?.expected_exit = true;
    s.send(f, b"\x02d")?;
    s.wait("graceful frontend exit", |s| Ok(s.frontend(f)?.exited))?;
    ensure(
        s.frontend(f)?.exit_success,
        "application: abnormal graceful frontend exit",
    )?;
    ensure(
        s.frontend(f)?.terminal_restored()?,
        "graceful frontend did not restore terminal attributes",
    )?;
    ensure(
        s.frontend(f)?.capture.bytes().ends_with(b"\x1b[?1049l"),
        "graceful frontend did not restore alternate screen",
    )?;
    s.wait("viewer removal", |s| {
        Ok(!s
            .query(VIEWER)?
            .iter()
            .any(|row| id(row).ok() == Some(viewer)))
    })
}
fn resize(s: &mut Server, sizes: &[[u16; 4]], token: &str) -> Result<()> {
    let a = s.attach(24, 80)?;
    let b = s.attach(18, 60)?;
    let va = s.frontend(a)?.viewer;
    let vb = s.frontend(b)?.viewer;
    let pids = running(s)?;
    // Each burst contains three immediate PTY ioctls; no wait separates them.
    // Check liveness during the burst, and settle only its final dimensions.
    for burst in sizes.chunks(3) {
        for &[ar, ac, br, bc] in burst {
            s.resize(a, ar, ac)?;
            s.resize(b, br, bc)?;
            s.rpc("rpc.discover", Value::Null)?;
        }
        let &[ar, ac, br, bc] = burst.last().ok_or("empty resize burst")?;
        // A one-row viewer has only chrome, hence no visible pane constraint.
        let visible: Vec<_> = [(ar, ac), (br, bc)]
            .into_iter()
            .filter(|(r, _)| *r > 1)
            .collect();
        let expected = visible
            .iter()
            .map(|(r, _)| r - 1)
            .min()
            .zip(visible.iter().map(|(_, c)| *c).min());
        s.wait("viewer/PTY resize convergence", |s| {
            Ok(dims(s, va, ar, ac)?
                && dims(s, vb, br, bc)?
                && match expected {
                    // The owned backing emulator now accepts the exact tiny
                    // size. Only the independent frame decoder keeps 2x2.
                    Some((rows, cols)) => process_size(s, rows, cols)?,
                    None => true, // All panes hidden: no new size is negotiated.
                })
        })?;
        for (viewer, r, c) in [(va, ar, ac), (vb, br, bc)] {
            let frame = s.rpc("fux.frame", json!({"viewer":viewer}))?;
            let paint = frame
                .get("paint")
                .and_then(Value::as_str)
                .ok_or("missing paint")?;
            check_frame_bounds(paint.as_bytes(), r, c)?;
        }
    }
    // A supported API zero viewport is distinct from an OS PTY resize to zero.
    let zero = s
        .rpc("fux.attach", json!({"rows":0,"cols":0}))?
        .get("viewer")
        .and_then(Value::as_u64)
        .ok_or("zero attach rejected")?;
    ensure(
        s.frame(zero, 1, 1)?.is_empty(),
        "zero viewport painted content",
    )?;
    s.control(zero, json!({"kind":"detach"}))?;
    s.resize(a, 24, 80)?;
    s.resize(b, 18, 60)?;
    s.wait("final shared size", |s| {
        Ok(dims(s, va, 24, 80)? && dims(s, vb, 18, 60)? && process_size(s, 17, 60)?)
    })?;
    // Verify inside the actual child, not only its reflected ProcessState.
    child_command(s, a, "stty size > size.txt")?;
    s.wait("child stty size", |s| {
        Ok(fs::read_to_string(s.directory.join("size.txt")).is_ok_and(|v| v.trim() == "17 60"))
    })?;
    detach(s, b)?;
    ensure(
        pids.iter().all(|p| alive(*p)),
        "detach killed shared process",
    )?;
    s.wait("detached viewer releases constraint", |s| {
        process_size(s, 23, 80)
    })?;
    child_command(s, a, "stty size > detached-size.txt")?;
    s.wait("child size after detach", |s| {
        Ok(fs::read_to_string(s.directory.join("detached-size.txt"))
            .is_ok_and(|v| v.trim() == "23 80"))
    })?;

    let pane_a = s.relation(va, "fux::model::Focused")?;
    child_command(
        s,
        a,
        "stty raw -echo; printf '\\033[2J\\033[HPANE-A'; cat > input-a",
    )?;
    s.wait("raw pane A ready", |s| {
        Ok(s.frame(va, 24, 80)?.starts_with("PANE-A"))
    })?;
    s.control(va, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HPANE-B'; cat > input-b"}))?;
    let pane_b = s.relation(va, "fux::model::Focused")?;
    ensure(pane_a != pane_b, "split did not create a second pane")?;
    s.wait("raw pane B ready", |s| {
        Ok(s.frame(va, 24, 80)?.contains("PANE-B"))
    })?;
    s.send(a, format!("{token}-B").as_bytes())?;
    // PTY input and BRP use different transports. Acknowledge delivery before
    // changing focus; otherwise later bytes legitimately belong to the new pane.
    s.wait("pane B input delivered", |s| {
        Ok(fs::read(s.directory.join("input-b"))
            .is_ok_and(|v| v == format!("{token}-B").as_bytes()))
    })?;
    s.control(va, json!({"kind":"focus","pane":pane_a}))?;
    s.resize(a, 12, 41)?;
    s.send(a, format!("{token}-A").as_bytes())?;
    s.wait("input isolation", |s| {
        Ok(fs::read(s.directory.join("input-a"))
            .is_ok_and(|v| v == format!("{token}-A").as_bytes())
            && fs::read(s.directory.join("input-b"))
                .is_ok_and(|v| v == format!("{token}-B").as_bytes()))
    })?;
    Ok(())
}

// Use a larger emulator so an out-of-bounds paint cannot be silently clipped
// into a passing assertion by the oracle itself.
fn check_frame_bounds(paint: &[u8], rows: u16, cols: u16) -> Result<()> {
    let mut parser = vt100::Parser::new(rows + 1, cols + 1, 0);
    parser.process(paint);
    let screen = parser.screen();
    for y in 0..=rows {
        for x in 0..=cols {
            let cell = screen.cell(y, x).ok_or("missing frame cell")?;
            if y == rows || x == cols {
                ensure(
                    cell.contents().trim().is_empty() && cell.bgcolor() == vt100::Color::Default,
                    "frame painted beyond requested viewport",
                )?;
            }
            if y == rows - 1 && x < cols {
                ensure(
                    cell.bgcolor() == vt100::Color::Idx(8),
                    "bottom bar does not fill requested viewport",
                )?;
            }
        }
    }
    Ok(())
}

fn lifecycle(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let viewer = s.frontend(f)?.viewer;
    let initial = running(s)?;
    child_command(s, f, "sleep 60 & echo $! > background.pid")?;
    s.wait("background child pid", |s| {
        Ok(fs::read_to_string(s.directory.join("background.pid"))
            .is_ok_and(|v| v.trim().parse::<i32>().is_ok()))
    })?;
    let background: i32 = fs::read_to_string(s.directory.join("background.pid"))?
        .trim()
        .parse()?;
    let shell = *initial.first().ok_or("no initial child")?;
    ensure(
        nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(background)))?.as_raw() != shell,
        "background job did not get a separate group",
    )?;
    s.journal.record("background_group", json!({"shell":shell,"background":background,"shell_kind":"explicit /bin/bash --noprofile --norc -i wrapper"}))?;
    s.control(viewer, json!({"kind":"terminate"}))?;
    s.wait("shell and background job cleanup", |_| {
        Ok(!alive(shell) && !alive(background))
    })?;
    s.wait("published exit", |s| {
        Ok(states(s)?.iter().all(|(_, state)| {
            state.pointer("/status/kind") == Some(&json!("exited"))
                && state.pointer("/status/pid").is_none()
        }))
    })?;

    s.control(
        viewer,
        json!({"kind":"split","axis":"horizontal","program":"printf 'NATURAL-EXIT'; exit 7"}),
    )?;
    s.wait("natural exit retained", |s| {
        Ok(states(s)?
            .iter()
            .any(|(_, state)| state.pointer("/status/code") == Some(&json!(7)))
            && s.frame(viewer, 24, 80)?.contains("NATURAL-EXIT"))
    })?;
    s.control(
        viewer,
        json!({"kind":"split","axis":"vertical","program":"exec /bin/sh"}),
    )?;
    let mut live = Vec::new();
    s.wait("survivor running", |s| {
        live = states(s)?
            .iter()
            .filter_map(|(_, state)| pid(state).ok())
            .collect();
        Ok(!live.is_empty())
    })?;
    detach(s, f)?;
    ensure(
        live.iter().all(|p| alive(*p)),
        "graceful detach killed shared process",
    )?;
    let f = s.attach(24, 80)?;
    let viewer = s.frontend(f)?.viewer;
    let workspace = s.relation(viewer, "fux::model::Viewing")?;
    s.journal.record("kill_frontend", json!({"frontend":f}))?;
    s.frontend(f)?.stop()?;
    // A new paint makes closed-stream detection observable without assuming a
    // recurring idle repaint, and does not require the lost viewer to exist.
    s.rpc(
        "world.insert_components",
        json!({"entity":workspace,"components":{"bevy_ecs::name::Name":"disconnect-proof"}}),
    )?;
    s.wait("abrupt viewer removed", |s| {
        Ok(!s
            .query(VIEWER)?
            .iter()
            .any(|row| id(row).ok() == Some(viewer)))
    })?;
    ensure(
        live.iter().all(|p| alive(*p)),
        "abrupt viewer loss killed shared process",
    )?;
    s.signal_shutdown()?;
    s.wait_server_exit(false)?;
    ensure(
        live.iter().all(|p| !alive(*p)),
        "server shutdown left an owned process",
    )
}

fn output_shutdown(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let viewer = s.frontend(f)?.viewer;
    let initial = running(s)?;
    s.frontend(f)?.paused = true;
    s.journal
        .record("pause_outer_pty_reads", json!({"frontend":f}))?;
    s.control(
        viewer,
        json!({"kind":"split","axis":"vertical","program":"exec /usr/bin/yes OUTPUT"}),
    )?;
    let mut all = Vec::new();
    s.wait("noisy child alive while outer PTY unread", |s| {
        all = states(s)?
            .iter()
            .filter_map(|(_, state)| pid(state).ok())
            .collect();
        Ok(all.len() > initial.len() && s.frame(viewer, 24, 80)?.contains("OUTPUT"))
    })?;
    // Bounded requests keep the workload active without a timing-only sleep.
    for _ in 0..40 {
        s.rpc("fux.frame", json!({"viewer":viewer}))?;
    }
    s.signal_shutdown()?;
    s.wait_server_exit(false)?;
    ensure(
        all.iter().all(|p| !alive(*p)),
        "output shutdown left an owned process",
    )?;
    s.frontend(f)?.paused = false;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_oracle_detects_clipped_overflow_and_missing_bar() -> Result<()> {
        let good = b"\x1b[2;1H\x1b[48;5;8m  \x1b[0m";
        check_frame_bounds(good, 2, 2)?;
        let mut overflow = good.to_vec();
        overflow.extend_from_slice(b"\x1b[3;1HX");
        assert!(check_frame_bounds(&overflow, 2, 2).is_err());
        assert!(check_frame_bounds(b"", 2, 2).is_err());
        Ok(())
    }
}
