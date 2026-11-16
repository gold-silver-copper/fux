//! A pane's link as the server holds it, and what the server and a
//! client's connection's own process say to each other (`conn`). What a
//! pane's process and its server say is fux-pane's (`fux_pane::link`).
pub use fux_pane::link::*;
use fux_wire::command::Run;
use fux_wire::id::{ClientId, PaneId};
use std::path::PathBuf;

/// A reply its connection makes itself, to send its client: of a capture
/// pane `pane` holds as `token`, which it fetches and places, or of a client
/// screen it composes from `scene` and the panes' screens.
#[derive(Clone, Debug)]
pub enum Making {
    Capture {
        pane: PaneId,
        token: u64,
        placed: Placed,
    },
    Client {
        client: ClientId,
        scene: Box<crate::render::Scene>,
    },
    /// Its client's body `body` sent `client`'s connection as body
    /// `token`, which types it: answered once `client` answered the read
    /// numbered `asked` sent it after them, so took them
    /// (`ToConn::Typed`), or why it could not (`ToConn::Drop`).
    Keys {
        client: ClientId,
        body: u64,
        token: u64,
        asked: u64,
    },
}
wired!(Making { Capture { pane, token, placed }, Client { client, scene }, Keys { client, body, token, asked } });

/// A pane's link, as the server holds it: to its own process, or, in a
/// session held in one process (the tests', the tools'), to its host
/// itself, which answers at once.
pub enum Link {
    Remote(Remote),
    Local(Box<crate::host::Host>),
}

/// A pane's own process, linked: its pid, and how it ended once it has,
/// as its link closing and its reaping tell (`Session::pane_gone`).
pub struct Remote {
    pub end: End,
    pub pid: fuxix::process::Pid,
    /// It is this server's child, which reaps it; one taken back from a
    /// server that crashed is not (`Link::take`).
    pub child: bool,
    /// Its link closed.
    pub closed: bool,
    /// It was reaped, ending so.
    pub ended: Option<fuxix::process::Status>,
    /// What it said as it began or was taken back, its first facts last,
    /// till its pane takes them (`Session::linked`).
    pub first: Vec<FromPane>,
    /// Its link took none of what was last written to it: its process reads
    /// nothing (stopped, say). What would go to it waits upstream then.
    pub stalled: bool,
    /// What waits while its link takes nothing more, in the order it was
    /// said, and goes in that order once its link took what was queued:
    /// questions asked of it, each by its number, what says how it is
    /// to be now (its size, host, whether it is shown; no number), and
    /// whatever else is said to it while any of those wait, so that
    /// nothing said later goes first (what a client typed, say). A
    /// question waits here for its asker, not in the queue to it, and one
    /// whose asker went first is let go (`Link::unask`). Of what says how
    /// it is to be, only the latest of each kind since the last question
    /// is kept: a question is answered as it was when it was asked, after
    /// what was said before it and before what was said after. What a
    /// pane that reads nothing is owed grows with those still asking, and
    /// with what else is said to it meanwhile (a key a client types).
    pub parked: Vec<(Option<u64>, ToPane)>,
    /// The program whose handle the server copied from it (Windows,
    /// D-134), till another takes its place or the pane ends.
    pub adopted: Option<fuxix::process::Pid>,
}

impl Remote {
    /// Writes what is queued as far as its link takes it, and once it took
    /// it all, what waited till then (`parked`), in the order it was said.
    pub fn flush(&mut self) {
        let queued = self.end.out.len();
        let all = matches!(self.end.flush(), Ok(true));
        self.stalled = queued > 0 && self.end.out.len() == queued;
        if !all {
            return;
        }
        for (_, message) in std::mem::take(&mut self.parked) {
            self.end.send(&message);
        }
        let _ = self.end.flush();
    }

    /// `state`, which says how it is to be now, waits its turn: in the
    /// place of one of its kind said since the last question, if one was,
    /// which it makes stale; else after all that waits.
    fn park_state(&mut self, state: ToPane) {
        let kind = std::mem::discriminant(&state);
        let since = (self.parked.iter().rev())
            .take_while(|(_, waiting)| says_state(waiting))
            .count();
        let from = self.parked.len().saturating_sub(since);
        let stale = (self.parked.iter_mut().skip(from))
            .find(|(_, waiting)| std::mem::discriminant(waiting) == kind);
        match stale {
            Some(stale) => stale.1 = state,
            None => self.parked.push((None, state)),
        }
    }

