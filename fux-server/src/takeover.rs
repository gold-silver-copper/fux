//! Taking a run back (D-124, D-166): the run's record is the truth, and
//! the server that takes it makes the pane processes still there match it,
//! one pane at a time. Each is reached at its socket without waiting
//! (`socket::reach`) and asked (`Asked`); what it answers, or that it has
//! not yet, is how it is found (`Live`); and `reconcile` says from the
//! record and that alone what becomes of it (`Fate`). One pane that says
//! nothing holds up no other, nor the start: every pane is placed as its
//! record keeps it, and linked when its process speaks, in the server's
//! loop; a request for one not yet linked waits for it (`Session::begin`).
use crate::link::{End, FromPane, Greeting, Link, PANE_PROTOCOL, Read, Remote, Taking, ToPane};
use crate::socket::Reach;
use fux_wire::id::PaneId;
use std::collections::BTreeMap;
use std::path::Path;

/// How a pane's process was found by the server that reached for it. One
/// no process listens for has none: it is gone.
#[derive(Clone, Debug)]
pub enum Live {
    /// It said what it is, and its first facts: it is this server's now.
    Greeted(Box<Greeting>),
    /// It never began; its process, by its pid.
    Unbegun(i32),
    /// It is closed and ending.
    Closing,
    /// It is no pane of this run, or not the pane its socket names.
    OtherRun,
    /// The server with this pid holds its link still.
    Held(i32),
    /// It listens, and has not answered: stopped, say, or with no room for
    /// one more at its socket.
    Silent,
}

/// What becomes of a pane as its run is taken back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    /// It is linked, as it greeted.
    Take,
    /// It is begun, from its record.
    Begin,
    /// Its process is gone: it stays as its record keeps it, its program
    /// lost.
    Lost,
    /// It is placed from its record, and linked when it speaks.
    Await,
    /// It is ended: its run has it no more, or it was closing already.
    End,
    /// The run is not taken: the server with this pid holds the pane.
    Refuse(i32),
}

/// What becomes of one pane: one its run's record has, if `recorded`,
/// found as `live` says, or gone if that is none. None for one the run
/// has not and no process listens for.
pub fn fate(recorded: bool, live: Option<&Live>) -> Option<Fate> {
    let Some(live) = live else {
        return recorded.then_some(Fate::Lost);
    };
    Some(match (recorded, live) {
        (_, Live::Held(server)) => Fate::Refuse(*server),
        (true, Live::Greeted(_)) => Fate::Take,
        (true, Live::Unbegun(_)) => Fate::Begin,
        (true, Live::Silent) => Fate::Await,
        // No pane of this run listens there: this run's is gone.
        (true, Live::OtherRun) => Fate::Lost,
        // A close the run's last server made, whose record it could not
        // write, or one the user made of a pane not yet heard: finished.
        (true, Live::Closing) => Fate::End,
        // A close the user committed, recorded before its pane was told:
        // finished. One that says nothing is ended when it speaks.
        // A pane of another run in this run's directory is what an
        // earlier run on this socket left, never told to end: ended too.
        (
            false,
            Live::Greeted(_) | Live::Unbegun(_) | Live::Closing | Live::Silent | Live::OtherRun,
        ) => Fate::End,
    })
}

/// What becomes of each pane of the run `record` holds and of each pane
/// process found that it does not hold, by how each was found (`live`,
/// which has none that is gone). It reads nothing else, and does nothing.
pub fn reconcile(
    record: &crate::record::Record,
    live: &BTreeMap<PaneId, Live>,
) -> BTreeMap<PaneId, Fate> {
    let recorded: Vec<PaneId> = (crate::record::panes(&record.workspaces).iter())
        .map(|pane| pane.id)
        .collect();
    let of = |id: &PaneId, recorded: bool| Some((*id, fate(recorded, live.get(id))?));
    (recorded.iter().filter_map(|id| of(id, true)))
        .chain((live.keys().filter(|id| !recorded.contains(id))).filter_map(|id| of(id, false)))
        .collect()
}

/// A pane's process reached at its socket and asked: its link, and what it
/// said so far.
pub struct Asked {
    pub end: End,
    /// The process that listens, where the system names it (Windows).
    maker: Option<fuxix::process::Pid>,
    /// What it is linked as, once it said: as it greeted, or as the
    /// process, by its pid, that was begun on this link (`begin`).
    linking: Linking,
    /// Whether its first facts follow its greeting: asked to be taken, it
    /// is linked, and sends them; asked who it is, it is not.
    facts: bool,
    /// What it said after its greeting or its start, up to its first facts.
    said: Vec<FromPane>,
}

