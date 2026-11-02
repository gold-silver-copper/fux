//! The server's copy of each pane (D-120): what the pane's own process
//! (`host`) last told it, which `get`, listings, waits and every paint
//! read. It is never a second truth: it is behind its pane, never ahead of
//! it, and changes with the journal in one step (`Session::told`): each of
//! the pane's events the journal holds is in it, and none it does not.
//!
//! Its screen is a terminal of its own (`mirror`), the pane's visible
//! screen painted onto it as the pane sends it, a frame at a time; its
//! facts are the pane's own observation of itself.
use crate::journal::Told;
use crate::link::{Hosting, Update};
use crate::pane::{Runs, Said};
use fux_vt::{Screen, Size};
use fux_wire::answer::Ended;
use fux_wire::code::{Error, Raise, fail};
use fux_wire::command::{Counts, PaneOptions, Run, Version};
use fux_wire::id::PaneId;
use fux_wire::inspect::Facts;
use fux_wire::json::Json;

/// How far along its life a pane's program is, as the pane told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    /// It closed its terminal and runs on.
    HungUp,
    /// Reading its terminal failed, or the pane's own process ended, so.
    ReadFailed(String),
    Exited(Ended),
}

/// A pane, as the server has it (see the module documentation).
pub struct Copy {
    id: PaneId,
    /// Given by `name` or `rename`, and kept across respawns: the server's
    /// own.
    name: Option<String>,
    title: Said,
    pub runs: Runs,
    options: PaneOptions,
    /// How many times its program was started again.
    pub respawns: u64,
    /// It was asked to respawn, and has not said it did: a program at the
    /// next generation may be its already.
    pub respawning: bool,
    counts: Counts,
    state: State,
    facts: Facts,
    /// Its program's process, while it has one.
    pid: Option<fuxix::process::Pid>,
    mirror: fux_vt::Parser,
    /// What its host's terminal said, as it was last told it.
    pub hosting: Option<Hosting>,
    /// Whether a client shows it, as it was last told.
    pub shown: bool,
    /// The size it was told to take, till it tells it took it.
    asked: Option<Size>,
    /// How reading its program's terminal failed, if it did: kept past
    /// the program's end, to say why its pane closed.
    pub read_error: Option<String>,
}

/// What a copy's screen keeps of what its pane paints on it: its links,
/// and the cursor colour it sets (OSC 12).
const MIRROR: fux_vt::Options = fux_vt::Options::new()
    .with(fux_vt::Feature::Hyperlinks)
    .with(fux_vt::Feature::Palette)
    .with(fux_vt::Feature::Pictures);

/// What a mirror's paints say back: nothing, as no program reads them.
struct Discard;
impl fux_vt::Sink for Discard {}

impl Copy {
    /// Pane `id`, as it starts: `runs`, at `size`, with `options`, and
    /// nothing on its screen yet.
    pub fn new(
        id: PaneId,
        runs: Runs,
        size: Size,
        options: PaneOptions,
    ) -> Result<Copy, fux_vt::Error> {
        let launch = fux_wire::inspect::Launch {
            shell: matches!(runs, Runs::Shell(_)),
            cwd: crate::capture::path(runs.launch().cwd.as_os_str()),
        };
        Ok(Copy {
            id,
            name: None,
            title: Said::default(),
            runs,
            options,
            respawns: 0,
            respawning: false,
            counts: Counts::default(),
            state: State::Running,
            facts: Facts {
                program: Json::Null,
                launch,
                terminal: Vec::new(),
            },
            pid: None,
            mirror: fux_vt::Parser::with_options(size, 0, MIRROR)?,
            hosting: None,
            shown: false,
            asked: None,
            read_error: None,
        })
    }

    pub fn id(&self) -> PaneId {
        self.id
    }
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    /// Names it `name`: whether that is another.
    pub(crate) fn rename(&mut self, name: String) -> bool {
        let other = self.name.as_ref() != Some(&name);
        self.name = Some(name);
        other
    }
    pub fn title(&self) -> &Said {
        &self.title
    }
    pub fn options(&self) -> PaneOptions {
        self.options
    }
    pub fn size(&self) -> Size {
        self.screen().size()
    }
    /// Its visible screen, as last sent.
    pub fn screen(&self) -> &Screen {
        self.mirror.screen()
    }
    pub fn counts(&self) -> Counts {
        self.counts
    }
    /// Its version in the server run `run`.
    pub fn version(&self, run: Run) -> Version {
        Version {
            pane: self.id,
            counts: self.counts,
            run,
        }
    }
    pub fn state(&self) -> &State {
        &self.state
    }
    /// How its program ended, once it has.
    pub fn exit_status(&self) -> Option<Ended> {
        match self.state {
            State::Exited(ended) => Some(ended),
            State::Running | State::HungUp | State::ReadFailed(_) => None,
        }
    }
    /// Its program's process, while it has one.
    pub fn pid(&self) -> Option<fuxix::process::Pid> {
        self.pid
    }
    /// Its facts, as it told them.
    pub fn facts(&self) -> &Facts {
        &self.facts
    }
    /// Its terminal's `fact`, as it told it.
    pub fn terminal(&self, fact: fux_wire::inspect::Terminal) -> Json {
        let told = self.facts.terminal.iter().find(|(t, _)| *t == fact);
        told.map_or(Json::Null, |(_, json)| json.clone())
    }
    /// Whether a shell command was ever marked here (OSC 133 `C`).
    pub fn marked(&self) -> bool {
        self.screen().commands().started > 0
    }

