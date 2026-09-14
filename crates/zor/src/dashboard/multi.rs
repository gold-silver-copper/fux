//! Machine-attributed navigation over independently refreshed service observations.
use super::handoff::{Action, Outcome, Pending, Subject};
use crate::machines::{
    Catalog,
    supervision::{MachineSource, Observation, Selection, Source, Supervision},
};
use anyhow::{Context, Result};
use std::{
    io::Read,
    os::fd::AsFd,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub struct Reload {
    pub catalog_path: PathBuf,
    pub directory: Option<PathBuf>,
    pub koh_binary: PathBuf,
}
#[derive(Default)]
pub struct Notices {
    pub bell: bool,
    pub notify: bool,
    pub command: Option<PathBuf>,
}
impl Reload {
    fn start(&self) -> Result<std::sync::mpsc::Receiver<Result<Vec<MachineSource>>>> {
        let path = self.catalog_path.clone();
        let directory = self.directory.clone();
        let koh_binary = self.koh_binary.clone();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("zor-catalog-reload".into())
            .spawn(move || {
                let result = Catalog::load(&path)
                    .and_then(|catalog| sources(directory, catalog, koh_binary));
                let _ = send.send(result);
            })?;
        Ok(receive)
    }
}

pub fn sources(
    directory: Option<PathBuf>,
    catalog: Catalog,
    koh_binary: PathBuf,
) -> Result<Vec<MachineSource>> {
    if let Some(directory) = &directory {
        anyhow::ensure!(
            directory.is_absolute(),
            "local service directory must be absolute"
        );
    }
    let local = match directory.map_or_else(crate::service::directory, Ok) {
        Ok(directory) => Source::Local {
            socket: directory.join("control.sock"),
        },
        Err(error) => Source::Unavailable {
            reason: format!("Local service location unavailable: {error:#}"),
        },
    };
    let mut sources = vec![MachineSource {
        control: Default::default(),
        attachments: Default::default(),
        id: "local".into(),
        name: "Local".into(),
        source: local,
    }];
    for machine in catalog.machines {
        sources.push(MachineSource {
            control: Default::default(),
            attachments: machine.attachments,
            id: machine.id,
            name: machine.name,
            source: machine.control.map_or_else(
                || Source::Unavailable {
                    reason: "No control binding; configure machine control".into(),
                },
                |binding| Source::Remote {
                    binding,
                    koh_binary: koh_binary.clone(),
                },
            ),
        });
    }
    Ok(sources)
}

fn entries<'a>(
    observations: &'a [Observation],
    scope: Option<&str>,
) -> Vec<(&'a Observation, &'a super::Row)> {
    observations
        .iter()
        .filter(|item| scope.is_none_or(|id| item.machine_id == id))
        .flat_map(|item| {
            item.view
                .iter()
                .flat_map(move |view| view.rows.iter().map(move |row| (item, row)))
        })
        .collect()
}
fn clean(value: &str, width: usize) -> String {
    value
        .chars()
        .take(width)
        .map(|ch| {
            if ch.is_ascii_graphic() || ch == ' ' {
                ch
            } else {
                '?'
            }
        })
        .collect()
}
const HELP: &str = "Dashboard controls\nTab: cycle machines and All machines\nj/k: select a row\nEnter/i: inspect task or observed agent\na: attach exact pane; Ctrl-A d detaches\nr: task result\ne: inspect full status/error\nc: cancel task coordination\ns: stop managed task\nl: reconcile retained task evidence\nu: resume task; enter stable operation ID and explicit fux incarnation\nU: inspect saved/remote resume operation; read only\nR: reload machine catalog\nEsc: cancel preparation or close detail\nq: close detail or quit dashboard\nActions require fresh evidence.\nTask-only actions refuse observed agents.";

fn resume_status_arguments(text: &str) -> Result<Action> {
    let operation = text.trim();
    anyhow::ensure!(
        crate::tasks::model::id(operation),
        "enter one valid OPERATION_ID"
    );
    Ok(Action::ResumeStatus {
        operation: operation.into(),
    })
}

