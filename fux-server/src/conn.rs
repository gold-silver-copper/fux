//! A client's connection's own process (D-123): what a client can make
//! grow is held here, never in the server. It decodes what the client
//! sends, each frame as the one its stage expects, and sends the server
//! the frames whole; it holds what is to go to the client until the client
//! reads it; and for an attached client it composes its screen: from what
//! its server says the screen is made of (`render::Scene`) and the screens
//! of the panes it shows, which it gets from the panes themselves, at
//! their sockets (`ToSide`), and paints its client as that differs from
//! what the client shows. A client that floods it, or stops reading, ends
//! only its connection. It ends with its server: a server that crashes
//! leaves it nothing to serve, and what it still holds for its client is
//! let go.
use crate::link::{Drawn, End, Expect, FromConn, FromSide, Making, Read, ToConn, ToPane, ToSide};
use crate::render::{Grid, Scene, Sixels, Visible};
use crate::socket::Stream;
use fux_vt::bytes::ByteQueue;
use fux_wire::answer::PaneResults;
use fux_wire::command::{Input, Run};
use fux_wire::id::{ClientId, PaneId};
use fux_wire::protocol::{
    Asked, Attach, AttachedFrame, Decoder, Frame, Hello, Response, ToAttached,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The most it reads from its client before it looks at its server, and
/// the most it holds of what its client sent ahead of its stage.
const READ: usize = 256 * 1024;
/// What a client that sent that much ahead of a request that waits is told
/// as it is refused.
const AHEAD: &str = "more than 256 KiB sent while a request waited: \
                     a request is sent once the one before it is answered";
/// What a client is told whose server went without a last word for it:
/// said by its connection's process, which outlives its server.
const SERVER_GONE: &str = "the server process ended without a word (it was killed, or crashed, \
                           or it stopped as this client came); `fux '(start)'`, or `fux` in a \
                           terminal, takes back the run of one that did not stop";
/// The least time between two paints for a client, as a flood is painted.
/// A keystroke's echo (`Clock::echo`) and panes gone quiet (`QUIET`) are
/// painted sooner.
const PAINT: Duration = Duration::from_millis(16);
/// Panes quiet for this after their output: painted then, not held to
/// `PAINT`. A program's output in a few bursts, as a screen redrawn is,
/// shows when it ends; a flood, never quiet so long, keeps to `PAINT`.
const QUIET: Duration = Duration::from_millis(1);

/// What a pane's screen paints a copy of it with: no program reads what it
/// says back.
const MIRROR: fux_vt::Options = fux_vt::Options::new()
    .with(fux_vt::Feature::Hyperlinks)
    .with(fux_vt::Feature::Palette)
    .with(fux_vt::Feature::Pictures);

/// What it decodes next of its client's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Hello,
    Frames(Expect),
    /// A request is under way: its client's next waits for its answer,
    /// written to the client whole (`answered`).
    Answer,
    /// Nothing more till the server says.
    Nothing,
}

/// When its client may be painted next: `PAINT` after its last paint, as a
/// pane that keeps the screen changing is painted; at once for a
/// keystroke's echo; and once the panes are quiet, if that is sooner.
struct Clock {
    next: Instant,
    due: Instant,
    /// The pane the client last typed into, until it writes: what it writes
    /// then, a keystroke's echo, is painted at once, not held to `PAINT`
    /// behind a pane that keeps the screen changing (tmux paints it at
    /// once too). One paint a keystroke at most.
    echo: Option<PaneId>,
}

impl Clock {
    fn painted(&mut self, now: Instant) {
        self.next = crate::after(now, PAINT);
        self.due = self.next;
    }
    /// Pane `pane`'s screen came at `now`.
    fn output(&mut self, pane: PaneId, now: Instant) {
        if self.echo == Some(pane) {
            self.echo = None;
            (self.next, self.due) = (now, now);
        } else if now < self.next {
            self.due = crate::after(now, QUIET).min(self.next);
        }
    }
}

/// A pane it shows, reached at the pane's socket: its link, while the pane
/// has a process, and its screen, as the pane last sent it.
struct Side {
    end: Option<End>,
    mirror: Option<fux_vt::Parser>,
}

impl Side {
    /// Pane `id` of run `run`, at its socket in `panes`: asked to send its
    /// screen. One that can't be reached shows blank.
    fn reach(panes: &std::path::Path, run: Run, id: PaneId) -> Side {
        let end = crate::socket::connect(&panes.join(id.number().to_string()))
            .ok()
            .and_then(|stream| End::new(stream).ok())
            .map(|mut end| {
                end.send(&ToPane::Join { run });
                end.send(&ToSide::Watch);
                end
            });
        Side { end, mirror: None }
    }

    /// Paints `drawn`, the pane's screen as it sent it, onto its copy.
    fn take(&mut self, drawn: Drawn, id: PaneId) {
        if drawn.full
            || self
                .mirror
                .as_ref()
                .is_none_or(|m| m.screen().size() != drawn.size)
        {
            match fux_vt::Parser::with_options(drawn.size, 0, MIRROR) {
                Ok(mirror) => self.mirror = Some(mirror),
                Err(error) => {
                    log!("{id}: its copy at {:?}: {error}", drawn.size);
                    return;
                }
            }
        }
        let Some(mirror) = &mut self.mirror else {
            return;
        };
        let graphics = fux_vt::Graphics::from(drawn.draws);
        if mirror.screen().graphics() != graphics {
            mirror.set_host_graphics(graphics);
        }
        mirror.set_picture_bytes(drawn.picture_bytes);
        // Every byte read, budget or no: the bytes paint one screen. A
        // refusal loses only what it was for, and the mirror shows the rest.
        let _ = mirror.process_all(&drawn.screen, &mut ());
    }
}

/// What tells a picture shown apart from another: its pixels, by the `Arc`
/// that holds them, which no other picture can take while it shows, and
/// the part of them shown.
fn picture_id(picture: &Visible) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    std::sync::Arc::as_ptr(&picture.picture)
        .addr()
        .hash(&mut hasher);
    picture.crop.hash(&mut hasher);
    hasher.finish()
}

/// An attached client's screen as it shows it, and its pictures.
#[derive(Default)]
struct Screen {
    shown: Grid,
    /// The grid each screen is composed into, then shown.
    spare: Grid,
    /// Whether `shown` is what the client's terminal shows.
    known: bool,
    pictures: Vec<(u64, (u16, u16, u16, u16))>,
    sixels: Sixels,
}

impl Screen {
    /// Paints `spare`, composed, onto the client's terminal as it differs
    /// from what it shows, with `pictures`: a picture gone or moved has its
    /// cells painted again, blank, which takes its pixels away; one whose
    /// cells this paint paints over, or new, is sent after the text.
    fn paint(&mut self, pictures: &[Visible], out: &mut ByteQueue) {
        let known = self.known;
        let now: Vec<(u64, (u16, u16, u16, u16))> =
            pictures.iter().map(|p| (picture_id(p), p.area)).collect();
        for (_, area) in (self.pictures.iter()).filter(|p| known && !now.contains(p)) {
            self.shown.forget_area(*area);
        }
        let spare = &self.spare;
        let send: Vec<bool> = (now.iter())
            .map(|p| {
                !known
                    || spare.clears(&self.shown)
                    || !self.pictures.contains(p)
                    || !spare.same_in(&self.shown, p.1)
            })
            .collect();
        let mut bytes = Vec::new();
        let changed = crate::render::paint_into(&self.shown, known, spare, &mut bytes);
        std::mem::swap(&mut self.shown, &mut self.spare);
        if changed {
            fux_wire::protocol::split_into(&bytes, ToAttached::Paint, out);
            self.known = true;
        }
        let mut drawn = Vec::new();
        for (picture, _) in pictures.iter().zip(&send).filter(|(_, send)| **send) {
            self.sixels.picture_into(picture, &mut drawn);
        }
        if !drawn.is_empty() {
            fux_wire::protocol::split_into(&drawn, ToAttached::Paint, out);
        }
        self.pictures = now;
    }