    /// Keeps, of what says how it is to be, the latest of each kind
    /// between two questions: a question let go leaves what was said on
    /// either side of it side by side.
    fn tidy(&mut self) {
        let mut kept = Vec::with_capacity(self.parked.len());
        // From the last: one of a kind seen since the last question is
        // stale.
        let mut seen = Vec::new();
        while let Some((ask, message)) = self.parked.pop() {
            let kind = std::mem::discriminant(&message);
            match says_state(&message) {
                false => seen.clear(),
                true if seen.contains(&kind) => continue,
                true => seen.push(kind),
            }
            kept.push((ask, message));
        }
        kept.reverse();
        self.parked = kept;
    }

    /// Whether its link has not taken all it was sent: what is asked of it
    /// now waits (`parked`).
    fn backed_up(&self) -> bool {
        self.stalled || !self.end.out.is_empty() || !self.parked.is_empty()
    }
}

/// Whether `message` says how a pane is to be now (its size, host,
/// whether it is shown): of those only the latest matters.
fn says_state(message: &ToPane) -> bool {
    matches!(
        message,
        ToPane::Resize { .. } | ToPane::Host(_) | ToPane::Shown(_)
    )
}

impl Link {
    /// Says `message` to it; a host held here answers into `inbox` at once.
    pub fn send(
        &mut self,
        id: PaneId,
        message: ToPane,
        now: std::time::Instant,
        inbox: &mut std::collections::VecDeque<(PaneId, FromPane)>,
    ) {
        match self {
            Link::Remote(remote) if remote.closed => {}
            Link::Remote(remote) => {
                // What waits keeps the order it was said in: a size said
                // after a question goes after it, and so does all else.
                let waits = !remote.parked.is_empty();
                if says_state(&message) && (!remote.end.out.is_empty() || waits) {
                    remote.park_state(message);
                } else if let (ToPane::Ask { ask, .. }, true) = (&message, remote.backed_up()) {
                    remote.parked.push((Some(*ask), message));
                } else if waits {
                    remote.parked.push((None, message));
                } else {
                    remote.end.send(&message);
                }
            }
            Link::Local(host) => {
                host.handle(message, now);
                Link::told(id, host, inbox);
            }
        }
    }

    /// Question `ask`'s asker is gone: if it still waits here it is let
    /// go, never sent; the body its keys waited for, if it was one's.
    /// False if its pane was sent it.
    pub fn unask(&mut self, ask: u64) -> Option<Option<u64>> {
        let Link::Remote(remote) = self else {
            return None;
        };
        let mut body = None;
        remote.parked.retain(|(parked, question)| {
            if *parked != Some(ask) {
                return true;
            }
            body = Some(match question {
                ToPane::Ask {
                    what: Ask::Keys { body, .. },
                    ..
                } => *body,
                ToPane::Begin(_)
                | ToPane::Resize { .. }
                | ToPane::Host(_)
                | ToPane::Shown(_)
                | ToPane::Typed { .. }
                | ToPane::Ask { .. }
                | ToPane::Taken
                | ToPane::Close { .. }
                | ToPane::Take { .. }
                | ToPane::Who
                | ToPane::Join { .. }
                | ToPane::Drop { .. } => None,
            });
            false
        });
        if body.is_some() {
            remote.tidy();
        }
        body
    }

    /// What host `host`, held here, has to tell, its screen too, into
    /// `inbox`.
    pub fn told(
        id: PaneId,
        host: &mut crate::host::Host,
        inbox: &mut std::collections::VecDeque<(PaneId, FromPane)>,
    ) {
        inbox.extend(host.take().into_iter().map(|report| (id, report)));
        if let Some(update) = host.update() {
            inbox.push_back((id, FromPane::Update(Box::new(update))));
        }
    }

    /// Starts pane `begin.id`'s own process from `exe`, `fux __pane`,
    /// listening at `socket` on its stdin, where its server reaches it and
    /// the next one would if this one crashed (D-124), in a session of its
    /// own; and waits for its first word: why it did not start, if it did
    /// not.
    pub fn start(
        exe: &std::path::Path,
        socket: &std::path::Path,
        begin: Begin,
    ) -> Result<Link, String> {
        let (pid, stream) = crate::socket::start_pane(exe, socket)?;
        Link::begin(stream, pid, true, begin)
    }