/// How far a pane asked is towards its link.
enum Linking {
    Asked,
    Greeted(Box<Greeting>),
    /// Told to begin (`Asked::begin`), and whether it said it has.
    Beginning(fuxix::process::Pid, bool),
}

/// What a pane asked said, once it has.
pub enum Answer {
    Live(Live),
    /// Begun on this link (`Asked::begin`), its first facts come: linked.
    Began,
    /// Told to begin, it could not start its program: why.
    NotBegun(String),
    /// Its link closed with no word: its process ended meanwhile, or took
    /// none of it. Never "lost" by this alone: it is reached for again.
    Closed,
}

impl Asked {
    /// Reaches for the pane process at `socket`, and says `word` to it,
    /// without waiting: none if no process listens there, or one does with
    /// no room (`Reach::Busy`), which the second says.
    pub fn ask(socket: &Path, word: &ToPane) -> std::io::Result<(Option<Asked>, bool)> {
        let stream = match crate::socket::reach(socket)? {
            Reach::Listening(stream) => stream,
            Reach::NoListener => return Ok((None, false)),
            Reach::Busy => return Ok((None, true)),
        };
        let maker = crate::socket::listener_pid(&stream);
        let mut end = End::new(stream)?;
        end.send(word);
        // What its socket does not take now goes as it does (`hear`).
        let _ = end.flush();
        let asked = Asked {
            end,
            maker,
            linking: Linking::Asked,
            facts: matches!(word, ToPane::Take { .. }),
            said: Vec::new(),
        };
        Ok((Some(asked), false))
    }

    /// Tells it, which said it never began (`Live::Unbegun`, its process
    /// `pid`), to begin as `begin` says, on this link, which it still
    /// reads: its answer comes as its greeting would have (`hear`), and
    /// nothing waits for it.
    pub fn begin(&mut self, begin: crate::link::Begin, pid: fuxix::process::Pid) {
        self.end.send(&ToPane::Begin(Box::new(begin)));
        let _ = self.end.flush();
        self.linking = Linking::Beginning(pid, false);
    }

    /// Reads what pane `id` said: its answer, once it is whole. One that
    /// greets, or begins, has answered once its first facts came, so that
    /// nothing reads its copy before it has them.
    pub fn hear(&mut self, id: PaneId, buffer: &mut [u8]) -> Option<Answer> {
        let _ = self.end.flush();
        let closed = self.end.read(buffer) == Read::Closed;
        let stranger = |why: &str| {
            log!("{id}: what listens at its socket {why}");
            Some(Answer::Live(Live::OtherRun))
        };
        loop {
            let message = match self.end.next_message::<FromPane>() {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(error) => return stranger(&format!("sent a bad frame: {error}")),
            };
            match (message, &mut self.linking) {
                (FromPane::Taking(Taking::Greets(greeting)), Linking::Asked) => {
                    if let Err(why) = self.check(id, &greeting) {
                        return stranger(&format!("is not it: {why}"));
                    }
                    if !self.facts {
                        return Some(Answer::Live(Live::Greeted(greeting)));
                    }
                    self.linking = Linking::Greeted(greeting);
                }
                (FromPane::Taking(taking), Linking::Asked) => {
                    return Some(Answer::Live(match taking {
                        Taking::Unbegun { pid } => Live::Unbegun(pid),
                        Taking::Closing => Live::Closing,
                        Taking::Held { server } => Live::Held(server),
                        Taking::OtherRun | Taking::Greets(_) => Live::OtherRun,
                    }));
                }
                (FromPane::Begun(None), Linking::Beginning(_, begun)) => *begun = true,
                (FromPane::Begun(Some(why)), Linking::Beginning(..)) => {
                    return Some(Answer::NotBegun(why));
                }
                (update @ FromPane::Update(_), Linking::Greeted(greeting)) => {
                    let greeting = greeting.clone();
                    self.said.push(update);
                    return Some(Answer::Live(Live::Greeted(greeting)));
                }
                (update @ FromPane::Update(_), Linking::Beginning(_, true)) => {
                    self.said.push(update);
                    return Some(Answer::Began);
                }
                (other, Linking::Greeted(_) | Linking::Beginning(_, true)) => self.said.push(other),
                (_, Linking::Asked | Linking::Beginning(_, false)) => {
                    return stranger("answered with no answer");
                }
            }
        }
        closed.then_some(Answer::Closed)
    }