    /// Its terminal's cursor overrides given back as its own, at detach;
    /// one never set is left as it was.
    fn restore(&self, out: &mut ByteQueue) {
        let mut cursor = Vec::new();
        let modes = &self.shown.modes;
        crate::render::cursor_shape(
            modes.cursor_shape,
            fux_vt::CursorShape::Initial,
            &mut cursor,
        );
        crate::render::cursor_colour(modes.cursor_colour, None, &mut cursor);
        if !cursor.is_empty() {
            fux_wire::protocol::split_into(&cursor, ToAttached::Paint, out);
        }
    }
}

/// A frame its client sent, to send its server: its bytes, what it did to
/// the client's screen, and the inputs or paste text it keeps of it, which
/// go from it to the panes asked (`ToConn::Deliver`), never through the
/// server.
struct Sent {
    frame: Vec<u8>,
    typed: Typed,
    body: Option<Vec<Input>>,
}

/// The next whole frame `decoder` holds of those `want` expects, sent
/// again as it came, but for its body, if `bodies` (a server that starts
/// pane processes; `ToConn::Hello`): none if more bytes are needed, or none
/// is expected. An attached client's frame says whether it typed (its echo
/// is painted at once) or resized (its terminal is painted whole).
fn next_frame(decoder: &mut Decoder, want: Want, bodies: bool) -> Result<Option<Sent>, String> {
    fn again<'a, F: Frame<'a>>(frame: Option<F>) -> Option<Sent> {
        frame.map(|frame| Sent {
            frame: frame.encode(),
            typed: Typed::No,
            body: None,
        })
    }
    let frame = match want {
        Want::Hello => again(decoder.frame::<Hello>().map_err(|e| e.to_string())?),
        Want::Frames(Expect::Attach) => {
            again(decoder.frame::<Attach>().map_err(|e| e.to_string())?)
        }
        Want::Frames(Expect::Asked) => {
            let asked = decoder.frame::<Asked>().map_err(|e| e.to_string())?;
            asked.map(|mut asked| {
                use fux_wire::command::{Command, Request};
                let body = match &mut asked.request {
                    Request::Do(
                        Command::SendKeys { inputs, .. }
                        | Command::Client {
                            action: fux_wire::command::ClientAction::Keys(inputs),
                            ..
                        },
                    ) if bodies && !inputs.is_empty() => Some(std::mem::take(inputs)),
                    Request::Do(_)
                    | Request::Wait(_)
                    | Request::NextEvents { .. }
                    | Request::Read { .. } => None,
                };
                Sent {
                    frame: asked.encode(),
                    typed: Typed::No,
                    body,
                }
            })
        }
        Want::Frames(Expect::Attached) => {
            let frame = decoder
                .frame::<AttachedFrame>()
                .map_err(|e| e.to_string())?;
            frame.map(|mut frame| {
                let mut body = None;
                if let (true, AttachedFrame::Act(act)) = (bodies, &mut frame)
                    && let fux_wire::act::Act::Paste(text) = &mut **act
                {
                    body = Some(vec![Input::Paste(std::mem::take(text))]);
                }
                use fux_wire::act::Act;
                let typed = match &frame {
                    AttachedFrame::Act(act) if matches!(**act, Act::Key(_) | Act::Paste(_)) => {
                        Typed::Key
                    }
                    AttachedFrame::Resize(_) => Typed::Resize,
                    AttachedFrame::Act(_) | AttachedFrame::Detach | AttachedFrame::Answer(..) => {
                        Typed::No
                    }
                };
                Sent {
                    frame: frame.encode(),
                    typed,
                    body,
                }
            })
        }
        Want::Answer | Want::Nothing => None,
    };
    Ok(frame)
}

/// The pane `id` of run `run` reached at its socket in `panes`, its first
/// word said (`ToPane::Join`); none if it can't be.
fn join(panes: &std::path::Path, run: Run, id: PaneId) -> Option<End> {
    reach(&panes.join(id.number().to_string()), run)
}

/// Where client `client`'s connection listens for bodies (`ToConn::Listen`).
fn client_socket(panes: &std::path::Path, client: ClientId) -> PathBuf {
    panes.join(format!("c{}", client.number()))
}

/// What listens at `socket`, reached, its first word said.
fn reach(socket: &std::path::Path, run: Run) -> Option<End> {
    let stream = crate::socket::connect(socket).ok()?;
    let mut end = End::new(stream).ok()?;
    end.send(&ToPane::Join { run });
    Some(end)
}

/// Where an attached client's connection takes bodies other connections
/// send its client (`ToConn::Keys`): its socket, removed as it ends, and
/// those reaching it, each once it has said its first word (`Join`).
struct Listening {
    listener: crate::socket::Listener,
    path: PathBuf,
    peers: Vec<(End, bool)>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Listening {
    /// Listens as client `client`'s connection, in `panes`.
    fn at(panes: &std::path::Path, client: ClientId) -> Option<Listening> {
        let path = client_socket(panes, client);
        let listener = crate::socket::listen_at(&path).ok()?;
        Some(Listening {
            listener,
            path,
            peers: Vec::new(),
        })
    }

    /// Takes those reaching it, and the bodies they sent, into `bodies`: of
    /// run `run`, from its own user.
    fn take(
        &mut self,
        ready: &[bool],
        buffer: &mut [u8],
        run: Run,
        bodies: &mut BTreeMap<u64, Vec<Input>>,
    ) {
        if ready.first().copied().unwrap_or(false) {
            while let Ok(reached) = crate::socket::accept(&mut self.listener) {
                let ours = crate::socket::peer(&reached).is_ok_and(|peer| peer.user.is_current());
                if let (true, Ok(end)) = (ours, crate::socket::serve(reached).and_then(End::new)) {
                    self.peers.push((end, false));
                }
            }
        }
        let mut kept = Vec::new();
        for ((mut end, mut joined), readable) in std::mem::take(&mut self.peers)
            .into_iter()
            .zip(ready.iter().skip(1).chain(std::iter::repeat(&false)))
        {
            // One that says what it has no business saying is let go.
            let mut gone = *readable && end.read(buffer) == Read::Closed;
            loop {
                if !joined {
                    match end.next_message::<ToPane>() {
                        Ok(Some(ToPane::Join { run: theirs })) if theirs == run => joined = true,
                        Ok(None) => break,
                        Ok(Some(_)) | Err(_) => {
                            gone = true;
                            break;
                        }
                    }
                    continue;
                }
                match end.next_message::<ToSide>() {
                    Ok(Some(ToSide::Body { token, inputs })) => {
                        bodies.insert(token, inputs);
                    }
                    Ok(None) => break,
                    Ok(Some(_)) | Err(_) => {
                        gone = true;
                        break;
                    }
                }
            }
            if !gone {
                kept.push((end, joined));
            }
        }
        self.peers = kept;
    }
}

/// What it does at panes' sockets beside showing them, each on a link of
/// its own, in its loop: so that its server's end ends it whatever a pane
/// does (a stopped one takes nothing).
enum Errand {
    /// A body sent: done once its link took it all.
    Post(End),
    /// Its client's keys sent `client`'s connection as body `token`, if it
    /// could be reached: done once that took them all; its own client's
    /// reply if it could not.
    Keys {
        end: Option<End>,
        client: ClientId,
        token: u64,
    },
    /// A capture to fetch, then place: its client's reply.
    Fetch {
        end: Option<End>,
        pane: PaneId,
        placed: crate::link::Placed,
    },
    /// A client's screen to compose from `scene` and its panes' first
    /// screens, as they come: its client's reply once each came, or its
    /// pane went.
    Compose {
        client: ClientId,
        scene: Box<Scene>,
        waiting: Vec<(PaneId, End)>,
        sides: BTreeMap<PaneId, Side>,
    },
}

impl Errand {
    /// The errand making the reply `making` says (`ToConn::Make`), at
    /// `panes` of run `run`, its client's bodies
    /// `bodies` at hand.
    fn of(
        making: Making,
        panes: &std::path::Path,
        run: Run,
        bodies: &mut BTreeMap<u64, Vec<Input>>,
    ) -> Errand {
        match making {
            Making::Keys {
                client,
                body,
                token,
                ..
            } => {
                let inputs = bodies.remove(&body).unwrap_or_default();
                let end = reach(&client_socket(panes, client), run).map(|mut end| {
                    end.send(&ToSide::Body { token, inputs });
                    end
                });
                Errand::Keys { end, client, token }
            }
            Making::Capture {
                pane,
                token,
                placed,
            } => {
                let end = join(panes, run, pane).map(|mut end| {
                    end.send(&ToSide::Fetch { token });
                    end
                });
                Errand::Fetch { end, pane, placed }
            }
            Making::Client { client, scene } => {
                let mut waiting = Vec::new();
                let mut sides = BTreeMap::new();
                for &(id, _) in &scene.placement.panes {
                    sides.insert(
                        id,
                        Side {
                            end: None,
                            mirror: None,
                        },
                    );
                    if let Some(mut end) = join(panes, run, id) {
                        end.send(&ToSide::Watch);
                        waiting.push((id, end));
                    }
                }
                Errand::Compose {
                    client,
                    scene,
                    waiting,
                    sides,
                }
            }
        }
    }