    /// Begins the pane whose process `pid` waits at the other end of
    /// `stream` for its first word, and waits for its answer: this
    /// server's child if `child`, else one a crashed server started and
    /// never began (`Taking::Unbegun`), begun from its run's record.
    pub fn begin(
        stream: crate::socket::Stream,
        pid: fuxix::process::Pid,
        child: bool,
        begin: Begin,
    ) -> Result<Link, String> {
        let mut end = End::new(stream).map_err(|e| format!("its link: {e}"))?;
        end.send(&ToPane::Begin(Box::new(begin)));
        if let Err(error) = end.flush_all() {
            return Err(format!("its process left before it began: {error}"));
        }
        let began = end.wait::<FromPane>();
        let mut remote = Remote::new(end, pid, child);
        match began.map_err(|e| format!("its process: {e}"))? {
            Some(FromPane::Begun(None)) => {
                // Its first facts come with it, so that nothing reads its
                // copy before it has them.
                remote.first = first(&mut remote.end)?;
                Ok(Link::Remote(remote))
            }
            Some(FromPane::Begun(Some(why))) => Err(why),
            Some(_) => Err("its process's first word was not its start".to_owned()),
            None => Err("its process ended before it began".to_owned()),
        }
    }
}

impl Remote {
    /// The link `end` to the pane process `pid`, just begun or taken.
    pub fn new(end: End, pid: fuxix::process::Pid, child: bool) -> Remote {
        Remote {
            end,
            pid,
            child,
            closed: false,
            ended: None,
            first: Vec::new(),
            stalled: false,
            parked: Vec::new(),
            adopted: None,
        }
    }
}

/// What a pane says as it begins, up to its first facts, which it sends
/// once a frame it is part way through is whole: what it told before them
/// too, in order.
fn first(end: &mut End) -> Result<Vec<FromPane>, String> {
    let mut said = Vec::new();
    loop {
        match end.wait::<FromPane>() {
            Ok(Some(update @ FromPane::Update(_))) => {
                said.push(update);
                return Ok(said);
            }
            Ok(Some(other)) => said.push(other),
            Ok(None) => return Err("its process ended before its first facts".to_owned()),
            Err(error) => return Err(format!("its process: {error}")),
        }
    }
}

/// From the server to a client's connection's own process (D-123, `conn`).
#[derive(Debug)]
pub enum ToConn {
    /// Bytes for its client: frames, as the client protocol has them.
    Bytes(Vec<u8>),
    /// What its client is to send next: the frames it decodes.
    Expect(Expect),
    /// The answer to its client's request is all sent: once the client
    /// has it, its next request is read.
    Answered,
    /// Whether its client's request waits on someone else, with no answer
    /// on its way: for a pane to change, for events, for a client's own
    /// answer, or behind an attached client's frames. Not one its pane
    /// answers (keys, a capture): that answer is on its way.
    Waits(bool),
    /// Its first word: where its run's panes listen, and the run, which
    /// it tells each pane it reaches (`ToPane::Join`).
    Hello { panes: PathBuf, run: Run },
    /// What its attached client's screen is made of now but its panes'
    /// screens, which it gets from the panes (`render::Scene`).
    Scene(Box<crate::render::Scene>),
    /// Its client's body `body` (`FromConn::Frame`) goes to `panes`, as
    /// their server's `token`, a piece at a time; to none, it is let go,
    /// the pieces its client still sends too. Every body is answered so.
    Deliver {
        body: u64,
        token: u64,
        panes: Vec<PaneId>,
    },
    /// Its client is attached as `client`: it listens for bodies other
    /// connections send it, at `SOCKET.panes/cN` (`ToSide::Body`).
    Listen { client: ClientId },
    /// Its client is sent body `token`'s keys, as typed at its terminal
    /// (`ToAttached::Keys`), once the body comes: what follows waits.
    Keys { token: u64 },
    /// Body `token` goes no further: its sender is gone, or the client it
    /// was for. Taken as it comes, before what waits for it.
    Drop { token: u64 },
    /// Body `token`, its client's keys, was taken by the client it was
    /// for, still attached (`Making::Keys`).
    Typed { token: u64 },
    /// The answer to its client's request, which it makes itself, as its
    /// client is to have it: its last word once made.
    Make(Box<Making>),
    /// Its client is detached: its terminal is given back its own cursor
    /// shape and configured colour, where they were set.
    Detached,
    /// Its last word is sent: it ends once its client has it all.
    Close,
    /// Frames for its client that are state, not a backlog: a client that
    /// is not reading is owed the latest of each kind, never each one
    /// (`conn::Pending`). In pieces of one frame at most, whole at `last`.
    Owed {
        kind: Owed,
        bytes: Vec<u8>,
        last: bool,
    },
}
wired!(ToConn {
    Bytes(b),
    Expect(e),
    Answered,
    Waits(w),
    Hello { panes, run },
    Scene(s),
    Deliver { body, token, panes },
    Listen { client },
    Keys { token },
    Drop { token },
    Typed { token },
    Make(m),
    Detached,
    Close,
    Owed { kind, bytes, last }
});

