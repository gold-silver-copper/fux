//! A request's region: everything the server makes for one request, from
//! decoding its frame to its reply's bytes, is charged there before it is
//! made, and given back when the request is answered. The server answers
//! one request at a time, on its one thread, so its budget for requests
//! holds one request's region: a request kept past its answer shows as the
//! next one refused, out loud, never as memory.
//!
//! What a request leaves in the session (a pane's name, a launch, a clip)
//! is the session's from then on, bounded by its own limits.
fuxth_core::bounded_only!();

use fux_wire::protocol::{Asked, Frame, Reply};
use fux_wire::wire::{Budget, MAX_ALLOCATED};
use fuxth_core::{BoundedVec, Refused, Region};

/// What one request may hold at once: its decoded frame, what its answer
/// builds, and the answer's bytes, made in exactly their room. A styled
/// capture of a default history, 12,435 lines of 80 cells each in a style
/// of its own, holds 26,570,520 bytes at its peak: 13,540,703 built and
/// 13,035,337 on the wire. A reply is at most `MAX_REPLY` (32 MiB) on the
/// wire, and what it is built from about as large, so this admits any
/// reply the wire does, with room to spare.
pub const REQUEST_BYTES: usize = MAX_ALLOCATED;

/// What the waits under way hold between their request and their answer:
/// each watched pane's counts, 48 bytes, so about 87,000 watched panes in
/// all the waits at once. A wait's request is answered and its region given
/// back while it waits, so what it keeps can't be that region's.
pub const PENDING_BYTES: usize = 4 << 20;

/// The server's budget for requests: room for one request's region, and a
/// region the waits under way keep what they watch in.
pub struct Requests {
    budget: Region,
    pending: Region,
    each: usize,
}

impl Requests {
    /// Room for one request's region of `each` bytes, beside `pending`
    /// bytes for the waits under way.
    pub fn new(each: usize, pending: usize) -> Self {
        let whole = each
            .saturating_add(pending)
            .saturating_add(Region::OVERHEAD.saturating_mul(2));
        let budget = Region::new(whole);
        // Carved whole from a budget made with room for it.
        let pending = budget.child(pending).unwrap_or_else(|_| Region::new(0));
        Self {
            budget,
            pending,
            each,
        }
    }

    /// What the waits under way keep what they watch in, past their
    /// request's region.
    pub fn pending(&self) -> &Region {
        &self.pending
    }

    /// A request's budget, in a region carved whole from the server's
    /// before anything is made for it; refused while another is open.
    pub fn open(&self) -> Result<Budget, Refused> {
        self.budget
            .child(self.each)
            .map(|region| Budget::of(&region))
    }

    /// The bytes each request's region holds.
    pub fn each(&self) -> usize {
        self.each
    }
}

impl Default for Requests {
    fn default() -> Self {
        Self::new(REQUEST_BYTES, PENDING_BYTES)
    }
}

/// A request's frame, `kind` and `payload`, decoded in a region carved for
/// it from `requests`: the request, and its budget, which holds what it
/// made. One its region can't hold is refused, `too_long`, its reply to be
/// made once that region is given back; a frame that is no request is an
/// error.
pub fn decoded(
    requests: &Requests,
    kind: u8,
    payload: &[u8],
) -> Result<Result<(Asked, Budget), Reply>, fux_wire::protocol::Error> {
    let too_long = fux_wire::code::Code::from(fux_wire::code::Raise::TooLong);
    let mut budget = match requests.open() {
        Ok(budget) => budget,
        Err(why) => return Ok(Err(Reply::failed(too_long, refused(why)))),
    };
    match Asked::decode(kind, payload, &mut budget) {
        Ok(asked) => Ok(Ok((asked, budget))),
        Err(fux_wire::protocol::Error::Wire(fux_wire::wire::Error::Limit)) => {
            let each = requests.each();
            let why = format!("the request exceeds the {each} bytes a request may hold");
            Ok(Err(Reply::failed(too_long, why)))
        }
        Err(error) => Err(error),
    }
}