    /// Its links, each with what it waits for.
    fn links(&self) -> Vec<(&End, fuxix::poll::Events)> {
        use fuxix::poll::Events;
        let flags = |end: &End, read: bool| {
            let mut flags = if read { Events::IN } else { Events::NONE };
            if !end.out.is_empty() {
                flags |= Events::OUT;
            }
            flags
        };
        match self {
            Errand::Post(end) => vec![(end, flags(end, false))],
            Errand::Keys { end, .. } => end.iter().map(|end| (end, flags(end, false))).collect(),
            Errand::Fetch { end, .. } => end.iter().map(|end| (end, flags(end, true))).collect(),
            Errand::Compose { waiting, .. } => waiting
                .iter()
                .map(|(_, end)| (end, flags(end, true)))
                .collect(),
        }
    }

    /// Goes on, its links' readiness `ready`: done once it has no more to
    /// do, with its client's reply if it makes one.
    fn on(&mut self, ready: &[bool], buffer: &mut [u8], run: Run) -> Option<Option<Vec<u8>>> {
        match self {
            Errand::Post(end) => match end.flush() {
                Ok(false) => None,
                Ok(true) | Err(_) => Some(None),
            },
            // Taken all of, its server says when they are typed; taken by
            // none, it says so.
            Errand::Keys { end, client, .. } => match end.as_mut().map(End::flush) {
                Some(Ok(false)) => None,
                Some(Ok(true)) => Some(None),
                Some(Err(_)) | None => Some(Some(typed(run, Some(*client)))),
            },
            Errand::Fetch { end, pane, placed } => {
                let Some(end) = end else {
                    return Some(Some(reply(run, Err(gone_capture(*pane)))));
                };
                let gone = end.flush().is_err()
                    || ready.first().copied().unwrap_or(false) && end.read(buffer) == Read::Closed;
                match end.next_message::<FromSide>() {
                    Ok(Some(FromSide::Held {
                        captured: Some(mut captured),
                        ..
                    })) => {
                        crate::capture::place(&mut captured, placed);
                        Some(Some(reply(run, Ok(captured))))
                    }
                    Ok(Some(FromSide::Held { captured: None, .. } | FromSide::Drawn(_)))
                    | Err(_) => Some(Some(reply(run, Err(gone_capture(*pane))))),
                    Ok(None) if gone => Some(Some(reply(run, Err(gone_capture(*pane))))),
                    Ok(None) => None,
                }
            }
            Errand::Compose {
                client,
                scene,
                waiting,
                sides,
            } => {
                let mut left = Vec::new();
                for ((id, mut end), readable) in std::mem::take(waiting).into_iter().zip(ready) {
                    let gone =
                        end.flush().is_err() || *readable && end.read(buffer) == Read::Closed;
                    match end.next_message::<FromSide>() {
                        Ok(Some(FromSide::Drawn(drawn))) => {
                            if let Some(side) = sides.get_mut(&id) {
                                side.take(*drawn, id);
                            }
                        }
                        Ok(Some(FromSide::Held { .. })) | Err(_) => {}
                        Ok(None) if gone => {}
                        Ok(None) => left.push((id, end)),
                    }
                }
                *waiting = left;
                if !waiting.is_empty() {
                    return None;
                }
                let screens =
                    |id: PaneId| sides.get(&id)?.mirror.as_ref().map(fux_vt::Parser::screen);
                let mut grid = Grid::default();
                let captured = crate::render::compose_scene(scene, &screens, &mut grid)
                    .map(|()| Box::new(crate::capture::client(*client, &grid)))
                    .map_err(|e| format!("capturing {client}: {e}"));
                Some(Some(reply(run, captured)))
            }
        }
    }
}

fn gone_capture(pane: PaneId) -> String {
    format!("{pane}'s capture is gone: its own process ended")
}

/// The reply to keys for a client (`Making::Keys`): typed, or not, as they
/// did not reach the client `lost`.
fn typed(run: Run, lost: Option<ClientId>) -> Vec<u8> {
    framed(
        run,
        match lost {
            None => Response::Answered(PaneResults::Many(Vec::new())),
            Some(client) => Response::Failed {
                code: fux_wire::code::Code::Indeterminate,
                message: format!("the keys did not reach {client}'s connection"),
            },
        },
    )
}

/// A capture its connection makes itself (`Making`), framed for its client.
fn reply(run: Run, captured: Result<Box<fux_wire::answer::Captured>, String>) -> Vec<u8> {
    let response = match captured {
        Ok(captured) => Response::Returned(fux_wire::protocol::Body::Capture(captured)),
        Err(message) => Response::Failed {
            code: fux_wire::code::Code::Indeterminate,
            message,
        },
    };
    framed(run, response)
}

/// A reply its connection makes itself, framed for its client.
fn framed(run: Run, response: Response) -> Vec<u8> {
    use fux_wire::protocol::{Reply, ToCommand};
    let reply = Reply {
        run: Some(run),
        response,
    };
    let bytes = fux_wire::wire::encode(&reply);
    let mut out = ByteQueue::default();
    fux_wire::protocol::split_into(&bytes, ToCommand::Part, &mut out);
    out.push_with(|frames| ToCommand::PartsEnd.encode_into(frames));
    out.as_slice().to_vec()
}

/// What a client's frame did to its screen (`next_frame`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Typed {
    No,
    Key,
    Resize,
}