    /// Why its program takes no input, if it takes none, as last told:
    /// what the pane itself checks as it is sent input (`host::refuses_input`).
    pub fn refuses_input(&self) -> Option<Error> {
        let why = match &self.state {
            State::Running => return None,
            State::HungUp => "its program closed its terminal: nothing reads input there",
            State::ReadFailed(error) => {
                return Some(fail(
                    Raise::NotRunning,
                    format!("{}: reading program terminal failed: {error}", self.id),
                ));
            }
            State::Exited(status) => return Some(crate::host::ended(self.id, *status)),
        };
        Some(fail(Raise::NotRunning, format!("{}: {why}", self.id)))
    }

    /// It told `told`, which left it at `counts`.
    pub(crate) fn told(&mut self, told: &Told, counts: Counts) {
        self.counts = counts;
        match told {
            Told::Title(title) => self.title = title.clone(),
            Told::Resized(size) => {
                if self.asked == Some(*size) {
                    self.asked = None;
                }
                self.resize(*size);
            }
            Told::HungUp => {
                self.state = State::HungUp;
                self.program(&[("state", Json::str("hung_up"))]);
            }
            // A terminal an ended program left behind failing leaves its
            // end as it was.
            Told::ReadFailed(_) if matches!(self.state, State::Exited(_)) => {}
            Told::ReadFailed(error) => {
                self.state = State::ReadFailed(error.clone());
                self.read_error = Some(error.clone());
                let error = Json::str(error.clone());
                self.program(&[("state", Json::str("read_failed")), ("read_error", error)]);
            }
            Told::Exited(end) => {
                self.state = State::Exited(end.ended);
                self.pid = None;
                let (exited, signal) = match end.ended {
                    Ended::Exited(code) => (Json::int(code), Json::Null),
                    Ended::Signalled(signal) => (Json::Null, Json::int(signal)),
                    Ended::Lost => (Json::Null, Json::Null),
                };
                self.program(&[
                    ("state", Json::str("ended")),
                    ("pid", Json::Null),
                    ("exited", exited),
                    ("signal", signal),
                    ("input_unread", Json::count(end.left.input)),
                    ("foreground_group", Json::Null),
                ]);
            }
            Told::Respawned { generation, .. } => {
                self.respawns = *generation;
                self.respawning = false;
                self.read_error = None;
                self.state = State::Running;
                self.title = Said::default();
            }
            Told::HistoryBudget(bytes) => {
                if let Some(bytes) = fux_wire::command::HistoryBytes::new(*bytes) {
                    self.options.history_bytes = bytes;
                }
            }
            Told::Renamed(_)
            | Told::Bell
            | Told::Notification(_)
            | Told::CommandStarted
            | Told::Done(_)
            | Told::HistoryCleared
            | Told::Input { .. }
            | Told::Signalled { .. } => {}
        }
    }

    /// Its program's facts, `fields` as an event it told says they are now:
    /// the record it sends with its screen says so too, after.
    fn program(&mut self, fields: &[(&str, Json)]) {
        if let Json::Object(program) = &mut self.facts.program {
            for (key, value) in fields {
                if let Some((_, at)) = program.iter_mut().find(|(k, _)| k == key) {
                    *at = value.clone();
                }
            }
        }
    }

    /// Its screen and facts, as it sent them.
    pub(crate) fn update(&mut self, update: Update) {
        self.counts = update.counts;
        self.title = update.title;
        self.pid = update.pid.and_then(fuxix::process::Pid::from_raw);
        self.facts = update.facts;
        if update.full || update.size != self.size() {
            match fux_vt::Parser::with_options(update.size, 0, MIRROR) {
                Ok(mirror) => self.mirror = mirror,
                Err(error) => log!("{}: its copy at {:?}: {error}", self.id, update.size),
            }
        }
        let graphics = fux_vt::Graphics::from(update.draws);
        if self.screen().graphics() != graphics {
            self.mirror.set_host_graphics(graphics);
        }
        self.mirror.set_picture_bytes(update.picture_bytes);
        let mut bytes = update.screen.as_slice();
        // Every byte read, budget or no: the bytes paint one screen.
        while let Ok(Some((taken, _))) = self.mirror.process_until_stop(bytes, &mut Discard) {
            bytes = bytes.get(taken..).unwrap_or_default();
            if bytes.is_empty() && !self.mirror.has_work_left() {
                break;
            }
        }
    }

    /// Whether it is to be told to take `size`: one it is not at, nor was
    /// told already.
    pub(crate) fn ask_size(&mut self, size: Size) -> bool {
        let told = self.asked.unwrap_or(self.size());
        if told == size {
            return false;
        }
        self.asked = Some(size);
        true
    }

    /// Its program's process is gone with its pane's (`Session::pane_lost`).
    pub(crate) fn lost(&mut self) {
        self.pid = None;
    }

    fn resize(&mut self, size: Size) {
        if size != self.size()
            && let Err(error) = self.mirror.resize(size)
        {
            log!("{}: its copy at {size:?}: {error}", self.id);
        }
    }

    /// Its given name, title or launch program, or its id for display.
    pub fn label(&self) -> std::borrow::Cow<'_, str> {
        self.display_label().into()
    }

    pub(crate) fn display_label(&self) -> fux_wire::words::Label<'_> {
        use fux_wire::words::Label;
        if let Some(name) = self.name() {
            return Label::Text(name);
        }
        if !self.title.as_str().is_empty() {
            return Label::Text(self.title.as_str());
        }
        if let Runs::Explicit(launch) = &self.runs {
            return Label::Text(crate::pane::basename(launch.argv.program()));
        }
        Label::Id(fux_wire::command::AnyRef::Pane(self.id))
    }
}