/// The most bytes one key's encoding takes: a CSI with at most five
/// decimal parameters of up to ten digits each, their separators and its
/// final byte (2 + 50 + 4 + 1); an SS3 or a legacy key's is shorter.
const KEY_ROOM: usize = 64;

/// `inputs` as a program whose screen is `screen` takes them: keys and
/// pastes encoded as it asked for them, text and bytes as they are. Room
/// for each is charged to `budget` before it is written, and a paste's
/// rewriting of its line ends beside it (`fux_vt::keys::encode::paste`:
/// two rewrites, each growing to at most twice its length, one beside the
/// other's growth).
pub fn input(
    screen: &fux_vt::Screen,
    inputs: &[fux_wire::command::Input],
    budget: &mut Budget,
) -> Result<std::vec::Vec<u8>, fux_wire::code::Error> {
    use fux_vt::keys::encode::{PASTE_END, PASTE_START, key_bytes, paste};
    use fux_wire::command::Input;
    use fuxix::Allocation;
    let mut bytes = budget.vector(0)?;
    for input in inputs {
        match input {
            Input::Key(key) => {
                budget.reserve(&mut bytes, KEY_ROOM)?;
                key_bytes((*key).into(), screen.key_mode(), &mut bytes);
            }
            Input::Text(text) => {
                budget.reserve(&mut bytes, text.len())?;
                bytes.extend_from_slice(text.as_bytes());
            }
            Input::Bytes(given) => {
                budget.reserve(&mut bytes, given.len())?;
                bytes.extend_from_slice(given);
            }
            Input::Paste(text) => {
                let marks = PASTE_START.len().saturating_add(PASTE_END.len());
                budget.reserve(&mut bytes, text.len().saturating_add(marks))?;
                let rewrites = match text.contains('\n') {
                    true => text.len().saturating_mul(5),
                    false => 0,
                };
                budget.charge(rewrites)?;
                paste(text, screen.mode(fux_vt::Mode::BracketedPaste), &mut bytes);
                budget.release(rewrites)?;
            }
        }
    }
    Ok(bytes)
}

/// The most bytes a cell of a composed grid takes (`fux_vt::Cells`: an
/// eight-byte cell and its attributes), and its hyperlink beside it.
const CELL: usize = std::mem::size_of::<(u64, fux_vt::Attributes)>();
const LINK: usize = std::mem::size_of::<Option<(fux_wire::id::PaneId, fux_vt::Hyperlink)>>();
/// The most bytes of short clusters a cell keeps (fux-vt's `SHORT`).
const SHORT: usize = 17;

/// What composing a client's screen, `rows` by `cols`, beside `panes`
/// panes, may hold, charged to `budget` before it is composed: its cells,
/// their text for long and short clusters (`Cells::text_limit`, 17 bytes a
/// cell), each part three times over as it grows beside its old room,
/// each cell's hyperlink, the list of rows with links as it grows, and
/// where each pane and its separator are placed, likewise.
pub fn composed(
    (rows, cols): (u16, u16),
    panes: usize,
    budget: &mut Budget,
) -> Result<(), fux_wire::code::Error> {
    let cells = usize::from(rows).saturating_mul(usize::from(cols));
    let text = fux_vt::Cells::text_limit(cells).saturating_add(cells.saturating_mul(SHORT));
    let row = std::mem::size_of::<Option<std::vec::Vec<u8>>>();
    let placed = std::mem::size_of::<(fux_wire::id::PaneId, fux_wire::layout::Rect)>()
        .saturating_add(std::mem::size_of::<fux_wire::layout::Separator>());
    let bytes = [
        cells.saturating_mul(CELL.saturating_add(LINK)),
        text.saturating_mul(3),
        usize::from(rows).saturating_mul(row).saturating_mul(3),
        panes.saturating_mul(placed).saturating_mul(3),
    ];
    let bytes = bytes.into_iter().fold(0, usize::saturating_add);
    fuxix::Allocation::charge(budget, bytes)
}

/// The room a reply's refusal is made in, of its own, as the request's
/// region may be full: a `too_long` failure and its message.
const REFUSAL_BYTES: usize = 1024;