/// An attached client's screen as its connection composes it: its scene,
/// the panes it shows, and when it is painted next.
struct Composer {
    /// Where its run's panes listen, and the run (`ToConn::Hello`).
    panes: Option<(PathBuf, Run)>,
    scene: Option<Scene>,
    sides: BTreeMap<PaneId, Side>,
    screen: Screen,
    clock: Clock,
    /// What it shows changed since it was last painted.
    dirty: bool,
}

impl Composer {
    fn new(now: Instant) -> Composer {
        Composer {
            panes: None,
            scene: None,
            sides: BTreeMap::new(),
            screen: Screen::default(),
            clock: Clock {
                next: now,
                due: now,
                echo: None,
            },
            dirty: false,
        }
    }

    /// Its scene now `scene`: the panes it shows reached, those it no longer
    /// does let go.
    fn scene(&mut self, scene: Scene) {
        let shown: Vec<PaneId> = (scene.placement.panes.iter())
            .map(|(id, _)| *id)
            .filter(|id| scene.runs_in != Some(*id))
            .collect();
        self.sides.retain(|id, _| shown.contains(id));
        if let Some((panes, run)) = &self.panes {
            for id in shown {
                self.sides
                    .entry(id)
                    .or_insert_with(|| Side::reach(panes, *run, id));
            }
        }
        if self
            .scene
            .as_ref()
            .is_none_or(|s| (s.rows, s.cols) != (scene.rows, scene.cols))
        {
            self.screen.known = false;
        }
        self.scene = Some(scene);
        self.dirty = true;
    }

    /// When it is to be painted, if it is: once its client took the last.
    fn due(&self, idle: bool) -> Option<Instant> {
        (self.scene.is_some() && (self.dirty || !self.screen.known) && idle)
            .then_some(self.clock.due)
    }

    /// Composes its screen and paints it into `out`.
    fn paint(&mut self, out: &mut ByteQueue, now: Instant) -> Result<(), String> {
        let Some(scene) = &self.scene else {
            return Ok(());
        };
        let sides = &self.sides;
        let screens = |id: PaneId| sides.get(&id)?.mirror.as_ref().map(fux_vt::Parser::screen);
        crate::render::compose_scene(scene, &screens, &mut self.screen.spare)
            .map_err(|e| format!("rendering its client's screen: {e}"))?;
        let pictures = crate::render::pictures(scene, &screens);
        self.screen.paint(&pictures, out);
        self.clock.painted(now);
        self.dirty = false;
        Ok(())
    }

    /// Body `token`, `inputs`, sent to each of `panes` (`ToSide::Body`):
    /// on its link to one it shows, else on one made for it, which goes on
    /// as an errand till its pane took it all.
    fn deliver(&mut self, token: u64, inputs: &[Input], panes: &[PaneId]) -> Vec<Errand> {
        let message = ToSide::Body {
            token,
            inputs: inputs.to_vec(),
        };
        let mut posts = Vec::new();
        for id in panes {
            match self.sides.get_mut(id).and_then(|side| side.end.as_mut()) {
                Some(end) => end.send(&message),
                None => {
                    let reached = self
                        .panes
                        .as_ref()
                        .and_then(|(panes, run)| join(panes, *run, *id));
                    if let Some(mut end) = reached {
                        end.send(&message);
                        posts.push(Errand::Post(end));
                    }
                }
            }
        }
        posts
    }

    /// What the panes it shows sent: their screens, each taken.
    fn heard(&mut self, buffer: &mut [u8], ready: &[(PaneId, bool)], now: Instant) {
        for &(id, readable) in ready {
            let Some(side) = self.sides.get_mut(&id) else {
                continue;
            };
            let Some(end) = &mut side.end else {
                continue;
            };
            let closed = readable && end.read(buffer) == Read::Closed;
            let mut drawn = Vec::new();
            while let Ok(Some(FromSide::Drawn(screen))) = end.next_message::<FromSide>() {
                drawn.push(screen);
            }
            if !drawn.is_empty() {
                end.send(&ToSide::Taken);
            }
            // A pane whose process is gone shows its last screen.
            if closed || end.flush().is_err() {
                side.end = None;
            }
            for screen in drawn {
                side.take(*screen, id);
                self.clock.output(id, now);
                self.dirty = true;
            }
        }
    }
}

/// A client's connection's own process (`fux __conn`), started before its
/// client came (`Server::spare`): it waits at its server's socket,
/// `listener`, which is also its stdout, for a client of the server's own
/// user, as the server did; then lets the socket go, its stdout pointed at
/// `/dev/null`, tells its server it took a client, and serves it (`serve`),
/// its server on `server`. It never listens itself, so a client is still
/// told the server's pid (`SO_PEERCRED`). Its server gone first, it ends.
#[cfg(unix)]
pub fn wait(listener: std::os::unix::net::UnixListener, server: Stream) -> Result<(), String> {
    use fuxix::poll::{Events, PollFd};
    let mut link = End::new(server).map_err(|e| format!("its server: {e}"))?;
    let mut buffer = [0u8; 4096];
    let mut said = Vec::new();
    loop {
        let mut fds = [
            PollFd::new(&listener, Events::IN),
            PollFd::new(&link.stream, Events::IN),
        ];
        match fuxix::poll::poll(&mut fds, None) {
            Ok(_) | Err(fuxix::Errno::INTR) => {}
            Err(error) => return Err(format!("poll: {error}")),
        }
        let [theirs, ours] = fds.map(|fd| !fd.revents().is_empty());
        if ours {
            let closed = link.read(&mut buffer) == Read::Closed;
            while let Some(message) = (link.next_message::<ToConn>())
                .map_err(|e| format!("its server sent a bad frame: {e}"))?
            {
                said.push(message);
            }
            if closed {
                return Ok(());
            }
        }
        if !theirs {
            continue;
        }
        let client = match listener.accept() {
            Ok((client, _)) => client,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                continue;
            }
            // Out of descriptors, say: its server starts another later.
            Err(e) => return Err(format!("accept: {e}")),
        };
        if let Some(why) = crate::socket::stranger(fuxix::socket::peer(&client)) {
            log!("{why}");
            crate::socket::refuse(client, &why);
            continue;
        }
        drop(listener);
        // Its client is told why it is not served, should that fail.
        let detached = std::fs::File::open("/dev/null")
            .map_err(|e| format!("/dev/null: {e}"))
            .and_then(|null| {
                fuxix::io::redirect_stdout(&null).map_err(|e| format!("its stdout: {e}"))
            });
        if let Err(why) = detached {
            crate::socket::refuse(client, &format!("this connection's process failed: {why}"));
            return Err(why);
        }
        link.send(&FromConn::Accepted);
        return serve(client, link, said);
    }
}

/// A connection's process for a client its server took itself (`fux __conn
/// taken`): its client on `client`, its server on `server`.
pub fn taken(client: Stream, server: Stream) -> Result<(), String> {
    let link = End::new(server).map_err(|e| format!("its server: {e}"))?;
    serve(client, link, Vec::new())
}

/// A client's connection's own process at work (`serve_on`): its client
/// on `client`, its server on `link`, which said `said` before. One that
/// fails tells its client why before it closes (`Exit`, which every stage
/// reads), as much as the client takes at once: no connection its process
/// ends closes without a word.
pub fn serve(client: Stream, link: End, said: Vec<ToConn>) -> Result<(), String> {
    let served = serve_on(&client, link, said);
    if let Err(why) = &served {
        let why = format!("this connection's process failed: {why}");
        let mut out = ByteQueue::default();
        out.push_with(|frames| fux_wire::protocol::ToCommand::Exit(&why).encode_into(frames));
        let _ = write(&client, &mut out);
    }
    served
}