    /// Whether `greeting` is pane `id`'s, speaking this pane protocol, and
    /// names the process the system does, where it names one: the pid a
    /// takeover adopts a program from.
    fn check(&self, id: PaneId, greeting: &Greeting) -> Result<(), String> {
        if greeting.protocol != PANE_PROTOCOL {
            return Err(format!(
                "it speaks pane protocol {}, not {PANE_PROTOCOL}",
                greeting.protocol
            ));
        }
        if greeting.id != id {
            return Err(format!("it says it is {}", greeting.id));
        }
        let pid = fuxix::process::Pid::from_i32(greeting.pid)
            .ok_or_else(|| format!("its greeting names pid {}", greeting.pid))?;
        if self.maker.is_some_and(|maker| maker != pid) {
            return Err(format!(
                "its greeting names pid {pid}, and the system another"
            ));
        }
        Ok(())
    }

    /// Its link, once it greeted or began: not this server's child, so its
    /// link closing is all the server learns of its end.
    pub fn link(self) -> Option<Link> {
        let pid = match self.linking {
            Linking::Greeted(greeting) => fuxix::process::Pid::from_i32(greeting.pid)?,
            Linking::Beginning(pid, true) => pid,
            Linking::Asked | Linking::Beginning(_, false) => return None,
        };
        let mut remote = Remote::new(self.end, pid, false);
        remote.first = self.said;
        Some(Link::Remote(remote))
    }

    /// Says `word` after what it was asked, as far as its socket takes it
    /// now, and lets it go: a close for one that has not answered, which
    /// it reads when it reads what it was asked.
    pub fn leave(mut self, word: &ToPane) {
        self.end.send(word);
        let _ = self.end.flush();
    }
}

/// Reaches for the pane process at each of `sockets` and says `word` to
/// it, without waiting for any: each one asked, or none for one that
/// listens with no room at its socket, to be reached for again; one no
/// process listens for has no entry. A socket that can't be reached for
/// any other reason is the error, with nothing done but the asking.
pub fn ask_all(
    sockets: &BTreeMap<PaneId, std::path::PathBuf>,
    word: &ToPane,
) -> std::io::Result<BTreeMap<PaneId, Option<Asked>>> {
    let mut asked = BTreeMap::new();
    for (id, socket) in sockets {
        match Asked::ask(socket, word)? {
            (Some(one), _) => drop(asked.insert(*id, Some(one))),
            (None, true) => drop(asked.insert(*id, None)),
            (None, false) => {}
        }
    }
    Ok(asked)
}

/// The sockets of the pane processes that may wait at `panes`, a run's
/// directory, by the panes their names say: every entry named as a pane's
/// socket is (`N`), whether a process listens there or not.
pub fn sockets(panes: &Path) -> BTreeMap<PaneId, std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(panes) else {
        return BTreeMap::new();
    };
    (entries.flatten())
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            // A pane's socket is named by its number alone, as it prints.
            let id: PaneId = format!("%{name}").parse().ok()?;
            (id.number().to_string() == name).then(|| (id, entry.path()))
        })
        .collect()
}

/// The pane processes that listen at `panes`, a run's directory, each
/// with its pid: as the system names it, where it does; else as the pane
/// says itself, asked who it is and waited for, as a run that can't be
/// taken back names what the user is to end. One with no room at its
/// socket is named with none.
pub fn waiting(panes: &Path) -> Vec<(PaneId, Option<i32>)> {
    let mut waiting = Vec::new();
    for (id, socket) in sockets(panes) {
        let stream = match crate::socket::reach(&socket) {
            Ok(Reach::Listening(stream)) => stream,
            Ok(Reach::Busy) => {
                waiting.push((id, None));
                continue;
            }
            Ok(Reach::NoListener) | Err(_) => continue,
        };
        let named = crate::socket::peer_pid(&stream).map(fuxix::process::Pid::as_i32);
        let pid = named.or_else(|| {
            let mut end = End::new(stream).ok()?;
            end.send(&ToPane::Who);
            end.flush_all().ok()?;
            match end.wait::<FromPane>() {
                Ok(Some(FromPane::Taking(Taking::Greets(greeting)))) => Some(greeting.pid),
                Ok(Some(FromPane::Taking(Taking::Unbegun { pid }))) => Some(pid),
                Ok(_) | Err(_) => None,
            }
        });
        waiting.push((id, pid));
    }
    waiting
}