/// What a reply past its cap or its region is answered: the bound's one
/// code, as a builder past the region says it (`Budget::exceeded`).
const TOO_LONG: &str = "finite reply exceeds its byte or allocation limit; request a smaller scope";

/// `reply`'s bytes on the wire, made in `budget`'s region; or, if they
/// would pass the reply's cap or the region, a `too_long` failure's, made
/// once the reply is dropped, in a small region of its own.
pub fn encoded(reply: Reply, budget: &Budget) -> Result<BoundedVec<u8>, fux_wire::wire::Error> {
    if let Ok(bytes) = fux_wire::wire::encode_in(&reply, budget.region()) {
        return Ok(bytes);
    }
    let run = reply.run;
    drop(reply);
    let too_long = fux_wire::code::Code::from(fux_wire::code::Raise::TooLong);
    let refusal = Reply {
        run,
        ..Reply::failed(too_long, TOO_LONG)
    };
    fux_wire::wire::encode_in(&refusal, &Region::new(REFUSAL_BYTES))
}

/// Why a request was refused its region: another request's is open. A
/// failure's message, made once (the one `std` store here).
pub fn refused(refused: Refused) -> std::string::String {
    format!("the server holds another request's region: {refused}")
}

#[cfg(test)]
mod tests {
    use super::{Requests, decoded, encoded};
    use crate::session::testing::{attached, run, started};
    use crate::session::{Begun, Caller, Origin, Session};
    use fux_wire::code::{Code, Raise};
    use fux_wire::command::{AnyRef, CaptureView, ClientAction, Command, EnvPatch, Input};
    use fux_wire::command::{Request, To};
    use fux_wire::id::{PaneId, TabId};
    use fux_wire::protocol::{Asked, Context, Frame, Response};
    use std::time::Instant;
    type TestResult = Result<(), std::boxed::Box<dyn std::error::Error>>;

    /// What a request holds past its region: the server's budget and its
    /// region's counters, and what is made beside the reply's values (its
    /// box, a failure's message).
    const FIXED: usize = 4096;

    /// A request's `frame`, as the server answers it: decoded in its
    /// region, begun, and its reply's bytes made there, or a refusal's in a
    /// region of their own (`Conn::reply`, `Conn::answer`).
    fn answer(
        session: &mut Session,
        frame: &[u8],
    ) -> Result<fuxth_core::BoundedVec<u8>, std::boxed::Box<dyn std::error::Error>> {
        let (kind, payload) = frame
            .get(4..)
            .and_then(<[u8]>::split_first)
            .ok_or("frame")?;
        let (asked, mut budget) = match decoded(&session.requests, *kind, payload)? {
            Ok(decoded) => decoded,
            Err(refused) => return Ok(encoded(refused, &session.requests.open()?)?),
        };
        let origin = match asked.context.client {
            Some(client) => Origin::Client(client),
            None => Origin::Cli(Caller::default()),
        };
        let Begun::Done(reply) = session.begin(asked.request, &origin, &mut budget, Instant::now())
        else {
            return Err("a request that waits".into());
        };
        Ok(encoded(reply, &budget)?)
    }

    /// `count` names, unset, as long as a request may give.
    fn names(count: usize) -> Result<EnvPatch, String> {
        let mut env = EnvPatch::default();
        for n in 0..count {
            env.push(format!("V{n:04}"), None)?;
        }
        Ok(env)
    }