/// What a client is owed as state (`ToConn::Owed`): a clipboard copy and a
/// title, the latest of which is the one that counts; the bell, rung once
/// however often it rang; a prompt or chooser to open, one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owed {
    Clipboard,
    Title,
    Bell,
    Prompt,
}
wired!(Owed {
    Clipboard,
    Title,
    Bell,
    Prompt
});

/// The frames a connection's client sends next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Attach,
    Asked,
    Attached,
}
wired!(Expect {
    Attach,
    Asked,
    Attached
});

/// From a client's connection's own process to the server.
#[derive(Debug)]
pub enum FromConn {
    /// A whole frame its client sent, decoded as the frame expected: sent
    /// again as it came, for the server to take, but for its inputs or
    /// paste text, which it keeps as `body` (`ToConn::Deliver`).
    Frame { frame: Vec<u8>, body: Option<u64> },
    /// Everything sent for its client was written to it.
    Flushed,
    /// Its client shut its side: it sends no more.
    Shut,
    /// Its client sent what is no frame of those expected: why. It reads
    /// it no more.
    Bad(String),
    /// This many keys its client typed were not taken: they came faster
    /// than its server took them, a frame of them waiting here already.
    /// Its server tells its client so.
    Dropped(u64),
    /// It could not reach pane `pane` with body `token`, or its link to
    /// it went before the pane took all of it: its server, which told
    /// that pane to wait for the body, tells it to wait no more.
    Unreached { token: u64, pane: PaneId },
}
wired!(FromConn {
    Frame { frame, body },
    Flushed,
    Shut,
    Bad(why),
    Dropped(n),
    Unreached { token, pane },
});

/// How the server starts a client's connection's own process, for a
/// client it took: `fux __conn`, its link on stdin, its client on stdout
/// (`conn::serve`).
pub const CONN: &str = "__conn";

#[cfg(test)]
mod tests {
    use super::{BYTES, Message, Sending, ToConn};
    use fux_wire::protocol::{Decoder, Frame};