/// Whether the server with pid `server`, which a pane named as holding
/// its link, still runs: one that ended since holds nothing, and its pane
/// reads its link's end next.
pub fn holds(server: i32) -> bool {
    fuxix::process::Pid::from_i32(server).is_some_and(crate::socket::runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Child, Pane, Record, Tab, Workspace};
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A record of a run with panes `ids` in one tab.
    fn record(ids: &[u32]) -> Result<Record, Box<dyn std::error::Error>> {
        let size = fux_vt::Size::new(24, 80)?;
        let panes = (ids.iter())
            .map(|id| {
                Ok(Pane {
                    id: PaneId::of(*id),
                    name: None,
                    shell: true,
                    argv: fux_wire::command::Program::new("/bin/sh".into(), Vec::new())?,
                    cwd: "/".into(),
                    options: crate::pane::tests::options(1 << 10)?,
                    size,
                    respawns: 0,
                    ended: None,
                    read_error: None,
                })
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        let tab = Tab {
            id: fux_wire::id::TabId::of(1),
            name: None,
            nodes: Vec::new(),
            revision: Some(0),
            fixed: false,
            size,
            zoomed: None,
            panes,
        };
        Ok(Record {
            run: fux_wire::command::Run::now().ok_or("a clock")?,
            ids: Vec::new(),
            workspaces: vec![Workspace {
                id: fux_wire::id::WsId::of(1),
                name: None,
                children: vec![Child::Tab(tab)],
            }],
            ended: false,
        })
    }

    /// The record decides: a recorded pane is taken, begun, awaited or
    /// lost by how it is found; a pane process the record lacks is ended,
    /// a close the user committed; one that says nothing, or is closing,
    /// is never lost; and a pane a server still holds refuses the run,
    /// recorded or not.
    #[test]
    fn the_record_decides_each_panes_fate() -> TestResult {
        let record = record(&[1, 2, 3, 4, 5, 6])?;
        let greeting = Greeting {
            protocol: PANE_PROTOCOL,
            id: PaneId::of(6),
            pid: 72,
            run: record.run,
            respawns: 0,
            shell: true,
            argv: fux_wire::command::Program::new("/bin/sh".into(), Vec::new())?,
            cwd: "/".into(),
            options: crate::pane::tests::options(1 << 10)?,
            size: fux_vt::Size::new(24, 80)?,
            title: crate::pane::Said::default(),
            counts: fux_wire::command::Counts::default(),
            ended: None,
            hung_up: false,
            read_error: None,
        };
        let live = BTreeMap::from([
            // %1 is gone: it has none.
            (PaneId::of(2), Live::Unbegun(70)),
            (PaneId::of(6), Live::Greeted(Box::new(greeting.clone()))),
            (PaneId::of(11), Live::Greeted(Box::new(greeting))),
            (PaneId::of(3), Live::Silent),
            (PaneId::of(4), Live::Closing),
            (PaneId::of(5), Live::OtherRun),
            // The record lacks these.
            (PaneId::of(7), Live::Unbegun(71)),
            (PaneId::of(8), Live::Silent),
            (PaneId::of(9), Live::Closing),
            (PaneId::of(10), Live::OtherRun),
        ]);
        let fates = reconcile(&record, &live);
        let expected = BTreeMap::from([
            (PaneId::of(1), Fate::Lost),
            (PaneId::of(2), Fate::Begin),
            (PaneId::of(3), Fate::Await),
            (PaneId::of(4), Fate::End),
            (PaneId::of(5), Fate::Lost),
            (PaneId::of(6), Fate::Take),
            (PaneId::of(7), Fate::End),
            (PaneId::of(8), Fate::End),
            (PaneId::of(9), Fate::End),
            (PaneId::of(10), Fate::End),
            (PaneId::of(11), Fate::End),
        ]);
        assert_eq!(fates, expected);
        // Held, recorded or not: the run is refused, naming the server:
        // the pane is neither lost nor ended.
        for held in [3, 8] {
            let mut live = live.clone();
            live.insert(PaneId::of(held), Live::Held(4242));
            let fates = reconcile(&record, &live);
            assert_eq!(fates.get(&PaneId::of(held)), Some(&Fate::Refuse(4242)));
        }
        Ok(())
    }
}