    /// Each workload at a small region: its peak stays under the region and
    /// a fixed overhead, it is refused `too_long` when the region can't
    /// hold it, and once answered, or refused, the heap is where it started.
    /// - The default-history styled capture's shape, a line's every cell in
    ///   a style of its own.
    /// - One large request decoded (a send-keys of many inputs to no pane).
    /// - A send-keys to a live pane, and to every pane of a tab: their
    ///   encodings are made, and refused, before any pane is sent any.
    /// - An environment patch of 1,024 names, kept as decoded.
    /// - A client's screen captured: the grid it is composed in admitted
    ///   first.
    #[test]
    fn a_request_holds_no_more_than_its_region_and_a_fixed_overhead() -> TestResult {
        let allocator = fux_vt_peak::ThreadPeak;
        let mut session = started()?;
        let pane = PaneId::of(1);
        for line in 0..300 {
            let cells = (0..80).map(|cell| format!("\x1b[38;5;{}mx", (line + cell) % 256));
            let text = format!("{}\x1b[m\r\n", cells.collect::<std::string::String>());
            session.output(pane, text.as_bytes(), Instant::now());
        }
        let (mut tab, client) = attached(24, 80)?;
        for _ in 0..7 {
            run(&mut tab, "%1 str vertical axis split")?;
        }
        let capture = Command::CapturePane {
            target: pane,
            history: Some(usize::MAX),
            view: CaptureView {
                styles: true,
                ..CaptureView::default()
            },
        };
        let to = |target| To::Now(target);
        let keys = Command::SendKeys {
            to: to(AnyRef::Pane(PaneId::of(99))),
            inputs: std::iter::repeat_n(Input::Text("".into()), 190_000).collect(),
        };
        let text = |target, bytes| Command::SendKeys {
            to: to(target),
            inputs: vec![Input::Text(std::iter::repeat_n('x', bytes).collect())],
        };
        let live = text(AnyRef::Pane(pane), 40_000);
        let fan_out = text(AnyRef::Tab(TabId::of(1)), 8_000);
        let env = Command::Respawn {
            target: PaneId::of(99),
            kill: false,
            env: names(EnvPatch::MAX_NAMES)?,
            argv: None,
        };
        let screen = Command::Client {
            client: Some(client),
            action: ClientAction::Capture,
        };
        let too_long = Some(Code::from(Raise::TooLong));
        // A workload, the session it runs in, its region and its answer.
        for (command, on_tab, region, answered) in [
            (&capture, false, 1 << 20, None),
            (&capture, false, 512 << 10, too_long),
            (&keys, false, 8 << 20, Some(Code::NoPane)),
            (&keys, false, 1 << 20, too_long),
            (&live, false, 64 << 10, too_long),
            (&fan_out, true, 32 << 10, too_long),
            (&env, false, 64 << 10, Some(Code::NoPane)),
            (&screen, true, 512 << 10, None),
            (&screen, true, 256 << 10, too_long),
        ] {
            let session = if on_tab { &mut tab } else { &mut session };
            let asked = Asked {
                after: None,
                request: Request::Do(command.clone()),
                context: Context::default(),
            };
            // Its frame, as the connection holds it, read.
            let frame = asked.encode()?;
            session.requests = Requests::new(region, 0);
            // The first grows the buffers every settle reuses.
            drop(answer(session, &frame)?);
            let base = allocator.live();
            allocator.reset_peak();
            let bytes = answer(session, &frame)?;
            let peak = allocator.peak().saturating_sub(base);
            let reply = fux_wire::wire::decode::<fux_wire::protocol::Reply>(&bytes)?;
            let refused = match &reply.response {
                Response::Failed { code, .. } => Some(*code),
                Response::Returned(_) | Response::Answered(_) => None,
            };
            drop((bytes, reply));
            let after = allocator.live();
            let what = format!("{command:.40?} in {region}: {refused:?}, {peak} at the peak");
            assert_eq!(refused, answered, "{what}");
            assert!(peak <= region + FIXED, "{what}");
            // The region is used, not met by refusing everything: but a
            // screen's, which admits a bound, not what it holds (below).
            let screen = matches!(command, Command::Client { .. });
            assert!(refused.is_some() || screen || peak >= region / 8, "{what}");
            assert_eq!(after, base, "{what}: the heap after");
            println!("{what}");
        }
        // The keys refused above were sent to no pane; in a region that
        // holds them, every pane is sent them.
        let queued =
            |s: &Session| -> usize { (s.parts.panes().values()).map(|p| p.input().len()).sum() };
        assert_eq!((queued(&session), queued(&tab)), (0, 0));
        for (command, session, sent) in
            [(&live, &mut session, 40_000), (&fan_out, &mut tab, 64_000)]
        {
            session.requests = Requests::default();
            let asked = Asked {
                after: None,
                request: Request::Do(command.clone()),
                context: Context::default(),
            };
            drop(answer(session, &asked.encode()?)?);
            assert_eq!(queued(session), sent, "{command:.40?}");
        }
        Ok(())
    }