fn resume_arguments(text: &str) -> Result<Action> {
    let parts: Vec<_> = text.split_whitespace().collect();
    let [operation, instance] = parts.as_slice() else {
        anyhow::bail!("enter OPERATION_ID FUX_INSTANCE");
    };
    anyhow::ensure!(crate::tasks::model::id(operation), "invalid operation ID");
    anyhow::ensure!(
        !instance.is_empty()
            && instance.len() <= 256
            && instance
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
        "invalid fux incarnation"
    );
    Ok(Action::Resume {
        operation: (*operation).into(),
        instance: (*instance).into(),
    })
}

fn wrapped(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    text.lines()
        .flat_map(|line| {
            let line = clean(line, usize::MAX);
            if line.is_empty() {
                return vec![String::new()];
            }
            let mut result = Vec::new();
            let mut rest = line.as_str();
            while rest.len() > width {
                let split = rest
                    .get(..width)
                    .unwrap_or(rest)
                    .rfind(' ')
                    .filter(|index| *index > 0)
                    .map_or(width, |index| index + 1);
                let (part, remaining) = rest.split_at(split);
                result.push(part.into());
                rest = remaining;
            }
            result.push(rest.into());
            result
        })
        .collect()
}

fn narrow(
    observations: &[Observation],
    scope: Option<&str>,
    selection: Option<&Selection>,
    problem: &str,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let now = Instant::now();
    let scope_name = scope
        .and_then(|id| observations.iter().find(|item| item.machine_id == id))
        .map_or("All machines", |item| item.machine_name.as_str());
    let mut lines = vec![format!("zor dashboard | {scope_name}")];
    lines.extend(wrapped(
        if width < 60 {
            "Tab machines | j/k rows\n? help | q quit"
        } else {
            "Tab machines | j/k rows | ? help | q quit"
        },
        width,
    ));
    if !problem.is_empty() {
        lines.push("e: full status/error".into());
    }
    let live = observations.iter().filter(|item| item.fresh(now)).count();
    lines.push(format!("{live}/{} machines live", observations.len()));
    let entries = entries(observations, scope);
    let selected = entries
        .iter()
        .position(|(item, row)| item.selection(&row.key).as_ref() == selection);
    let mut footer = Vec::new();
    if !problem.is_empty() {
        footer.extend(wrapped(problem, width));
    }
    if let Some(item) = observations
        .iter()
        .find(|item| scope.is_none_or(|id| item.machine_id == id) && item.problem.is_some())
    {
        footer.extend(wrapped(
            &format!(
                "{}: {}",
                item.machine_name,
                item.problem.as_deref().unwrap_or_default()
            ),
            width,
        ));
    }
    if let Some((item, row)) = selected.and_then(|index| entries.get(index)) {
        footer.extend(wrapped(
            &format!("{}: {}", item.machine_name, row.detail),
            width,
        ));
    }
    footer.truncate((height / 3).max(1));
    let page = height.saturating_sub(lines.len() + footer.len()).max(1);
    let start = selected.unwrap_or(0) / page * page;
    for (index, (item, row)) in entries.iter().enumerate().skip(start).take(page) {
        lines.push(format!(
            "{} {} [{}] {}",
            if Some(index) == selected { ">" } else { " " },
            item.machine_name,
            if item.row_fresh(row, now) {
                &row.status
            } else {
                "stale"
            },
            row.label
        ));
    }
    if entries.is_empty() {
        lines.push("No observed rows in this scope".into());
    }
    lines.extend(footer);
    let mut frame = String::from("\x1b[H");
    for (index, line) in lines.iter().take(height).enumerate() {
        frame.push_str(&clean(line, width));
        frame.push_str("\x1b[K");
        if index + 1 < height {
            frame.push_str("\r\n");
        }
    }
    frame.push_str("\x1b[J");
    frame.into_bytes()
}
fn paint(
    observations: &[Observation],
    scope: Option<&str>,
    selection: Option<&Selection>,
    problem: &str,
    cols: usize,
    rows: usize,
) -> Vec<u8> {
    let now = Instant::now();
    let width = cols.saturating_sub(1).clamp(1, 200);
    let height = rows.clamp(1, 80);
    if width < 100 {
        return narrow(observations, scope, selection, problem, width, height);
    }
    let scope_name = scope
        .and_then(|id| observations.iter().find(|item| item.machine_id == id))
        .map_or("All machines", |item| item.machine_name.as_str());
    let mut lines = vec![format!(
        "zor dashboard | {scope_name} | Tab: machine  j/k: row  Enter: inspect  a: attach  r: result  c: cancel  s: stop  l: reconcile  u: resume  U: status  e: error  R: reload  q: quit"
    )];
    lines.push(
        observations
            .iter()
            .map(|item| {
                format!(
                    "{}:{}",
                    item.machine_name,
                    if item.fresh(now) {
                        "live"
                    } else if item.view.is_some() {
                        "stale"
                    } else {
                        "unavailable"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("  "),
    );
    let entries = entries(observations, scope);
    let selected = entries
        .iter()
        .position(|(item, row)| item.selection(&row.key).as_ref() == selection);
    let problem_lines = wrapped(problem, width);
    let page = height.saturating_sub(5 + problem_lines.len()).max(1);
    let start = selected.unwrap_or(0) / page * page;
    for (index, (item, row)) in entries.iter().enumerate().skip(start).take(page) {
        lines.push(format!(
            "{} {:16} {:12} {}",
            if Some(index) == selected { ">" } else { " " },
            item.machine_name,
            if item.row_fresh(row, now) {
                &row.status
            } else {
                "stale"
            },
            row.label
        ));
    }
    if entries.is_empty() {
        lines.push("No observed rows in this scope".into());
    }
    if let Some((item, row)) = selected.and_then(|index| entries.get(index)) {
        lines.push(format!("{}: {}", item.machine_name, row.detail));
    }
    if !problem.is_empty() {
        lines.extend(problem_lines);
    }
    if let Some(item) = observations
        .iter()
        .find(|item| scope.is_none_or(|id| item.machine_id == id) && item.problem.is_some())
    {
        lines.push(format!(
            "{}: {}",
            item.machine_name,
            item.problem.as_deref().unwrap_or_default()
        ));
    }
    let mut frame = String::from("\x1b[H");
    for (index, line) in lines.iter().take(height).enumerate() {
        frame.push_str(&clean(line, width));
        frame.push_str("\x1b[K");
        if index + 1 < height {
            frame.push_str("\r\n");
        }
    }
    frame.push_str("\x1b[J");
    frame.into_bytes()
}

fn paint_resume(text: &str, cols: usize, rows: usize) -> Vec<u8> {
    // Input is bounded ASCII. Keep its editing tail above explanatory text on narrow screens.
    let visible = cols
        .saturating_sub(1)
        .clamp(1, 200)
        .saturating_mul(3)
        .saturating_sub(4);
    let start = text.len().saturating_sub(visible);
    let tail = text.get(start..).unwrap_or(text);
    let marker = if start > 0 { "..." } else { ">" };
    paint_detail_title(
        &format!(
            "Resume selected task\nType: OPERATION_ID FUX_INSTANCE\n{marker} {tail}\nBackspace: edit\nKeep the stable operation ID to reconcile an unknown outcome.\nService checks provider/session eligibility; no prompt replay."
        ),
        0,
        cols,
        rows,
        Some("Resume | Enter submit | Esc cancel"),
    )
}

fn paint_detail(text: &str, offset: usize, cols: usize, rows: usize) -> Vec<u8> {
    paint_detail_title(text, offset, cols, rows, None)
}
fn paint_detail_title(
    text: &str,
    offset: usize,
    cols: usize,
    rows: usize,
    title: Option<&str>,
) -> Vec<u8> {
    let width = cols.saturating_sub(1).clamp(1, 200);
    let height = rows.clamp(1, 80);
    let mut lines = vec![
        if let Some(title) = title {
            title
        } else if width < 40 {
            "j/k scroll | Esc back"
        } else {
            "Inspection/result | j/k scroll | Esc back"
        }
        .to_owned(),
    ];
    lines.extend(
        wrapped(text, width)
            .into_iter()
            .skip(offset)
            .take(height.saturating_sub(1)),
    );
    let mut frame = String::from("\x1b[H");
    for (index, line) in lines.iter().take(height).enumerate() {
        frame.push_str(&clean(line, width));
        frame.push_str("\x1b[K");
        if index + 1 < height {
            frame.push_str("\r\n");
        }
    }
    frame.push_str("\x1b[J");
    frame.into_bytes()
}

pub fn run(
    mut sources: Vec<MachineSource>,
    once: bool,
    fux_binary: PathBuf,
    initial_scope: Option<String>,
    reload: Reload,
    notices: Notices,
) -> Result<u8> {
    anyhow::ensure!(
        initial_scope
            .as_ref()
            .is_none_or(|id| sources.iter().any(|source| &source.id == id)),
        "selected machine is not in the supervision catalog"
    );
    let mut supervision = Supervision::start(sources.clone())?;
    if once {
        let deadline = Instant::now() + Duration::from_secs(7);
        loop {
            let observations = supervision.observations()?;
            if observations
                .iter()
                .all(|item| item.problem.as_deref() != Some("Connecting"))
                || Instant::now() >= deadline
            {
                let now = Instant::now();
                let machines: Vec<_> = observations.iter().map(|item| serde_json::json!({
                    "id":item.machine_id,"name":item.machine_name,"view":item.view,"fresh":item.fresh(now),
                    "problem":item.problem,"observed_age_ms":item.observed_at.map(|at| now.saturating_duration_since(at).as_millis())
                })).collect();
                // Intended stdout surface: the `--once` machine/view JSON envelope.
                #[allow(clippy::print_stdout)]
                {
                    println!("{}", serde_json::json!({"machines":machines}));
                }
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let _signals = super::handoff::Signals::new(&stop)?;
    let mut screen = Some(super::terminal::Screen::open(&stop)?);
    let mut input = std::fs::File::from(std::io::stdin().as_fd().try_clone_to_owned()?);
    let mut pending: Option<Pending> = None;
    let mut detail: Option<String> = None;
    let mut resume_input: Option<(Selection, String, bool)> = None;
    let mut detail_offset = 0usize;
    let mut scope = initial_scope;
    let mut selected: Option<Selection> = None;
    let mut remembered = std::collections::BTreeMap::<Option<String>, Selection>::new();
    let mut problem = String::new();
    let mut last = Vec::new();
    let mut loading = None;
    let mut attention = super::attention::Machines::new();
    let mut delivery = notices
        .notify
        .then(|| crate::platform::notification::Delivery::new(notices.command.clone()));
    let mut notification_problem = None;
    let result = (|| -> Result<u8> {
        while !stop.load(Ordering::Acquire) {
            if let Some(receive) = &loading {
                let receive: &std::sync::mpsc::Receiver<Result<Vec<MachineSource>>> = receive;
                match receive.try_recv() {
                    Ok(result) => {
                        loading = None;
                        match result.and_then(|next| supervision.reload(&mut sources, next)) {
                            Ok(()) => {
                                remembered.retain(|_, selection| {
                                    sources
                                        .iter()
                                        .any(|source| source.id == selection.machine_id)
                                });
                                if scope.as_ref().is_some_and(|id| {
                                    !sources.iter().any(|source| &source.id == id)
                                }) {
                                    scope = None;
                                }
                                detail = None;
                                problem = "Machine catalog reloaded".into();
                            }
                            Err(error) => {
                                problem =
                                    format!("Reload failed; existing profiles retained: {error:#}");
                            }
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        loading = None;
                        problem = "Catalog reload worker failed; existing profiles retained".into();
                    }
                }
            }
            if let Some(result) = pending.as_ref().and_then(Pending::poll) {
                let action_selection = pending.as_ref().map(|job| job.selection.clone());
                let action_label = pending
                    .as_ref()
                    .map(|job| job.label.clone())
                    .unwrap_or_default();
                let cancelled = pending.as_ref().is_some_and(Pending::cancelled);
                let mutation_started = pending.as_ref().is_some_and(Pending::mutation_started);
                pending.take();
                match result {
                    Ok(Outcome::Detail(text)) if !cancelled && selected == action_selection => {
                        detail = Some(text);
                        detail_offset = 0;
                        problem.clear();
                    }
                    Ok(Outcome::Detail(_)) => {
                        problem = format!(
                            "{action_label}: action finished; inspect this task for its current result"
                        );
                    }
                    Ok(Outcome::Viewer(viewer)) if !cancelled && selected == action_selection => {
                        super::handoff::flush_input()?;
                        drop(delivery.take());
                        drop(screen.take());
                        let outcome = viewer.run(&fux_binary, &stop);
                        super::handoff::flush_input()?;
                        screen = Some(super::terminal::Screen::open(&stop)?);
                        delivery = notices.notify.then(|| {
                            crate::platform::notification::Delivery::new(notices.command.clone())
                        });
                        last.clear();
                        detail = None;
                        problem = match outcome {
                            Ok(()) => "Detached; refreshing the selection".into(),
                            Err(error) => format!("Attachment ended: {error:#}"),
                        };
                    }
                    Ok(Outcome::Viewer(_)) => {
                        problem =
                            "Selection changed or preparation cancelled; attachment discarded"
                                .into();
                    }
                    Err(error) => {
                        problem = if mutation_started {
                            format!(
                                "{action_label}: outcome unconfirmed; inspect task before retry: {error:#}"
                            )
                        } else if cancelled {
                            format!("{action_label}: preparation cancelled; no task action sent")
                        } else {
                            format!("{action_label}: action failed: {error:#}")
                        };
                    }
                }
            }
            let observations = supervision.observations()?;
            let now = Instant::now();
            let fresh_attention = attention.observe(&observations, now);
            if let Some(delivery) = &mut delivery
                && let Some(result) = delivery.poll()
            {
                notification_problem = result
                    .err()
                    .map(|error| format!("Notification failed: {error:#}"));
            }
            if (notices.bell || delivery.is_some())
                && delivery.as_ref().is_none_or(|delivery| !delivery.busy())
                && let Some(count) = attention.take(&fresh_attention, now)
            {
                if notices.bell {
                    screen
                        .as_mut()
                        .context("dashboard screen missing")?
                        .write(b"\x07")?;
                }
                if let Some(delivery) = &mut delivery
                    && let Err(error) = delivery.start("Zor needs attention", &format!("{count} new attention entries across saved machines. Open zor dashboard for details.")) {
                        notification_problem = Some(format!("Notification failed: {error:#}"));
                }
            }
            let rows = entries(&observations, scope.as_deref());
            // A missing or replaced identity is never silently rebound to a same-named row.
            if selected.is_none() {
                selected = rows
                    .first()
                    .and_then(|(item, row)| item.selection(&row.key));
            }
            let size = crate::platform::winsize(1);
            let frame = if let Some((_, text, read_only)) = &resume_input {
                if *read_only {
                    paint_detail_title(
                        &format!(
                            "Inspect resume operation\nType: OPERATION_ID\n> {text}\nRead only; no resume request is sent."
                        ),
                        0,
                        usize::from(size.cols),
                        usize::from(size.rows),
                        Some("Status | Enter read | Esc cancel"),
                    )
                } else {
                    paint_resume(text, usize::from(size.cols), usize::from(size.rows))
                }
            } else if let Some(detail) = &detail {
                paint_detail(
                    detail,
                    detail_offset,
                    usize::from(size.cols),
                    usize::from(size.rows),
                )
            } else {
                paint(
                    &observations,
                    scope.as_deref(),
                    selected.as_ref(),
                    &notification_problem
                        .as_ref()
                        .map_or_else(|| problem.clone(), |notice| format!("{problem} {notice}")),
                    usize::from(size.cols),
                    usize::from(size.rows),
                )
            };
            if frame != last {
                screen
                    .as_mut()
                    .context("dashboard screen missing")?
                    .write(&frame)?;
                last = frame;
            }
            let mut polls = [nix::poll::PollFd::new(
                input.as_fd(),
                nix::poll::PollFlags::POLLIN,
            )];
            match nix::poll::poll(&mut polls, 100_u16) {
                Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            let mut bytes = [0; 64];
            let count = input.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            for byte in bytes.iter().take(count) {
                let mut resume_action = None;
                let mut dispatched_byte = *byte;
                if let Some((selection, text, read_only)) = &mut resume_input {
                    match byte {
                        27 => {
                            resume_input = None;
                            break;
                        }
                        3 | 4 => {
                            stop.store(true, Ordering::Release);
                            break;
                        }
                        8 | 127 => {
                            text.pop();
                            continue;
                        }
                        b'\r' | b'\n' => {
                            if selected.as_ref() != Some(selection) {
                                problem = "Resume selection changed; select a current row".into();
                                resume_input = None;
                                break;
                            }
                            match if *read_only {
                                resume_status_arguments(text)
                            } else {
                                resume_arguments(text)
                            } {
                                Ok(action) => resume_action = Some(action),
                                Err(error) => {
                                    problem = format!("Resume unavailable: {error:#}");
                                    resume_input = None;
                                    break;
                                }
                            }
                            resume_input = None;
                            dispatched_byte = b'u';
                        }
                        32..=126 if text.len() < 512 => {
                            text.push(char::from(*byte));
                            continue;
                        }
                        _ => continue,
                    }
                }
                let byte = &dispatched_byte;
                if detail.is_some() {
                    match byte {
                        3 | 4 => {
                            stop.store(true, Ordering::Release);
                            break;
                        }
                        27 | b'q' => {
                            detail = None;
                            break;
                        }
                        b'j' => {
                            let maximum = detail.as_ref().map_or(0, |text| {
                                wrapped(text, usize::from(size.cols).saturating_sub(1))
                                    .len()
                                    .saturating_sub(usize::from(size.rows).saturating_sub(1).max(1))
                            });
                            detail_offset = detail_offset.saturating_add(1).min(maximum);
                        }
                        b'k' => detail_offset = detail_offset.saturating_sub(1),
                        _ => {}
                    }
                    continue;
                }
                match byte {
                    b'e' => {
                        detail = Some(if problem.is_empty() {
                            "No action error or status".into()
                        } else {
                            problem.clone()
                        });
                        detail_offset = 0;
                        break;
                    }
                    b'?' => {
                        detail = Some(HELP.into());
                        detail_offset = 0;
                        break;
                    }
                    b'R' => {
                        if pending.is_some() {
                            problem =
                                "Finish or cancel the pending action before reloading profiles"
                                    .into();
                        } else if loading.is_some() {
                            problem = "Catalog reload is already pending".into();
                        } else {
                            match reload.start() {
                                Ok(receive) => {
                                    loading = Some(receive);
                                    problem = "Reloading machine catalog...".into();
                                }
                                Err(error) => problem = format!("Catalog reload failed: {error:#}"),
                            }
                        }
                        break;
                    }
                    b'q' | 3 | 4 => {
                        stop.store(true, Ordering::Release);
                        break;
                    }
                    b'\t' => {
                        if let Some(selection) = selected.take() {
                            remembered.insert(scope.clone(), selection);
                        }
                        scope = match scope.as_ref().and_then(|id| {
                            observations.iter().position(|item| &item.machine_id == id)
                        }) {
                            None => observations.first().map(|item| item.machine_id.clone()),
                            Some(index) => observations
                                .get(index + 1)
                                .map(|item| item.machine_id.clone()),
                        };
                        selected = remembered.get(&scope).cloned();
                        problem.clear();
                        break;
                    }
                    b'j' | b'k' => {
                        let index = rows
                            .iter()
                            .position(|(item, row)| {
                                item.selection(&row.key).as_ref() == selected.as_ref()
                            })
                            .unwrap_or(0);
                        let next = if *byte == b'j' {
                            (index + 1).min(rows.len().saturating_sub(1))
                        } else {
                            index.saturating_sub(1)
                        };
                        selected = rows
                            .get(next)
                            .and_then(|(item, row)| item.selection(&row.key));
                    }
                    b'\r' | b'\n' | b'i' | b'r' | b'a' | b'c' | b's' | b'l' | b'u' | b'U' => {
                        if loading.is_some() {
                            problem = "Wait for catalog reload before starting an action".into();
                            break;
                        }
                        if pending.is_some() {
                            problem = "An action is pending; it will not be sent again".into();
                            break;
                        }
                        if matches!(*byte, b'u' | b'U') && resume_action.is_none() {
                            if let Some(selection) = &selected {
                                resume_input =
                                    Some((selection.clone(), String::new(), *byte == b'U'));
                            } else {
                                problem = "Select a task before resuming".into();
                            }
                            break;
                        }
                        let request = (|| -> Result<Pending> {
                            let (item, row) = rows
                                .iter()
                                .find(|(item, row)| {
                                    item.selection(&row.key).as_ref() == selected.as_ref()
                                })
                                .context("selection no longer exists; select a current row")?;
                            anyhow::ensure!(
                                item.row_fresh(row, Instant::now()),
                                "evidence stale; wait for a fresh observation"
                            );
                            let subject = Subject::row(row)?;
                            let source = sources
                                .iter()
                                .find(|source| source.id == item.machine_id)
                                .context("machine profile unavailable")?
                                .clone();
                            let selection = selected.clone().context("selection missing")?;
                            let action = match byte {
                                b'r' => Action::Result,
                                b'a' => Action::Attach,
                                b'c' => Action::Cancel,
                                b's' => Action::Stop,
                                b'l' => Action::Reconcile,
                                b'u' => resume_action.context("resume arguments missing")?,
                                _ => Action::Inspect,
                            };
                            Pending::start(
                                source,
                                selection,
                                subject,
                                action,
                                crate::machines::intents::path(&reload.catalog_path)?,
                            )
                        })();
                        match request {
                            Ok(job) => {
                                problem = format!("{}: preparing action...", job.label);
                                pending = Some(job);
                            }
                            Err(error) => problem = format!("Action unavailable: {error:#}"),
                        }
                        break;
                    }
                    27 => {
                        if let Some(job) = &pending {
                            job.cancel();
                            problem =
                                "Cancellation requested; an already sent action may still complete"
                                    .into();
                        } else {
                            problem.clear();
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
        Ok(0)
    })();
    drop(delivery);
    drop(screen);
    drop(pending);
    drop(supervision);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_form_never_constructs_a_mutation() -> Result<()> {
        for invalid in ["", "op instance", "../op", "op\u{1b}"] {
            assert!(resume_status_arguments(invalid).is_err());
        }
        assert!(
            matches!(resume_status_arguments("resume-one")?, Action::ResumeStatus { operation } if operation == "resume-one")
        );
        Ok(())
    }

    #[test]
    fn narrow_resume_keeps_input_tail_visible() -> Result<()> {
        let text = format!("{}END-OF-INPUT", "a".repeat(490));
        let frame = String::from_utf8(paint_resume(&text, 40, 16))?;
        assert!(frame.contains("END-OF-INPUT"));
        assert!(frame.contains("Enter submit | Esc cancel"));
        assert!(frame.contains("Resume selected task"));
        Ok(())
    }

    #[test]
    fn resume_form_requires_explicit_bounded_identifiers() -> Result<()> {
        for invalid in [
            "",
            "operation",
            "op instance extra",
            "../op instance",
            "op ../instance",
            "op \u{1b}[31m",
        ] {
            assert!(resume_arguments(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(resume_arguments(&format!("op {}", "a".repeat(257))).is_err());
        let Action::Resume {
            operation,
            instance,
        } = resume_arguments("resume-one fux-123")?
        else {
            anyhow::bail!("wrong action");
        };
        assert_eq!(operation, "resume-one");
        assert_eq!(instance, "fux-123");
        Ok(())
    }

    #[test]
    fn narrow_errors_and_detail_tails_are_reachable_without_horizontal_clipping() {
        let error = "Builder: no attachment binding for workspace agent; use machine bind";
        let frame = paint(&[], None, None, error, 40, 16);
        let mut parser = vt100::Parser::new(16, 40, 0);
        parser.process(&frame);
        let contents = parser.screen().contents();
        assert!(contents.contains("? help"));
        assert!(contents.contains("q quit"));
        assert!(
            contents
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(error)
        );
        let text = format!("{}VISIBLE_END", "x".repeat(200));
        let lines = wrapped(&text, 39);
        let offset = lines.len().saturating_sub(2);
        let mut parser = vt100::Parser::new(3, 40, 0);
        parser.process(&paint_detail(&text, offset, 40, 3));
        assert!(parser.screen().contents().contains("VISIBLE_END"));
        assert!(parser.screen().contents().contains("Esc back"));
    }
    #[test]
    fn machine_scope_and_narrow_render_keep_unavailability_visible() {
        let observations = vec![
            Observation {
                machine_id: "local".into(),
                machine_name: "Local".into(),
                view: None,
                observed_at: None,
                problem: Some("offline".into()),
            },
            Observation {
                machine_id: "remote".into(),
                machine_name: "Builder".into(),
                view: None,
                observed_at: None,
                problem: Some("No control binding".into()),
            },
        ];
        let frame = paint(&observations, Some("remote"), None, "", 100, 10);
        let mut parser = vt100::Parser::new(10, 100, 0);
        parser.process(&frame);
        let contents = parser.screen().contents();
        assert!(contents.contains("Builder"));
        assert!(contents.contains("No control binding"));
        assert!(!contents.contains("Local: offline"));
        let frame = paint(&observations, None, None, "", 15, 2);
        let mut parser = vt100::Parser::new(2, 15, 0);
        parser.process(&frame);
        assert!(parser.screen().contents().starts_with("zor dashboard"));
    }
}