    /// A thousand questions to a pane whose process reads nothing, each
    /// given up by its asker, leave nothing more queued to it: once its
    /// link takes no more, a question waits with its asker and goes with
    /// it (D-185; they queued in the server, one for each who had asked).
    /// One whose asker stays is sent once the pane reads again.
    #[cfg(unix)]
    #[test]
    fn questions_to_a_pane_that_reads_nothing_leave_with_their_askers()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::{Ask, End, Link, Read, Remote, ToPane};
        use fux_wire::command::Input;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        let mut theirs = End::new(theirs)?;
        let mut link = Link::Remote(Remote {
            end: End::new(ours)?,
            pid: fuxix::process::Pid::from_i32(1).ok_or("a pid")?,
            child: false,
            closed: false,
            ended: None,
            first: Vec::new(),
            stalled: false,
            parked: Vec::new(),
            adopted: None,
        });
        let id = fux_wire::id::PaneId::of(1);
        let (now, mut inbox) = (std::time::Instant::now(), std::collections::VecDeque::new());
        let question = |ask: u64, bytes: usize| ToPane::Ask {
            ask,
            actor: fux_wire::events::Actor::Outside,
            what: Ask::Keys {
                inputs: vec![Input::Text(std::iter::repeat_n('k', bytes).collect())],
                seen: None,
                body: None,
            },
        };
        // What the server does each turn: what it holds for (queued, parked).
        fn held(link: &mut Link) -> (usize, usize) {
            match link {
                Link::Remote(remote) => {
                    remote.flush();
                    (remote.end.out.len(), remote.parked.len())
                }
                Link::Local(_) => (0, 0),
            }
        }
        // Asked till its socket takes no more: what is queued then is the
        // most there ever is.
        let mut asked = 0u64;
        while held(&mut link).1 == 0 {
            asked += 1;
            assert!(asked < 10_000, "its socket took everything");
            link.send(id, question(asked, 16 << 10), now, &mut inbox);
        }
        let (queued, _) = held(&mut link);
        let first_parked = asked;
        assert_eq!(link.unask(first_parked), Some(None));
        for _ in 0..1_000 {
            asked += 1;
            link.send(id, question(asked, 16 << 10), now, &mut inbox);
            assert_eq!(held(&mut link), (queued, 1), "question {asked}");
            assert_eq!(link.unask(asked), Some(None));
            assert_eq!(held(&mut link), (queued, 0));
        }
        // One sent before its link stopped is the pane's to answer.
        assert_eq!(link.unask(1), None);
        // An asker that stays: its question goes once the pane reads.
        asked += 1;
        link.send(id, question(asked, 5), now, &mut inbox);
        let mut buffer = vec![0u8; 64 << 10];
        let mut last = 0;
        while held(&mut link) != (0, 0) || last != asked {
            assert_ne!(theirs.read(&mut buffer), Read::Closed);
            while let Some(ToPane::Ask { ask, .. }) = theirs.next_message::<ToPane>()? {
                assert!(
                    ask < first_parked || ask == asked,
                    "a question let go was sent: {ask}"
                );
                last = ask;
            }
        }
        Ok(())
    }

    /// What is said to a pane whose link takes nothing more goes, once it
    /// reads, in the order it was said: a size said after a question comes
    /// after it, as the question was asked at the size before (a size said
    /// later went before every question that waited). Of two sizes with no
    /// question between them only the later is sent, and a question let go
    /// leaves no size behind it that a later one made stale.
    #[cfg(unix)]
    #[test]
    fn what_waits_for_a_pane_goes_in_the_order_it_was_said()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::{Ask, End, Link, Read, Remote, ToPane};
        use fux_wire::events::Actor;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        let mut theirs = End::new(theirs)?;
        let mut link = Link::Remote(Remote {
            end: End::new(ours)?,
            pid: fuxix::process::Pid::from_i32(1).ok_or("a pid")?,
            child: false,
            closed: false,
            ended: None,
            first: Vec::new(),
            stalled: false,
            parked: Vec::new(),
            adopted: None,
        });
        let id = fux_wire::id::PaneId::of(1);
        let (now, mut inbox) = (std::time::Instant::now(), std::collections::VecDeque::new());
        let size = |cols: u16| -> Result<ToPane, Box<dyn std::error::Error>> {
            let (rows, cols) = (std::num::NonZeroU16::MIN, std::num::NonZeroU16::new(cols));
            Ok(ToPane::Resize {
                size: fux_vt::Size::from((rows, cols.ok_or("columns")?)),
                actor: Actor::Outside,
            })
        };
        let question = |ask: u64| ToPane::Ask {
            ask,
            actor: Actor::Outside,
            what: Ask::Signal(fuxix::process::Signal::Term),
        };
        let waiting = |link: &Link| match link {
            Link::Remote(remote) => remote.parked.len(),
            Link::Local(_) => 0,
        };
        // Something its link has not taken: what is said now waits.
        link.send(id, ToPane::Shown(true), now, &mut inbox);
        link.send(id, size(10)?, now, &mut inbox);
        link.send(id, size(11)?, now, &mut inbox);
        assert_eq!(waiting(&link), 1, "two sizes, the later kept");
        link.send(id, question(1), now, &mut inbox);
        link.send(id, size(12)?, now, &mut inbox);
        link.send(id, question(2), now, &mut inbox);
        link.send(id, size(13)?, now, &mut inbox);
        link.send(id, question(3), now, &mut inbox);
        link.send(id, size(14)?, now, &mut inbox);
        assert_eq!(waiting(&link), 7);
        // Its asker gone, the sizes on either side of it are one.
        assert_eq!(link.unask(2), Some(None));
        assert_eq!(waiting(&link), 5);
        if let Link::Remote(remote) = &mut link {
            remote.flush();
        }
        assert_eq!(waiting(&link), 0);
        let mut buffer = [0u8; 4096];
        assert_ne!(theirs.read(&mut buffer), Read::Closed);
        let mut said = Vec::new();
        while let Some(message) = theirs.next_message::<ToPane>()? {
            said.push(match message {
                ToPane::Resize { size, .. } => format!("size {}", size.cols()),
                ToPane::Ask { ask, .. } => format!("question {ask}"),
                ToPane::Shown(_) => "shown".to_owned(),
                other @ (ToPane::Begin(_)
                | ToPane::Host(_)
                | ToPane::Typed { .. }
                | ToPane::Taken
                | ToPane::Close { .. }
                | ToPane::Take { .. }
                | ToPane::Who
                | ToPane::Join { .. }
                | ToPane::Drop { .. }) => format!("{other:?}"),
            });
        }
        let order = [
            "shown",
            "size 11",
            "question 1",
            "size 13",
            "question 3",
            "size 14",
        ];
        assert_eq!(said, order);
        Ok(())
    }

    /// What a client types at a pane whose link is backed up goes after a
    /// question asked of it before: nothing said to a pane overtakes what
    /// waits for it.
    #[cfg(unix)]
    #[test]
    fn what_is_typed_goes_after_a_question_that_waits() -> Result<(), Box<dyn std::error::Error>> {
        use super::{Ask, End, Link, Read, Remote, ToPane, Typed};
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        let mut theirs = End::new(theirs)?;
        let pid = fuxix::process::Pid::from_i32(1).ok_or("a pid")?;
        let mut link = Link::Remote(Remote::new(End::new(ours)?, pid, false));
        let id = fux_wire::id::PaneId::of(1);
        let (now, mut inbox) = (std::time::Instant::now(), std::collections::VecDeque::new());
        // Something its link has not taken: a question asked now waits.
        link.send(id, ToPane::Shown(true), now, &mut inbox);
        let question = ToPane::Ask {
            ask: 1,
            actor: fux_wire::events::Actor::Outside,
            what: Ask::Respawn {
                kill: false,
                argv: None,
            },
        };
        link.send(id, question, now, &mut inbox);
        let typed = ToPane::Typed {
            client: fux_wire::id::ClientId::of(1),
            typed: Typed::Focus(true),
        };
        link.send(id, typed, now, &mut inbox);
        if let Link::Remote(remote) = &mut link {
            remote.flush();
        }
        let mut buffer = [0u8; 4096];
        assert_ne!(theirs.read(&mut buffer), Read::Closed);
        let mut said = Vec::new();
        while let Some(message) = theirs.next_message::<ToPane>()? {
            said.push(std::mem::discriminant(&message));
        }
        let order = [
            std::mem::discriminant(&ToPane::Shown(true)),
            std::mem::discriminant(&ToPane::Ask {
                ask: 0,
                actor: fux_wire::events::Actor::Outside,
                what: Ask::Respawn {
                    kill: false,
                    argv: None,
                },
            }),
            std::mem::discriminant(&ToPane::Typed {
                client: fux_wire::id::ClientId::of(1),
                typed: Typed::Focus(true),
            }),
        ];
        assert!(said == order, "typed went before the question that waited");
        Ok(())
    }

    /// A read up to a limit reads no more than it: the rest stays in the
    /// socket for the next read.
    #[cfg(unix)]
    #[test]
    fn a_read_up_to_a_limit_reads_no_more() -> Result<(), Box<dyn std::error::Error>> {
        use super::{End, Read};
        use std::io::{Read as _, Write as _};
        let (ours, mut theirs) = std::os::unix::net::UnixStream::pair()?;
        let mut end = End::new(ours)?;
        theirs.write_all(&[0u8; 100])?;
        drop(theirs);
        assert_eq!(end.read_up_to(&mut [0u8; 64], 10), Read::More);
        let mut rest = Vec::new();
        (&end.stream).read_to_end(&mut rest)?;
        assert_eq!(rest.len(), 90);
        Ok(())
    }

    /// As many of a client's bytes as the server sends its connection's
    /// process at once are one frame, which that process, joining nothing
    /// its server sends, takes.
    #[test]
    fn a_full_message_of_a_clients_bytes_is_one_frame() {
        let mut sent = Vec::new();
        Sending(&ToConn::Bytes(vec![7; BYTES])).encode_into(&mut sent);
        let mut decoder = Decoder::default();
        decoder.push(&sent);
        let taken = decoder.frame::<Message<ToConn>>();
        assert!(
            matches!(&taken, Ok(Some(Message(ToConn::Bytes(bytes)))) if bytes.len() == BYTES),
            "{:?}",
            taken.map(|message| message.map(|m| m.0))
        );
    }
}