/// The loop of a client's connection's own process, until its client or
/// its server goes.
fn serve_on(client: &Stream, mut link: End, said: Vec<ToConn>) -> Result<(), String> {
    use fuxix::poll::{Events, PollFd};
    crate::socket::nonblocking(client).map_err(|e| format!("its client: {e}"))?;
    let mut decoder = Decoder::default();
    let mut out = ByteQueue::default();
    let mut composer = Composer::new(Instant::now());
    let mut want = Want::Hello;
    let (mut answered, mut shut, mut closing) = (false, false, false);
    // Its client's request waits on someone else, its server said: no
    // answer is on its way.
    let mut waits = false;
    // Its client hung up, told unasked while it was not read: not a side
    // shut, which leaves its answers a reader.
    let mut gone = false;
    // Bytes its server sent its client, not all written yet: its server is
    // told once they are (`Flushed`). Its own paints and replies are no
    // news to it.
    let mut unflushed = false;
    // Its client shut its side, and its server is told so once what it
    // sent before is taken (`Shut`).
    let (mut ended, mut told_shut) = (false, false);
    let mut buffer = vec![0u8; READ];
    let mut poll = fuxix::poll::Poll::default();
    // What it keeps of its client's frames (`Sent`), each by its number.
    let mut bodies: BTreeMap<u64, Vec<Input>> = BTreeMap::new();
    let mut numbered = 0u64;
    // What it does at panes' sockets, and whether one of them makes its
    // client's reply, which its client's next request waits for.
    let mut errands: Vec<Errand> = Vec::new();
    let mut making = false;
    // The keys for a client its client asked for, sent, till their server
    // says whether they were typed.
    let mut typing: Option<(u64, ClientId)> = None;
    // An attached client's: where other connections send it bodies, those
    // come, the keys it waits to send its client till their body comes, and
    // what its server said after them, which waits too.
    let mut listening: Option<Listening> = None;
    let mut received: BTreeMap<u64, Vec<Input>> = BTreeMap::new();
    let mut keys: Option<u64> = None;
    let mut backlog: std::collections::VecDeque<ToConn> = said.into();
    loop {
        let now = Instant::now();
        // What its client sent, as far as its stage lets it be taken.
        loop {
            match next_frame(&mut decoder, want, composer.panes.is_some()) {
                Ok(Some(Sent { frame, typed, body })) => {
                    let body = body.map(|inputs| {
                        numbered = numbered.wrapping_add(1);
                        bodies.insert(numbered, inputs);
                        numbered
                    });
                    link.send(&FromConn::Frame { frame, body });
                    match typed {
                        Typed::Key => {
                            composer.clock.echo = composer.scene.as_ref().and_then(|s| s.focus);
                        }
                        Typed::Resize => composer.screen.known = false,
                        Typed::No => {}
                    }
                    want = match want {
                        Want::Hello => Want::Nothing,
                        Want::Frames(Expect::Asked) => {
                            answered = false;
                            Want::Answer
                        }
                        Want::Frames(Expect::Attach) => Want::Nothing,
                        other @ (Want::Frames(Expect::Attached) | Want::Answer | Want::Nothing) => {
                            other
                        }
                    };
                }
                Ok(None) => break,
                Err(why) => {
                    link.send(&FromConn::Bad(why));
                    (want, shut) = (Want::Nothing, true);
                    break;
                }
            }
        }
        // Whether its stage takes its client's frames now, and whether its
        // request waits on someone else, nothing to be written meanwhile:
        // its server said so (`Waits`), or it is keys an attached client
        // is to take. Its client is read then too, to hear its end, and
        // what it sent ahead is held, `READ` at most: one that sent that
        // much ahead is refused, told why (`AHEAD`), and from then on a
        // client gone, which its server ends. With an answer on its way
        // its client is not read: what it sent ahead stays in its socket.
        let takes = !shut && !matches!(want, Want::Answer | Want::Nothing);
        let idle = want == Want::Answer && (waits || typing.is_some());
        if idle && decoder.buffered() >= READ && !(ended || shut || closing) {
            out.push_with(|frames| fux_wire::protocol::ToCommand::Exit(AHEAD).encode_into(frames));
            decoder = Decoder::default();
            link.send(&FromConn::Shut);
            (ended, told_shut) = (true, true);
        }
        // Its client sends no more: said once what it sent is taken, or
        // can't be, a frame left whole for its stage or a part of one. With
        // a request under way it is said at once if that waits on someone
        // else, or its client hung up; with an answer on its way, only once
        // that is written: a client that shut its side reads it still.
        let taken = decoder.buffered() == 0 || !decoder.ready().unwrap_or(true);
        let tells = match want {
            Want::Answer => idle || gone,
            Want::Nothing => false,
            Want::Hello | Want::Frames(_) => taken,
        };
        if ended && !told_shut && tells {
            link.send(&FromConn::Shut);
            told_shut = true;
        }
        // Its screen, once its client took the last.
        if composer.due(out.is_empty()).is_some_and(|at| at <= now) {
            composer.paint(&mut out, now)?;
        }
        // What waits for its client, as far as it takes it.
        if !write(client, &mut out) {
            return Ok(());
        }
        if out.is_empty() {
            out.shrink(64 << 10);
            if std::mem::take(&mut unflushed) {
                link.send(&FromConn::Flushed);
            }
            if want == Want::Answer && answered && !making {
                want = Want::Frames(Expect::Asked);
                continue;
            }
            // Done once what it sends panes is sent too: keys its client
            // typed as it went go on.
            if closing && errands.is_empty() {
                return Ok(());
            }
        }
        // Its server out of reach, what it would say to it is let go: its
        // link then reads to its end, what its server said before it went
        // is taken, and its client is told (`gone`, below), so that one in
        // hand whose server went before it heard of it (a spare's, taken as
        // its server stopped) is not just closed.
        if link.flush().is_err() {
            link.out.take(link.out.len());
        }
        // Its client is read while its stage takes its frames, and while
        // its request waits on someone else, to hear its end.
        let listens = !(ended || shut || closing) && (takes || idle);
        let wake = composer.due(out.is_empty());
        // An errand with no link left to wait on goes on at once.
        let timeout = match errands.iter().any(|errand| errand.links().is_empty()) {
            true => Some(Duration::ZERO),
            false => wake.map(|at| at.saturating_duration_since(now)),
        };
        let mut fds = poll.list();
        // Not read, only its hang-up is to be learned, till it is: Linux
        // tells one unasked; Windows of a read kept in flight, which `HUP`
        // and `ERR` ask for; macOS not, and there a write to it fails.
        let mut theirs = match listens {
            true => Events::IN,
            false if cfg!(windows) && !gone => Events::HUP | Events::ERR,
            false => Events::NONE,
        };
        if !out.is_empty() {
            theirs |= Events::OUT;
        }
        // Hung up and nothing left to write it, its client is waited on no
        // more: a hang-up, told unasked, would wake every wait.
        let watched = !gone || !out.is_empty();
        if watched {
            fds.push(PollFd::new(client, theirs));
        }
        // Where its server's link is among those waited on, the rest after.
        let ours_at = usize::from(watched);
        let rest = ours_at.saturating_add(1);
        let ours = match link.out.is_empty() {
            true => Events::IN,
            false => Events::IN | Events::OUT,
        };
        fds.push(PollFd::new(&link.stream, ours));
        let sides: Vec<PaneId> = (composer.sides.iter())
            .filter(|(_, side)| side.end.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in &sides {
            if let Some(end) = composer.sides.get(id).and_then(|s| s.end.as_ref()) {
                let mut flags = Events::IN;
                if !end.out.is_empty() {
                    flags |= Events::OUT;
                }
                fds.push(PollFd::new(&end.stream, flags));
            }
        }
        let listened = listening
            .as_ref()
            .map_or(0, |l| l.peers.len().saturating_add(1));
        if let Some(listening) = &listening {
            fds.push(PollFd::new(&listening.listener, Events::IN));
            for (end, _) in &listening.peers {
                fds.push(PollFd::new(&end.stream, Events::IN));
            }
        }
        let mut counts = Vec::with_capacity(errands.len());
        for errand in &errands {
            let links = errand.links();
            counts.push(links.len());
            for (end, flags) in links {
                fds.push(PollFd::new(&end.stream, flags));
            }
        }
        match fds.poll(timeout) {
            Ok(_) | Err(fuxix::Errno::INTR) => {}
            Err(error) => return Err(format!("poll: {error}")),
        }
        let ready: Vec<Events> = fds.revents().collect();
        let now = Instant::now();
        // A client read is read to its end, so that its server is told
        // (`Shut`): as much at once as `READ` for a stage that takes its
        // frames, and for a request that waits no more than brings what is
        // held to `READ`. One not read that hung up is gone.
        let client_ready = match watched {
            true => ready.first().copied().unwrap_or(Events::NONE),
            false => Events::NONE,
        };
        if !listens && client_ready.intersects(Events::HUP | Events::ERR) {
            (ended, gone) = (true, true);
        }
        if listens && !client_ready.is_empty() {
            let room = match takes {
                true => READ,
                false => READ.saturating_sub(decoder.buffered()),
            };
            let mut read = 0usize;
            while read < room {
                let into = buffer
                    .get_mut(..room.saturating_sub(read))
                    .unwrap_or_default();
                match fuxix::io::read(client, into) {
                    Ok(n) if n > 0 => {
                        read = read.saturating_add(n);
                        decoder.push(buffer.get(..n).unwrap_or_default());
                    }
                    Err(fuxix::Errno::INTR) => {}
                    Err(fuxix::Errno::AGAIN) => break,
                    Ok(_) | Err(_) => {
                        ended = true;
                        break;
                    }
                }
            }
        }
        let heard: Vec<(PaneId, bool)> = (sides.iter().zip(ready.iter().skip(rest)))
            .map(|(id, flags)| (*id, !flags.is_empty()))
            .collect();
        composer.heard(&mut buffer, &heard, now);
        // Bodies other connections sent its client.
        let mut at = sides.len().saturating_add(rest);
        if let (Some(listen), Some((_, run))) = (&mut listening, &composer.panes) {
            let readable: Vec<bool> = (ready.iter().skip(at).take(listened))
                .map(|flags| !flags.is_empty())
                .collect();
            listen.take(&readable, &mut buffer, *run, &mut received);
        }
        at = at.saturating_add(listened);
        // Its errands, each as far as its links let it go now.
        let mut going = Vec::with_capacity(errands.len());
        for (mut errand, count) in std::mem::take(&mut errands).into_iter().zip(counts) {
            let readable: Vec<bool> = (ready.iter().skip(at).take(count))
                .map(|flags| !flags.is_empty())
                .collect();
            at = at.saturating_add(count);
            let run = composer.panes.as_ref().map(|(_, run)| *run);
            match run.and_then(|run| errand.on(&readable, &mut buffer, run)) {
                None if run.is_some() => going.push(errand),
                None | Some(None) => {}
                Some(Some(made)) => {
                    if let Errand::Keys { .. } = errand {
                        typing = None;
                    }
                    out.push(&made);
                    making = false;
                }
            }
        }
        errands = going;
        let mut gone = false;
        if ready.get(ours_at).is_some_and(|f| !f.is_empty()) {
            gone = link.read(&mut buffer) == Read::Closed;
            loop {
                match link.next_message::<ToConn>() {
                    // A body none will send now holds nothing back; one that
                    // won't reach its client is not typed.
                    Ok(Some(ToConn::Drop { token })) => {
                        received.remove(&token);
                        backlog.retain(|m| !matches!(m, ToConn::Keys { token: t } if *t == token));
                        if keys == Some(token) {
                            keys = None;
                        }
                        if let (Some((_, run)), Some((asked, client))) = (&composer.panes, typing)
                            && asked == token
                        {
                            errands.retain(
                                |e| !matches!(e, Errand::Keys { token: t, .. } if *t == token),
                            );
                            out.push(&typed(*run, Some(client)));
                            (typing, making) = (None, false);
                        }
                    }
                    Ok(Some(ToConn::Typed { token })) => {
                        if let (Some((_, run)), Some((asked, _))) = (&composer.panes, typing)
                            && asked == token
                        {
                            out.push(&typed(*run, None));
                            (typing, making) = (None, false);
                        }
                    }
                    Ok(Some(message)) => backlog.push_back(message),
                    Ok(None) => break,
                    Err(error) => return Err(format!("its server sent a bad frame: {error}")),
                }
            }
        }
        // What its server said, in order: keys for its client whose body has
        // not come yet hold back what follows them.
        loop {
            if let Some(token) = keys {
                let Some(inputs) = received.remove(&token) else {
                    break;
                };
                let frame = ToAttached::Keys(inputs);
                out.push_with(|frames| frame.encode_into(frames));
                keys = None;
            }
            let Some(message) = backlog.pop_front() else {
                break;
            };
            {
                match message {
                    ToConn::Bytes(bytes) => {
                        out.push(&bytes);
                        unflushed = true;
                    }
                    ToConn::Expect(expect) => want = Want::Frames(expect),
                    ToConn::Answered => (answered, waits) = (true, false),
                    ToConn::Waits(now) => waits = now,
                    ToConn::Hello { panes, run } => composer.panes = Some((panes, run)),
                    ToConn::Scene(scene) => composer.scene(*scene),
                    ToConn::Deliver { body, token, panes } => {
                        let inputs = bodies.remove(&body).unwrap_or_default();
                        errands.extend(composer.deliver(token, &inputs, &panes));
                    }
                    ToConn::Listen { client: me } => {
                        listening = composer
                            .panes
                            .as_ref()
                            .and_then(|(panes, _)| Listening::at(panes, me));
                    }
                    ToConn::Keys { token } => keys = Some(token),
                    ToConn::Drop { .. } | ToConn::Typed { .. } => {}
                    ToConn::Make(made) => {
                        if let Some((panes, run)) = &composer.panes {
                            if let Making::Keys { client, token, .. } = *made {
                                typing = Some((token, client));
                            }
                            errands.push(Errand::of(*made, panes, *run, &mut bodies));
                            making = true;
                        }
                    }
                    ToConn::Detached => composer.screen.restore(&mut out),
                    ToConn::Close => (closing, want) = (true, Want::Nothing),
                }
            }
        }
        // Its server gone, so is it, once it has taken what that said
        // before: what it holds for its client is let go, but what one
        // write takes. A crashed server's clients have nothing left to be
        // served.
        // Gone with no last word for its client (`Close`), its server
        // ended, or let it go, without one: its client is told that much.
        if gone {
            if !closing {
                out.push_with(|frames| {
                    fux_wire::protocol::ToCommand::Exit(SERVER_GONE).encode_into(frames)
                });
            }
            let _ = write(client, &mut out);
            return Ok(());
        }
    }
}

/// Writes what waits for its client, as far as the client takes it now:
/// false once the client is gone.
fn write(client: &Stream, out: &mut ByteQueue) -> bool {
    while !out.is_empty() {
        match fuxix::io::write(client, out.as_slice()) {
            Ok(0) | Err(fuxix::Errno::AGAIN) => break,
            Ok(n) => out.take(n),
            Err(fuxix::Errno::INTR) => {}
            Err(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{Composer, Screen};
    use fux_vt::bytes::ByteQueue;
    use fux_wire::protocol::{Decoder, ToAttached};
    use std::time::{Duration, Instant};

    /// What a client was sent: its paints' bytes.
    fn painted(out: &ByteQueue) -> Vec<u8> {
        let mut decoder = Decoder::default();
        decoder.push(out.as_slice());
        let mut painted = Vec::new();
        while let Ok(Some(ToAttached::Paint(bytes))) = decoder.frame() {
            painted.extend_from_slice(&bytes);
        }
        painted
    }

    /// A client let go gives its terminal back only the cursor colour a
    /// pane set (OSC 112), and the cursor's shape as the terminal's own
    /// (DECSCUSR 0, then xterm's 7) only if a pane set one: by ctlseqs, 0 is
    /// a blinking block, which a terminal fux never told otherwise was not
    /// asked to be.
    #[test]
    fn detaching_gives_back_only_the_cursor_a_pane_set() {
        for (shape, colour, given) in [
            (fux_vt::CursorShape::Initial, None, ""),
            (
                fux_vt::CursorShape::Initial,
                Some([0, 255, 0].into()),
                "\x1b]112\x1b\\",
            ),
            (fux_vt::CursorShape::SteadyBar, None, "\x1b[0 q\x1b[7 q"),
        ] {
            let mut screen = Screen::default();
            screen.shown.modes.cursor_shape = shape;
            screen.shown.modes.cursor_colour = colour;
            let mut out = ByteQueue::default();
            screen.restore(&mut out);
            assert_eq!(String::from_utf8_lossy(&painted(&out)), given, "{shape:?}");
        }
    }

    /// A keystroke's echo is painted at once, not held to `PAINT` behind
    /// the paint before it; other output waits for it, or for the panes to
    /// go quiet.
    #[test]
    fn an_echo_is_painted_at_once() {
        let now = Instant::now();
        let mut composer = Composer::new(now);
        composer.clock.painted(now);
        let pane = fux_wire::id::PaneId::of(1);
        let later = now + Duration::from_millis(1);
        composer.clock.output(pane, later);
        assert!(composer.clock.due > later, "output waits");
        composer.clock.echo = Some(pane);
        composer.clock.output(pane, later);
        assert_eq!(composer.clock.due, later, "its echo does not");
    }

    /// A client whose screen is forgotten (a resize) is due a whole paint
    /// though nothing it shows changed; but not while its last paint is
    /// still being written.
    #[test]
    fn a_forgotten_screen_is_due_a_paint_once_the_last_is_sent() -> Result<(), String> {
        let (session, client) = crate::session::testing::attached(10, 40)?;
        let mut composer = Composer::new(Instant::now());
        composer.scene(crate::render::scene(&session, client).ok_or("a scene")?);
        composer.dirty = false;
        composer.screen.known = true;
        assert_eq!(composer.due(true), None, "nothing to paint");
        composer.screen.known = false;
        assert!(composer.due(true).is_some());
        assert_eq!(composer.due(false), None, "one paint in flight");
        Ok(())
    }

    /// What its server said before it went is passed on, though its end
    /// comes in the same read: a reply sent as the server dies arrives.
    #[cfg(unix)]
    #[test]
    fn what_its_server_said_before_its_end_is_passed_on() -> Result<(), String> {
        use crate::link::{End, ToConn};
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        let e = |error: std::io::Error| error.to_string();
        let (client, mut theirs) = UnixStream::pair().map_err(e)?;
        let (server, link) = UnixStream::pair().map_err(e)?;
        let mut said = End::new(server).map_err(e)?;
        said.send(&ToConn::Bytes(b"the reply".to_vec()));
        said.flush_all().map_err(|errno| errno.to_string())?;
        drop(said);
        super::serve(client, End::new(link).map_err(e)?, Vec::new())?;
        let mut got = Vec::new();
        theirs.read_to_end(&mut got).map_err(e)?;
        // And then that its server went, which had no last word for it.
        let said = got.strip_prefix(b"the reply").ok_or("no reply")?;
        let mut decoder = Decoder::default();
        decoder.push(said);
        assert!(matches!(
            decoder.frame(),
            Ok(Some(fux_wire::protocol::ToCommand::Exit(_)))
        ));
        Ok(())
    }

    /// A spare that took a client as its server stopped, its `Accepted`
    /// not yet sent when its server let its link go, can't reach its
    /// server with a client in hand: it tells that client its server went,
    /// and does not just close it.
    #[cfg(unix)]
    #[test]
    fn a_client_taken_as_its_server_went_is_told_so() -> Result<(), String> {
        use crate::link::{End, FromConn};
        use fux_wire::protocol::Greeting;
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        let e = |error: std::io::Error| error.to_string();
        let (client, mut theirs) = UnixStream::pair().map_err(e)?;
        let (server, link) = UnixStream::pair().map_err(e)?;
        // As `wait` leaves it: its first word said, not yet written.
        let mut link = End::new(link).map_err(e)?;
        link.send(&FromConn::Accepted);
        drop(server);
        super::serve(client, link, Vec::new())?;
        let mut got = Vec::new();
        theirs.read_to_end(&mut got).map_err(e)?;
        let mut decoder = Decoder::default();
        decoder.push(&got);
        // Its client, which has had no greeting, reads it as one.
        assert!(matches!(decoder.frame(), Ok(Some(Greeting::Exit(_)))));
        Ok(())
    }

    /// A connection's process that fails tells its client why before it
    /// closes: here its server sent it what is no message.
    #[cfg(unix)]
    #[test]
    fn a_process_that_fails_tells_its_client_why() -> Result<(), String> {
        use fux_wire::protocol::ToCommand;
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let e = |error: std::io::Error| error.to_string();
        let (client, mut theirs) = UnixStream::pair().map_err(e)?;
        let (mut server, link) = UnixStream::pair().map_err(e)?;
        server.write_all(&[0, 0, 0, 0]).map_err(e)?;
        let failed = super::serve(client, crate::link::End::new(link).map_err(e)?, Vec::new());
        assert!(failed.is_err(), "it took what is no message");
        let mut got = Vec::new();
        theirs.read_to_end(&mut got).map_err(e)?;
        let mut decoder = Decoder::default();
        decoder.push(&got);
        assert!(matches!(decoder.frame(), Ok(Some(ToCommand::Exit(_)))));
        Ok(())
    }

    /// A client whose request waits for its answer, which its server never
    /// gives: its end of its socket, its server's end of its process's
    /// link, and its process.
    #[cfg(unix)]
    type Waits = (
        std::os::unix::net::UnixStream,
        crate::link::End,
        std::thread::JoinHandle<Result<(), String>>,
    );

    #[cfg(unix)]
    fn waiting() -> Result<Waits, String> {
        let (theirs, mut server, conn) = asked(false)?;
        server.send(&crate::link::ToConn::Waits(true));
        server.flush_all().map_err(|errno| errno.to_string())?;
        Ok((theirs, server, conn))
    }

    /// A client whose request its process took and its server has: one
    /// that sent it and shut its side at once, if `shut`. Nothing says yet
    /// that the request waits.
    #[cfg(unix)]
    fn asked(shut: bool) -> Result<Waits, String> {
        use crate::link::{End, Expect, FromConn, ToConn};
        use fux_wire::protocol::{Asked, Frame, Hello, PROTOCOL, Role};
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let e = |error: std::io::Error| error.to_string();
        let hello = Hello {
            protocol: PROTOCOL,
            version: "0".into(),
            role: Role::Command,
        };
        let asked = Asked {
            after: None,
            request: fux_wire::command::Command::Inspect {
                scope: fux_wire::inspect::Scope::All,
                projection: fux_wire::inspect::Projection::Facts,
            }
            .into(),
            context: fux_wire::protocol::Context {
                of: None,
                cwd: std::path::PathBuf::new(),
                pane: None,
                server: None,
                client: None,
            },
        };
        let (client, mut theirs) = UnixStream::pair().map_err(e)?;
        let (server, link) = UnixStream::pair().map_err(e)?;
        let mut server = End::new(server).map_err(e)?;
        let link = End::new(link).map_err(e)?;
        let conn = std::thread::spawn(move || super::serve(client, link, Vec::new()));
        theirs.write_all(&hello.encode()).map_err(e)?;
        theirs.write_all(&asked.encode()).map_err(e)?;
        if shut {
            theirs.shutdown(std::net::Shutdown::Write).map_err(e)?;
        }
        let hello = said(&mut server)?;
        assert!(matches!(hello, FromConn::Frame { .. }), "hello");
        server.send(&ToConn::Expect(Expect::Asked));
        server.flush_all().map_err(|errno| errno.to_string())?;
        let asked = said(&mut server)?;
        assert!(matches!(asked, FromConn::Frame { .. }), "asked");
        Ok((theirs, server, conn))
    }

    /// What a connection's process says its server next, given long enough.
    #[cfg(unix)]
    fn said(server: &mut crate::link::End) -> Result<crate::link::FromConn, String> {
        use fuxix::poll::{Events, PollFd};
        let mut buffer = [0u8; 4096];
        loop {
            if let Some(said) = server.next_message().map_err(|e| e.to_string())? {
                return Ok(said);
            }
            let mut fds = [PollFd::new(&server.stream, Events::IN)];
            let ready = fuxix::poll::poll(&mut fds, Some(Duration::from_secs(20)));
            if ready == Ok(0) || server.read(&mut buffer) == crate::link::Read::Closed {
                return Err("its process said nothing".into());
            }
        }
    }

    /// A client gone while its request waits for its answer is told its
    /// server (`Shut`), whatever the request waits for there, whether it
    /// closed or only shut its side, and though it sent as much ahead as is
    /// held of it but a byte: so the wait ends with it.
    #[cfg(unix)]
    #[test]
    fn a_client_gone_while_its_request_waits_is_told_its_server() -> Result<(), String> {
        use crate::link::FromConn;
        use std::io::Write;
        let e = |error: std::io::Error| error.to_string();
        for (closes, ahead) in [(true, 0), (false, 0), (false, super::READ - 1)] {
            let (mut theirs, mut server, conn) = waiting()?;
            theirs.write_all(&vec![0u8; ahead]).map_err(e)?;
            if !closes {
                theirs.shutdown(std::net::Shutdown::Write).map_err(e)?;
            }
            // One that only shut its side still reads till its wait ended.
            let reading = (!closes).then_some(theirs);
            let said = said(&mut server);
            drop((server, reading));
            conn.join().map_err(|_| "its process panicked")??;
            assert!(
                matches!(said, Ok(FromConn::Shut)),
                "{said:?} (closes: {closes}, ahead: {ahead})"
            );
        }
        Ok(())
    }

    /// A client that sent its request and shut its side, as a script
    /// piping one in does, is not said gone while the answer is on its way
    /// (a pane's, say): only once it is written, which the client reads.
    #[cfg(unix)]
    #[test]
    fn a_client_that_shut_its_side_is_gone_only_once_answered() -> Result<(), String> {
        use crate::link::{FromConn, ToConn};
        use std::io::Read;
        // Its end was read with its request, before that was taken.
        let (mut theirs, mut server, conn) = asked(true)?;
        server.send(&ToConn::Bytes(b"the answer".to_vec()));
        server.send(&ToConn::Answered);
        server.flush_all().map_err(|errno| errno.to_string())?;
        let first = said(&mut server)?;
        assert!(matches!(first, FromConn::Flushed), "{first:?}");
        let then = said(&mut server)?;
        assert!(matches!(then, FromConn::Shut), "{then:?}");
        drop(server);
        conn.join().map_err(|_| "its process panicked")??;
        let mut got = Vec::new();
        theirs.read_to_end(&mut got).map_err(|e| e.to_string())?;
        // Then, its server gone with no last word, that it went.
        assert!(got.starts_with(b"the answer"), "{got:?}");
        Ok(())
    }

    /// A client that hung up with an answer on its way is not read, and is
    /// said gone all the same where its hang-up is told unasked.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_client_that_hung_up_before_its_answer_is_told_its_server() -> Result<(), String> {
        let (theirs, mut server, conn) = asked(false)?;
        drop(theirs);
        let said = said(&mut server);
        drop(server);
        conn.join().map_err(|_| "its process panicked")??;
        assert!(matches!(said, Ok(crate::link::FromConn::Shut)), "{said:?}");
        Ok(())
    }

    /// A client that sends ahead, while its request waits, as much as is
    /// held of it is refused (one whose answer is on its way is only read
    /// no more, `tests/servers.rs`): told why (an `Exit`), and its server told it
    /// is gone, which ends its wait and its process. No more of it is held
    /// than that.
    #[cfg(unix)]
    #[test]
    fn a_client_that_sends_too_much_ahead_is_refused_and_told_why() -> Result<(), String> {
        use crate::link::FromConn;
        use fux_wire::protocol::ToCommand;
        use std::io::{Read, Write};
        let e = |error: std::io::Error| error.to_string();
        let (mut theirs, mut server, conn) = waiting()?;
        // More than is held: what is over stays in its socket, unread.
        theirs
            .write_all(&vec![0u8; super::READ + 4096])
            .map_err(e)?;
        let said = said(&mut server);
        assert!(matches!(said, Ok(FromConn::Shut)), "{said:?}");
        drop(server);
        conn.join().map_err(|_| "its process panicked")??;
        // Its last word, before its socket's end (told as a reset, with
        // bytes of its own left unread there).
        let mut got = Vec::new();
        let mut buffer = [0u8; 4096];
        while let Ok(n @ 1..) = theirs.read(&mut buffer) {
            got.extend_from_slice(buffer.get(..n).unwrap_or_default());
        }
        let mut decoder = Decoder::default();
        decoder.push(&got);
        let last = decoder.frame::<ToCommand>().map_err(|e| e.to_string())?;
        assert!(matches!(last, Some(ToCommand::Exit(_))), "{last:?}");
        Ok(())
    }

    /// A screen it can't compose, refused memory, paints nothing: what the
    /// client shows stays, and its connection ends, saying why.
    #[cfg(feature = "checks")]
    #[test]
    fn a_refused_paint_paints_nothing() -> Result<(), String> {
        let (session, client) = crate::session::testing::attached(10, 40)?;
        let mut composer = Composer::new(Instant::now());
        composer.scene(crate::render::scene(&session, client).ok_or("a scene")?);
        let mut out = ByteQueue::default();
        fux_vt::refuse_next(1);
        let refused = composer.paint(&mut out, Instant::now());
        assert!(refused.is_err_and(|why| why.contains("rendering")));
        assert!(out.is_empty(), "painted");
        Ok(())
    }
}