    /// A client's screen of long clusters, each in a hyperlink of its own,
    /// composed holds no more than `composed` admits for it.
    #[test]
    fn a_composed_screen_holds_no_more_than_it_is_admitted() -> TestResult {
        let allocator = fux_vt_peak::ThreadPeak;
        let (mut session, client) = attached(24, 80)?;
        run(&mut session, "%1 str vertical axis split")?;
        let marks: std::string::String = std::iter::repeat_n('\u{301}', 60).collect();
        let cluster = format!("x{marks}");
        for pane in [PaneId::of(1), PaneId::of(2)] {
            for n in 0..24 * 80 {
                let cell = format!("\x1b]8;;https://example.com/{n}\x07{cluster}\x1b]8;;\x07");
                session.output(pane, cell.as_bytes(), Instant::now());
            }
        }
        let view = session.views.get(&client).ok_or("client")?;
        let budget = &mut fux_wire::wire::Budget::with_limit(usize::MAX);
        super::composed((view.rows, view.cols), session.parts.panes().len(), budget)?;
        let admitted = usize::MAX - budget.region().remaining();
        let base = allocator.live();
        allocator.reset_peak();
        let mut grid = crate::render::Grid::default();
        let mut placement = fux_wire::layout::Placement::default();
        crate::render::compose_view(&session, view, &mut grid, &mut placement)?;
        let peak = allocator.peak().saturating_sub(base);
        let long = grid.cells.text_len();
        drop((grid, placement));
        println!("composed {peak} of {admitted} admitted, {long} bytes of long clusters");
        assert!(long > 24 * 80 * 16, "the clusters are long: {long}");
        assert!(peak <= admitted, "{peak} > {admitted}");
        Ok(())
    }

    /// A wait on a tab keeps each of its panes' counts in the waits' own
    /// region, admitted before they are collected, till the wait ends: a
    /// wait it has no room for is refused, and the room comes back as the
    /// wait ends, though its request's region went back as it began.
    #[test]
    fn a_waits_watched_panes_are_held_in_the_waits_region_till_it_ends() -> TestResult {
        let (mut tab, _) = attached(24, 80)?;
        for _ in 0..7 {
            run(&mut tab, "%1 str vertical axis split")?;
        }
        let entry = std::mem::size_of::<(PaneId, fux_wire::command::Counts)>();
        let wait = || -> Result<Request, &str> {
            Ok(Request::Wait(fux_wire::command::Wait {
                since: fux_wire::command::Since::Now(AnyRef::Tab(TabId::of(1))),
                timeout: fux_wire::command::Limit::secs(60).ok_or("limit")?,
            }))
        };
        let origin = Origin::Cli(Caller::default());
        for (room, waits) in [(8 * entry - 1, false), (8 * entry, true)] {
            tab.requests = Requests::new(1 << 20, room);
            let mut budget = tab.requests.open()?;
            let begun = tab.begin(wait()?, &origin, &mut budget, Instant::now());
            drop(budget);
            match (begun, waits) {
                (Begun::Waiting(waiting), true) => {
                    assert_eq!(tab.requests.pending().remaining(), room - 8 * entry);
                    drop(waiting);
                    assert_eq!(tab.requests.pending().remaining(), room);
                }
                (Begun::Done(reply), false) => {
                    let Response::Failed { code, .. } = reply.response else {
                        return Err(format!("{reply:?}").into());
                    };
                    assert_eq!(code, Code::from(Raise::TooLong));
                    assert_eq!(tab.requests.pending().remaining(), room);
                }
                (Begun::Waiting(_), false) => return Err("waited without room".into()),
                (Begun::Done(reply), true) => return Err(format!("{reply:?}").into()),
            }
        }
        Ok(())
    }
}
